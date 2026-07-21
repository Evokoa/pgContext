#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_upgrade_matrix}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

CURRENT_VERSION="$(sed -n "s/^default_version = '\([^']*\)'/\1/p" "${REPO_ROOT}/crates/context-pg/pgcontext.control")"
if [[ -z "${CURRENT_VERSION}" ]]; then
    echo "could not determine current extension version" >&2
    exit 2
fi
ROLLBACK_PROBE_VERSION="${CURRENT_VERSION}-rollback-probe"
ROLLBACK_PROBE_TABLE="_rollback_probe_should_not_persist"
ROLLBACK_PROBE_SCRIPT=""
ROLLBACK_PROBE_SCRIPT_CREATED=0
PREVIOUS_INSTALL_SQL_STAGED=()

cleanup_rollback_probe_script() {
    if [[ -n "${ROLLBACK_PROBE_SCRIPT}" && "${ROLLBACK_PROBE_SCRIPT_CREATED}" -eq 1 ]]; then
        rm -f -- "${ROLLBACK_PROBE_SCRIPT}"
    fi
}

cleanup_previous_install_sql() {
    local staged_sql
    for staged_sql in "${PREVIOUS_INSTALL_SQL_STAGED[@]}"; do
        rm -f -- "${staged_sql}"
    done
}

cleanup_extension_scripts() {
    cleanup_rollback_probe_script
    cleanup_previous_install_sql
}
trap cleanup_extension_scripts EXIT

start_and_install_extension

INSTALL_VERSIONS=()
while IFS= read -r version; do
    INSTALL_VERSIONS+=("${version}")
done < <(
    find "${REPO_ROOT}/sql" -maxdepth 1 -type f -name 'pgcontext--*.sql' ! -name 'pgcontext--*--*.sql' \
        | sed -n 's/.*pgcontext--\(.*\)\.sql$/\1/p' \
        | sort
)

stage_previous_install_sql_versions() {
    local extension_dir
    local version
    local source_sql
    local destination_sql

    extension_dir="$("${PG_CONFIG}" --sharedir)/extension"
    if [[ ! -d "${extension_dir}" ]]; then
        echo "extension directory does not exist: ${extension_dir}" >&2
        exit 2
    fi
    if [[ ! -w "${extension_dir}" ]]; then
        echo "extension directory is not writable for previous install SQL: ${extension_dir}" >&2
        exit 2
    fi

    for version in "$@"; do
        source_sql="${REPO_ROOT}/sql/pgcontext--${version}.sql"
        destination_sql="${extension_dir}/pgcontext--${version}.sql"

        if [[ ! -f "${source_sql}" || -L "${source_sql}" ]]; then
            echo "previous install SQL is missing or is a symlink: ${source_sql}" >&2
            exit 2
        fi

        if [[ -e "${destination_sql}" ]]; then
            if cmp -s "${source_sql}" "${destination_sql}"; then
                continue
            fi
            echo "previous install SQL already exists with different contents: ${destination_sql}" >&2
            exit 2
        fi

        PREVIOUS_INSTALL_SQL_STAGED+=("${destination_sql}")
        cp "${source_sql}" "${destination_sql}"
        chmod 0644 "${destination_sql}"
    done
}

validate_lifecycle_state() {
    psql_db <<'SQL'
DO $$
DECLARE
    preexisting_rows bigint;
    preexisting_indexes bigint;
    catalog_privileges bigint;
    sequence_privileges bigint;
    missing_visibility_views bigint;
    missing_function_execute bigint;
BEGIN
    SELECT count(*) INTO preexisting_rows FROM public.preexisting_source_table;
    IF preexisting_rows <> 2 THEN
        RAISE EXCEPTION 'extension install/update mutated preexisting source rows: %', preexisting_rows;
    END IF;

    SELECT count(*)
      INTO preexisting_indexes
      FROM pg_catalog.pg_index
      JOIN pg_catalog.pg_class index_rel ON index_rel.oid = pg_index.indexrelid
      JOIN pg_catalog.pg_class table_rel ON table_rel.oid = pg_index.indrelid
      JOIN pg_catalog.pg_namespace table_ns ON table_ns.oid = table_rel.relnamespace
     WHERE table_ns.nspname = 'public'
       AND table_rel.relname = 'preexisting_source_table'
       AND index_rel.relname <> 'preexisting_source_table_pkey';
    IF preexisting_indexes <> 0 THEN
        RAISE EXCEPTION 'extension install/update built unexpected indexes on user source table';
    END IF;

    IF pg_catalog.has_schema_privilege('m1_upgrade_priv_probe', 'pgcontext', 'USAGE') THEN
        RAISE EXCEPTION 'fresh role unexpectedly has pgcontext schema USAGE';
    END IF;

    IF NOT pg_catalog.has_type_privilege('m1_upgrade_priv_probe', 'vector', 'USAGE') THEN
        RAISE EXCEPTION 'fresh role unexpectedly lacks vector type USAGE';
    END IF;

    SELECT count(*)
      INTO missing_function_execute
      FROM pg_catalog.pg_proc
      JOIN pg_catalog.pg_namespace ON pg_namespace.oid = pg_proc.pronamespace
     WHERE pg_namespace.nspname = 'pgcontext'
       AND NOT pg_catalog.has_function_privilege('m1_upgrade_priv_probe', pg_proc.oid, 'EXECUTE');
    IF missing_function_execute <> 0 THEN
        RAISE EXCEPTION 'fresh role lacks EXECUTE on % pgcontext functions', missing_function_execute;
    END IF;

    SELECT count(*)
      INTO catalog_privileges
      FROM pg_catalog.pg_class
      JOIN pg_catalog.pg_namespace ON pg_namespace.oid = pg_class.relnamespace
     CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER']) AS requested(privilege)
     WHERE pg_namespace.nspname = 'pgcontext'
       AND relkind = 'r'
       AND pg_catalog.has_table_privilege('m1_upgrade_priv_probe', pg_class.oid, requested.privilege);
    IF catalog_privileges <> 0 THEN
        RAISE EXCEPTION 'fresh role has % privileges on extension catalog tables', catalog_privileges;
    END IF;

    SELECT count(*)
      INTO sequence_privileges
      FROM pg_catalog.pg_class
      JOIN pg_catalog.pg_namespace ON pg_namespace.oid = pg_class.relnamespace
     CROSS JOIN unnest(ARRAY['SELECT', 'UPDATE', 'USAGE']) AS requested(privilege)
     WHERE pg_namespace.nspname = 'pgcontext'
       AND relkind = 'S'
       AND pg_catalog.has_sequence_privilege('m1_upgrade_priv_probe', pg_class.oid, requested.privilege);
    IF sequence_privileges <> 0 THEN
        RAISE EXCEPTION 'fresh role has % privileges on extension catalog sequences', sequence_privileges;
    END IF;

    SELECT count(*)
      INTO missing_visibility_views
      FROM pg_catalog.pg_class
      JOIN pg_catalog.pg_namespace ON pg_namespace.oid = pg_class.relnamespace
     WHERE pg_namespace.nspname = 'pgcontext'
       AND relkind = 'v'
       AND relname IN (
           '_collection_acl',
           '_visible_collection_vectors',
           '_visible_collection_sparse_vectors',
           '_visible_collection_points',
           '_visible_collection_payload_columns'
       )
       AND NOT pg_catalog.has_table_privilege('m1_upgrade_priv_probe', pg_class.oid, 'SELECT');
    IF missing_visibility_views <> 0 THEN
        RAISE EXCEPTION '% ACL-filtered visibility views are missing SELECT', missing_visibility_views;
    END IF;
END
$$;
SQL
}

load_representative_state() {
    psql_db <<'SQL'
CREATE TABLE public.docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL,
    tenant text NOT NULL
);

INSERT INTO public.docs (id, embedding, body, tenant)
VALUES
    (1, '[0,0]'::vector, 'database internals', 'acme'),
    (2, '[1,0]'::vector, 'query planning', 'acme'),
    (3, '[5,5]'::vector, 'gardening notes', 'other');

SELECT * FROM pgcontext.create_collection('upgrade_docs', 'public.docs');
SELECT * FROM pgcontext.register_vector('upgrade_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.register_filter_column('upgrade_docs', 'tenant', 'tenant');
SELECT * FROM pgcontext.upsert_points('upgrade_docs', ARRAY['1', '2', '3']);
SQL
}

validate_representative_behavior() {
    psql_db <<'SQL'
DO $$
DECLARE
    nearest_source_key text;
    filtered_count bigint;
BEGIN
    SELECT source_key
      INTO nearest_source_key
      FROM pgcontext.search('upgrade_docs', '[0,0]'::vector, 1);
    IF nearest_source_key <> '1' THEN
        RAISE EXCEPTION 'unexpected upgraded nearest source key: %', nearest_source_key;
    END IF;

    SELECT count(*)
      INTO filtered_count
      FROM pgcontext.search(
          'upgrade_docs',
          '[0,0]'::vector,
          '{"must":[{"key":"tenant","match":"acme"}]}',
          10
      );
    IF filtered_count <> 2 THEN
        RAISE EXCEPTION 'unexpected upgraded filtered result count: %', filtered_count;
    END IF;
END
$$;
SQL
}

validate_vector_metadata_compatibility() {
    psql_db <<'SQL'
ALTER TABLE public.docs ADD COLUMN IF NOT EXISTS sparse_terms sparsevec;
UPDATE public.docs
   SET sparse_terms = CASE id
       WHEN 1 THEN pgcontext.sparsevec('{1:0.9,3:0.1}/16')
       WHEN 2 THEN pgcontext.sparsevec('{2:0.8,4:0.2}/16')
       ELSE pgcontext.sparsevec('{8:1}/16')
   END;

SELECT * FROM pgcontext.configure_vector(
    'upgrade_docs',
    'embedding',
    '{"m":16,"ef_search":64}'::jsonb,
    '{"metadata_version":1,"mode":"scalar","levels":256}'::jsonb,
    'ready'
);

DO $$
DECLARE
    dense_options jsonb;
    sparse_options jsonb;
    rejected_sqlstate text;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext.collection_sparse_vectors('upgrade_docs')
         WHERE vector_name = 'lexical'
    ) THEN
        PERFORM *
          FROM pgcontext.register_sparse_vector(
              'upgrade_docs',
              'lexical',
              'sparse_terms',
              16,
              'inner_product'
          );
    END IF;

    PERFORM *
      FROM pgcontext.configure_sparse_vector(
          'upgrade_docs',
          'lexical',
          '{"format":"source_table_column"}'::jsonb,
          '{"strategy":"exact"}'::jsonb,
          'ready'
      );

    SELECT quantization_options
      INTO dense_options
      FROM pgcontext.collection_vectors('upgrade_docs')
     WHERE vector_name = 'embedding';
    IF dense_options <> '{"metadata_version": 1, "mode": "scalar", "levels": 256}'::jsonb THEN
        RAISE EXCEPTION 'unexpected upgraded dense quantization metadata: %', dense_options;
    END IF;

    SELECT storage_options
      INTO sparse_options
      FROM pgcontext.collection_sparse_vectors('upgrade_docs')
     WHERE vector_name = 'lexical';
    IF sparse_options <> '{"format": "source_table_column"}'::jsonb THEN
        RAISE EXCEPTION 'unexpected upgraded sparse storage metadata: %', sparse_options;
    END IF;

    BEGIN
        PERFORM *
          FROM pgcontext.configure_vector(
              'upgrade_docs',
              'embedding',
              '{}'::jsonb,
              '{"metadata_version":999}'::jsonb,
              'ready'
          );
        RAISE EXCEPTION 'expected future quantization metadata rejection';
    EXCEPTION WHEN OTHERS THEN
        GET STACKED DIAGNOSTICS rejected_sqlstate = RETURNED_SQLSTATE;
        IF rejected_sqlstate <> '22023' THEN
            RAISE EXCEPTION 'unexpected future quantization metadata SQLSTATE: %', rejected_sqlstate;
        END IF;
    END;
END
$$;
SQL
}

install_failing_update_rollback_probe() {
    local extension_dir
    extension_dir="$("${PG_CONFIG}" --sharedir)/extension"
    ROLLBACK_PROBE_SCRIPT="${extension_dir}/pgcontext--${CURRENT_VERSION}--${ROLLBACK_PROBE_VERSION}.sql"

    if [[ -e "${ROLLBACK_PROBE_SCRIPT}" ]]; then
        echo "rollback probe script already exists: ${ROLLBACK_PROBE_SCRIPT}" >&2
        exit 2
    fi
    if [[ ! -w "${extension_dir}" ]]; then
        echo "extension directory is not writable for rollback probe: ${extension_dir}" >&2
        exit 2
    fi

    cat >"${ROLLBACK_PROBE_SCRIPT}" <<SQL
CREATE TABLE pgcontext.${ROLLBACK_PROBE_TABLE} (id integer);
DO \$\$
BEGIN
    RAISE EXCEPTION 'rollback probe forced failure after extension-owned catalog mutation';
END
\$\$;
SQL
    ROLLBACK_PROBE_SCRIPT_CREATED=1
}

validate_failed_update_rollback_path() {
    if [[ "${ROLLBACK_PROBE_SCRIPT_CREATED}" -eq 0 ]]; then
        install_failing_update_rollback_probe
    fi

    if psql_db <<SQL
BEGIN;
ALTER EXTENSION pgcontext UPDATE TO '${ROLLBACK_PROBE_VERSION}';
COMMIT;
SQL
    then
        echo "unexpectedly updated pgcontext to rollback probe version" >&2
        exit 1
    fi

    validate_lifecycle_state
    validate_representative_behavior
    psql_db <<SQL
DO \$\$
DECLARE
    installed_version text;
    probe_table_exists boolean;
BEGIN
    SELECT extversion INTO installed_version
      FROM pg_catalog.pg_extension
     WHERE extname = 'pgcontext';
    IF installed_version <> '${CURRENT_VERSION}' THEN
        RAISE EXCEPTION 'failed update rollback left pgcontext at %, expected ${CURRENT_VERSION}', installed_version;
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_class
          JOIN pg_catalog.pg_namespace ON pg_namespace.oid = pg_class.relnamespace
         WHERE pg_namespace.nspname = 'pgcontext'
           AND pg_class.relname = '${ROLLBACK_PROBE_TABLE}'
    ) INTO probe_table_exists;
    IF probe_table_exists THEN
        RAISE EXCEPTION 'failed update rollback left probe table behind';
    END IF;
END
\$\$;
SQL
    echo "rollback_path_exercised: failed_update_probe -> current_catalog_validated"
}

reset_database

psql_db <<SQL
CREATE TABLE public.preexisting_source_table (
    id bigint PRIMARY KEY,
    body text NOT NULL
);
INSERT INTO public.preexisting_source_table (id, body)
VALUES (1, 'before install'), (2, 'still user owned');
DROP ROLE IF EXISTS m1_upgrade_priv_probe;
CREATE ROLE m1_upgrade_priv_probe;
CREATE EXTENSION pgcontext VERSION '${CURRENT_VERSION}';
SQL

validate_lifecycle_state
load_representative_state
validate_representative_behavior
validate_vector_metadata_compatibility
validate_failed_update_rollback_path

previous_versions=()
for version in "${INSTALL_VERSIONS[@]}"; do
    if [[ "${version}" != "${CURRENT_VERSION}" ]]; then
        previous_versions+=("${version}")
    fi
done

if [[ "${#previous_versions[@]}" -eq 0 ]]; then
    echo "No previous pgcontext SQL versions are present; current-version lifecycle checks passed."
    exit 0
fi

stage_previous_install_sql_versions "${previous_versions[@]}"

for version in "${previous_versions[@]}"; do
    reset_database
    psql_db <<SQL
CREATE TABLE public.preexisting_source_table (
    id bigint PRIMARY KEY,
    body text NOT NULL
);
INSERT INTO public.preexisting_source_table (id, body)
VALUES (1, 'before install'), (2, 'still user owned');
DROP ROLE IF EXISTS m1_upgrade_priv_probe;
CREATE ROLE m1_upgrade_priv_probe;
CREATE EXTENSION pgcontext VERSION '${version}';
SQL

    validate_lifecycle_state
    load_representative_state

    psql_db -c "ALTER EXTENSION pgcontext UPDATE TO '${CURRENT_VERSION}'"
    echo "upgrade_path_exercised: ${version} -> ${CURRENT_VERSION}"

    validate_lifecycle_state
    validate_representative_behavior
    validate_vector_metadata_compatibility
    validate_failed_update_rollback_path
done
