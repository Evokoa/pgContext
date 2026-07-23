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
CONTROL_RESTORE_REQUIRED=0
PGVECTOR_STUB_STAGED=0

restore_current_control() {
    if [[ "${CONTROL_RESTORE_REQUIRED}" -eq 0 ]]; then
        return
    fi
    cp "${REPO_ROOT}/crates/context-pg/pgcontext.control" \
        "$("${PG_CONFIG}" --sharedir)/extension/pgcontext.control"
    CONTROL_RESTORE_REQUIRED=0
}

stage_previous_control() {
    local version="$1"
    local previous_control="${REPO_ROOT}/sql/pgcontext--${version}.control"
    if [[ ! -f "${previous_control}" || -L "${previous_control}" ]]; then
        echo "previous control fixture is missing or is a symlink: ${previous_control}" >&2
        exit 2
    fi
    cp "${previous_control}" "$("${PG_CONFIG}" --sharedir)/extension/pgcontext.control"
    CONTROL_RESTORE_REQUIRED=1
}

cleanup_rollback_probe_script() {
    if [[ -n "${ROLLBACK_PROBE_SCRIPT}" && "${ROLLBACK_PROBE_SCRIPT_CREATED}" -eq 1 ]]; then
        rm -f -- "${ROLLBACK_PROBE_SCRIPT}"
    fi
}

cleanup_previous_install_sql() {
    local staged_sql
    if [[ "${#PREVIOUS_INSTALL_SQL_STAGED[@]}" -eq 0 ]]; then
        return
    fi
    for staged_sql in "${PREVIOUS_INSTALL_SQL_STAGED[@]}"; do
        rm -f -- "${staged_sql}"
    done
}

cleanup_extension_scripts() {
    restore_current_control
    cleanup_rollback_probe_script
    cleanup_previous_install_sql
    if [[ "${PGVECTOR_STUB_STAGED}" -eq 1 ]]; then
        rm -f -- "$("${PG_CONFIG}" --sharedir)/extension/vector.control" \
            "$("${PG_CONFIG}" --sharedir)/extension/vector--0.8.5.sql"
    fi
}
trap cleanup_extension_scripts EXIT

start_and_install_extension

stage_pgvector_stub_if_needed() {
    local extension_dir
    extension_dir="$("${PG_CONFIG}" --sharedir)/extension"
    if [[ -f "${extension_dir}/vector.control" ]]; then
        return
    fi
    if [[ -e "${extension_dir}/vector--0.8.5.sql" ]]; then
        echo "refusing to overwrite existing pgvector SQL without vector.control: ${extension_dir}/vector--0.8.5.sql" >&2
        exit 2
    fi
    cp "${REPO_ROOT}/tests/fixtures/pgvector_stub/vector.control" \
        "${extension_dir}/vector.control"
    cp "${REPO_ROOT}/tests/fixtures/pgvector_stub/vector--0.8.5.sql" \
        "${extension_dir}/vector--0.8.5.sql"
    chmod 0644 "${extension_dir}/vector.control" "${extension_dir}/vector--0.8.5.sql"
    PGVECTOR_STUB_STAGED=1
}

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
    local update_sql
    local destination_update_sql

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
            if ! cmp -s "${source_sql}" "${destination_sql}"; then
                echo "previous install SQL already exists with different contents: ${destination_sql}" >&2
                exit 2
            fi
        else
            PREVIOUS_INSTALL_SQL_STAGED+=("${destination_sql}")
            cp "${source_sql}" "${destination_sql}"
            chmod 0644 "${destination_sql}"
        fi

        update_sql="${REPO_ROOT}/sql/pgcontext--${version}--${CURRENT_VERSION}.sql"
        destination_update_sql="${extension_dir}/pgcontext--${version}--${CURRENT_VERSION}.sql"
        if [[ ! -f "${update_sql}" || -L "${update_sql}" ]]; then
            echo "extension update SQL is missing or is a symlink: ${update_sql}" >&2
            exit 2
        fi
        if [[ -e "${destination_update_sql}" ]]; then
            if ! cmp -s "${update_sql}" "${destination_update_sql}"; then
                echo "extension update SQL already exists with different contents: ${destination_update_sql}" >&2
                exit 2
            fi
        else
            PREVIOUS_INSTALL_SQL_STAGED+=("${destination_update_sql}")
            cp "${update_sql}" "${destination_update_sql}"
            chmod 0644 "${destination_update_sql}"
        fi
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
           '_visible_artifact_segments',
           '_visible_build_jobs',
           '_visible_collection_late_interaction',
           '_visible_collection_limits',
           '_visible_collection_payload_columns',
           '_visible_collection_points',
           '_visible_collection_sparse_vectors',
           '_visible_collection_vectors',
           '_visible_collections',
           '_visible_pgvector_ownership_conversions',
           '_visible_query_stats'
       )
       AND NOT pg_catalog.has_table_privilege('m1_upgrade_priv_probe', pg_class.oid, 'SELECT');
    IF missing_visibility_views <> 0 THEN
        RAISE EXCEPTION '% ACL-filtered visibility views are missing SELECT', missing_visibility_views;
    END IF;
END
$$;
SQL
}

validate_visibility_barriers() {
    psql_db <<'SQL'
DO $$
DECLARE
    membership_filtered_views bigint;
    unbarriered_views text[];
BEGIN
    SELECT count(*),
           pg_catalog.array_agg(relname::text ORDER BY relname)
               FILTER (
                   WHERE NOT COALESCE(
                       reloptions @> ARRAY['security_barrier=true']::text[],
                       false
                   )
               )
      INTO membership_filtered_views, unbarriered_views
      FROM pg_catalog.pg_class
      JOIN pg_catalog.pg_namespace
        ON pg_namespace.oid = pg_class.relnamespace
     WHERE pg_namespace.nspname = 'pgcontext'
       AND relkind = 'v'
       AND pg_catalog.pg_get_viewdef(pg_class.oid)
           ILIKE '%pg_has_role(SESSION_USER,%';

    IF membership_filtered_views <> 11 THEN
        RAISE EXCEPTION 'expected 11 membership-filtered visibility views, found %',
                        membership_filtered_views;
    END IF;
    IF unbarriered_views IS NOT NULL THEN
        RAISE EXCEPTION 'membership-filtered views lack security barriers: %',
                        unbarriered_views;
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

load_representative_legacy_state() {
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

WITH inserted AS (
    INSERT INTO pgcontext._collections (
        collection_name,
        owner_role,
        source_table_oid,
        source_schema_name,
        source_table_name
    )
    VALUES (
        'upgrade_docs',
        CURRENT_USER::regrole::oid,
        'public.docs'::regclass,
        'public',
        'docs'
    )
    RETURNING collection_id
)
INSERT INTO pgcontext._collection_vectors (
    collection_id,
    vector_name,
    source_table_oid,
    source_schema_name,
    source_table_name,
    vector_column_name,
    vector_attnum,
    dimensions,
    metric
)
SELECT collection_id,
       'embedding',
       'public.docs'::regclass,
       'public',
       'docs',
       'embedding',
       (
           SELECT attnum FROM pg_attribute
            WHERE attrelid = 'public.docs'::regclass
              AND attname = 'embedding'
       ),
       2,
       'l2'
  FROM inserted;

INSERT INTO pgcontext._collection_payload_columns (
    collection_id,
    filter_key,
    source_table_oid,
    source_schema_name,
    source_table_name,
    column_name,
    column_attnum
)
SELECT collection_id,
       'tenant',
       'public.docs'::regclass,
       'public',
       'docs',
       'tenant',
       (
           SELECT attnum FROM pg_attribute
            WHERE attrelid = 'public.docs'::regclass
              AND attname = 'tenant'
       )
  FROM pgcontext._collections
 WHERE collection_name = 'upgrade_docs';

INSERT INTO pgcontext._collection_points (collection_id, source_key)
SELECT collection_id, source_key
  FROM pgcontext._collections
 CROSS JOIN unnest(ARRAY['1', '2', '3']) AS source_key
 WHERE collection_name = 'upgrade_docs';

INSERT INTO pgcontext._query_stats (
    collection_id,
    cohort,
    query_kind,
    result_count,
    candidate_count,
    latency_ms
)
SELECT collection_id,
       'automatic',
       'search',
       777,
       888,
       9.5
  FROM pgcontext._collections
 WHERE collection_name = 'upgrade_docs';
SQL
}

validate_legacy_automatic_reclassification() {
    psql_db <<'SQL'
DO $$
DECLARE
    legacy_rows bigint;
    exposed_as_automatic bigint;
BEGIN
    SELECT count(*)
      INTO legacy_rows
      FROM pgcontext._query_stats
     WHERE cohort = 'legacy_automatic'
       AND result_count = 777
       AND candidate_count = 888
       AND strategy = 'unspecified'
       AND completion = 'unspecified';
    IF legacy_rows <> 1 THEN
        RAISE EXCEPTION 'released manual automatic cohort was not reclassified exactly once';
    END IF;

    SELECT coalesce(sum(query_count), 0)
      INTO exposed_as_automatic
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'upgrade_docs';
    IF exposed_as_automatic <> 0 THEN
        RAISE EXCEPTION 'legacy manual row leaked into internal automatic observations';
    END IF;
END
$$;
SQL
}

validate_upgrade_catalog_transition() {
    psql_db <<'SQL'
DO $$
DECLARE
    extension_namespace text;
    dependency_namespace text;
    schema_is_extension_member boolean;
    support_functions bigint;
    public_support_functions bigint;
    serving_columns integer;
    migration_columns integer;
    mmap_security_definer boolean;
BEGIN
    SELECT namespace.nspname
      INTO extension_namespace
      FROM pg_catalog.pg_extension AS extension
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = extension.extnamespace
     WHERE extension.extname = 'pgcontext';
    IF extension_namespace <> 'pgcontext' THEN
        RAISE EXCEPTION 'upgraded extension namespace is %, expected pgcontext', extension_namespace;
    END IF;

    SELECT namespace.nspname
      INTO dependency_namespace
      FROM pg_catalog.pg_extension AS extension
      JOIN pg_catalog.pg_depend AS dependency
        ON dependency.classid = 'pg_catalog.pg_extension'::pg_catalog.regclass
       AND dependency.objid = extension.oid
       AND dependency.refclassid = 'pg_catalog.pg_namespace'::pg_catalog.regclass
       AND dependency.deptype = 'n'
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = dependency.refobjid
     WHERE extension.extname = 'pgcontext';
    IF dependency_namespace <> extension_namespace THEN
        RAISE EXCEPTION 'extension namespace dependency is %, expected %', dependency_namespace, extension_namespace;
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_namespace AS namespace
          JOIN pg_catalog.pg_depend AS dependency
            ON dependency.classid = 'pg_catalog.pg_namespace'::pg_catalog.regclass
           AND dependency.objid = namespace.oid
           AND dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
           AND dependency.deptype = 'e'
          JOIN pg_catalog.pg_extension AS extension ON extension.oid = dependency.refobjid
         WHERE namespace.nspname = 'pgcontext' AND extension.extname = 'pgcontext'
    ) INTO schema_is_extension_member;
    IF schema_is_extension_member THEN
        RAISE EXCEPTION 'pgcontext schema remained an extension member after upgrade';
    END IF;

    SELECT count(*) FILTER (WHERE namespace.nspname = 'pgcontext'),
           count(*) FILTER (WHERE namespace.nspname = 'public')
      INTO support_functions, public_support_functions
      FROM pg_catalog.pg_proc AS procedure
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = procedure.pronamespace
     WHERE procedure.proname IN (
         'vector_in', 'vector_out', 'halfvec_in', 'halfvec_out',
         'sparsevec_in', 'sparsevec_out', 'bitvec_in', 'bitvec_out'
     );
    IF support_functions <> 8 OR public_support_functions <> 0 THEN
        RAISE EXCEPTION 'type support function schema parity failed: pgcontext %, public %',
                        support_functions, public_support_functions;
    END IF;

    SELECT cardinality(proallargtypes)
      INTO serving_columns
      FROM pg_catalog.pg_proc
     WHERE oid = 'pgcontext.hnsw_serving_stats()'::pg_catalog.regprocedure;
    SELECT cardinality(proallargtypes)
      INTO migration_columns
      FROM pg_catalog.pg_proc
     WHERE oid = 'pgcontext.migration_report()'::pg_catalog.regprocedure;
    SELECT prosecdef
      INTO mmap_security_definer
      FROM pg_catalog.pg_proc
     WHERE oid = 'pgcontext.build_mmap_hnsw_artifact(bigint)'::pg_catalog.regprocedure;
    IF serving_columns <> 14 OR migration_columns <> 10 OR mmap_security_definer THEN
        RAISE EXCEPTION 'retained C function ABI/ACL transition is stale: serving %, migration %, definer %',
                        serving_columns, migration_columns, mmap_security_definer;
    END IF;

    PERFORM * FROM pgcontext.hnsw_serving_stats();
    PERFORM * FROM pgcontext.migration_report();
END
$$;
SQL
    local default_path_types
    default_path_types="$(
        PGOPTIONS='-c search_path="$user",public' \
            psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" \
            -v ON_ERROR_STOP=1 -Atc \
            "SELECT (pg_catalog.to_regtype('vector') IS NULL)::text || '|' || (pg_catalog.to_regtype('pgcontext.vector') IS NOT NULL)::text"
    )"
    if [[ "${default_path_types}" != "true|true" ]]; then
        echo "upgraded vector type qualification contract failed: ${default_path_types}" >&2
        exit 1
    fi
    echo "upgrade_default_search_path_requires_qualified_vector"
}

validate_upgraded_dump_restore() {
    local restore_db="${DBNAME}_restore"
    local dump_file="${HEAVY_TMPDIR}/${DBNAME}-upgrade-restore.sql"
    require_simple_identifier "${restore_db}" "restore database"
    "$(pg_bin pg_dump)" -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" \
        --format=plain --no-owner --no-privileges --file="${dump_file}"
    drop_database "${restore_db}"
    create_database "${restore_db}"
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${restore_db}" -v ON_ERROR_STOP=1 \
        -f "${dump_file}"
    local restored_namespace
    restored_namespace="$(psql -h "${PGHOST}" -p "${PGPORT}" -d "${restore_db}" -Atc \
        "SELECT namespace.nspname FROM pg_catalog.pg_extension AS extension JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = extension.extnamespace WHERE extension.extname = 'pgcontext'")"
    if [[ "${restored_namespace}" != "pgcontext" ]]; then
        echo "restored extension namespace is ${restored_namespace}, expected pgcontext" >&2
        exit 1
    fi
    drop_database "${restore_db}"
    rm -f -- "${dump_file}"
    echo "upgrade_dump_restore_exercised: ${CURRENT_VERSION}"
}

validate_pgvector_first_upgrade_preflight() {
    local previous_version="$1"
    reset_database
    stage_pgvector_stub_if_needed
    stage_previous_control "${previous_version}"
    psql_db -c "CREATE EXTENSION vector" \
        -c "CREATE TABLE public.coexist_docs (id bigint PRIMARY KEY, embedding public.vector NOT NULL)" \
        -c "INSERT INTO public.coexist_docs VALUES (1, '[1,2]'::public.vector)" \
        -c "CREATE EXTENSION pgcontext VERSION '${previous_version}'"
    restore_current_control
    if psql_db -c "ALTER EXTENSION pgcontext UPDATE TO '${CURRENT_VERSION}'"; then
        echo "pgvector-first ${previous_version} upgrade unexpectedly succeeded" >&2
        exit 1
    fi
    psql_db <<SQL
DO \$\$
DECLARE
    installed_version text;
    vector_owner text;
    strategy_column_exists boolean;
    preserved_value text;
    preserved_type oid;
BEGIN
    SELECT extversion INTO installed_version
      FROM pg_catalog.pg_extension WHERE extname = 'pgcontext';
    IF installed_version <> '${previous_version}' THEN
        RAISE EXCEPTION 'failed coexistence preflight changed extension version to %', installed_version;
    END IF;
    SELECT extension.extname
      INTO vector_owner
      FROM pg_catalog.pg_type AS type
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = type.typnamespace
      JOIN pg_catalog.pg_depend AS dependency
        ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
       AND dependency.objid = type.oid
       AND dependency.deptype = 'e'
      JOIN pg_catalog.pg_extension AS extension ON extension.oid = dependency.refobjid
     WHERE namespace.nspname = 'public' AND type.typname = 'vector';
    IF vector_owner <> 'vector' THEN
        RAISE EXCEPTION 'coexistence preflight changed pgvector type ownership to %', vector_owner;
    END IF;
    SELECT EXISTS (
        SELECT 1 FROM pg_catalog.pg_attribute
         WHERE attrelid = 'pgcontext._query_stats'::pg_catalog.regclass
           AND attname = 'strategy' AND NOT attisdropped
    ) INTO strategy_column_exists;
    IF strategy_column_exists THEN
        RAISE EXCEPTION 'failed coexistence preflight left partial 0.2 catalog changes';
    END IF;
    SELECT embedding::text, pg_catalog.pg_typeof(embedding)::oid
      INTO preserved_value, preserved_type
      FROM public.coexist_docs WHERE id = 1;
    IF preserved_value <> '[1,2]' OR preserved_type <> 'public.vector'::pg_catalog.regtype::oid THEN
        RAISE EXCEPTION 'failed coexistence preflight changed user vector value/type: %, %',
                        preserved_value, preserved_type;
    END IF;
END
\$\$;
SQL
    echo "pgvector_first_upgrade_preflight_exercised: ${previous_version} -> ${CURRENT_VERSION}"
}

validate_non_superuser_upgrade_refusal() {
    local previous_version="$1"
    local probe_role="m1_upgrade_non_super_probe"
    reset_database
    drop_role_if_exists "${probe_role}"
    psql_postgres -c "CREATE ROLE ${probe_role} LOGIN SUPERUSER"
    stage_previous_control "${previous_version}"
    psql_db -c "SET SESSION AUTHORIZATION ${probe_role}; CREATE EXTENSION pgcontext VERSION '${previous_version}'"
    restore_current_control
    psql_postgres -c "ALTER ROLE ${probe_role} NOSUPERUSER"

    if psql_db -c "SET SESSION AUTHORIZATION ${probe_role}; ALTER EXTENSION pgcontext UPDATE TO '${CURRENT_VERSION}'"; then
        echo "non-superuser ${previous_version} upgrade unexpectedly succeeded" >&2
        exit 1
    fi
    assert_sql_equals \
        "SELECT extversion FROM pg_catalog.pg_extension WHERE extname = 'pgcontext'" \
        "${previous_version}"
    assert_sql_equals \
        "SELECT (pg_catalog.to_regtype('public.vector') IS NOT NULL AND pg_catalog.to_regtype('pgcontext.vector') IS NULL)::text" \
        "true"
    assert_sql_equals \
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid = 'pgcontext._query_stats'::pg_catalog.regclass AND attname = 'strategy' AND NOT attisdropped)::text" \
        "false"

    reset_database
    drop_role_if_exists "${probe_role}"
    echo "non_superuser_upgrade_refusal_exercised: ${previous_version} -> ${CURRENT_VERSION}"
}

validate_upgraded_drop_recreate() {
    local locker_application="pgcontext_upgrade_recreation_locker"
    local ddl_application="pgcontext_upgrade_recreation_ddl"
    local locker_client_pid locker_backend_pid ddl_client_pid ddl_backend_pid
    PGAPPNAME="${locker_application}" psql_db -c "
        BEGIN;
        LOCK TABLE pgcontext._query_stats IN ACCESS EXCLUSIVE MODE;
        SELECT pg_catalog.pg_sleep(30);
        COMMIT
    " >/dev/null 2>&1 &
    locker_client_pid=$!
    locker_backend_pid=""
    for _ in $(seq 1 100); do
        locker_backend_pid="$(psql_db -Atc "
            SELECT activity.pid
              FROM pg_catalog.pg_stat_activity AS activity
              JOIN pg_catalog.pg_locks AS lock
                ON lock.pid = activity.pid
               AND lock.relation = 'pgcontext._query_stats'::pg_catalog.regclass
               AND lock.granted
             WHERE activity.application_name = '${locker_application}'
               AND activity.datname = current_database()
             LIMIT 1
        ")"
        [[ "${locker_backend_pid}" =~ ^[1-9][0-9]*$ ]] && break
        sleep 0.05
    done
    if [[ ! "${locker_backend_pid}" =~ ^[1-9][0-9]*$ ]]; then
        echo "recreation probe could not establish the telemetry table lock" >&2
        exit 1
    fi

    psql_db <<'SQL'
DO $$
BEGIN
    FOR attempt IN 1..512 LOOP
        PERFORM *
          FROM pgcontext.execute_query(
               'upgrade_docs',
               pgcontext.query_lookup(ARRAY[1]::bigint[])
          );
    END LOOP;
END
$$;
SQL
    local pending_before old_worker_pid
    pending_before="$(psql_db -Atc "SELECT pending FROM pgcontext.query_telemetry_queue_stats()")"
    old_worker_pid="$(psql_db -Atc "SELECT COALESCE(worker_pid, 0) FROM pgcontext.query_telemetry_queue_stats()")"
    if [[ ! "${pending_before}" =~ ^[1-9][0-9]*$ || ! "${old_worker_pid}" =~ ^[1-9][0-9]*$ ]]; then
        echo "recreation probe did not establish pending work and a live old worker: pending=${pending_before}, worker=${old_worker_pid}" >&2
        exit 1
    fi

    PGAPPNAME="${ddl_application}" psql_db -c "
        BEGIN;
        DROP EXTENSION pgcontext CASCADE;
        CREATE EXTENSION pgcontext;
        COMMIT
    " >/dev/null 2>&1 &
    ddl_client_pid=$!
    ddl_backend_pid=""
    for _ in $(seq 1 100); do
        ddl_backend_pid="$(psql_db -Atc "
            SELECT pid
              FROM pg_catalog.pg_stat_activity
             WHERE application_name = '${ddl_application}'
               AND datname = current_database()
               AND wait_event_type = 'Lock'
             LIMIT 1
        ")"
        [[ "${ddl_backend_pid}" =~ ^[1-9][0-9]*$ ]] && break
        sleep 0.05
    done
    if [[ ! "${ddl_backend_pid}" =~ ^[1-9][0-9]*$ ]]; then
        echo "recreation DDL did not wait behind the telemetry worker" >&2
        exit 1
    fi
    assert_sql_equals \
        "SELECT pg_catalog.pg_terminate_backend(${locker_backend_pid})::text" \
        "true"
    if wait "${locker_client_pid}"; then
        echo "recreation lock holder unexpectedly completed" >&2
        exit 1
    fi
    wait "${ddl_client_pid}"

    assert_sql_equals \
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = ${old_worker_pid})::text" \
        "true"
    psql_db <<'SQL'
CREATE TABLE public.upgrade_recreate_probe (id bigint PRIMARY KEY);
INSERT INTO public.upgrade_recreate_probe VALUES (1);
SELECT pgcontext.create_collection(
    'upgrade_recreate_probe', 'public.upgrade_recreate_probe'
);
SELECT pgcontext.upsert_points('upgrade_recreate_probe', ARRAY['1']);
SELECT *
  FROM pgcontext.execute_query(
       'upgrade_recreate_probe',
       pgcontext.query_lookup(ARRAY[1]::bigint[])
  );
SQL
    for _ in $(seq 1 100); do
        recreation_state="$(psql_db -Atc "
            SELECT pending || '|' || dropped_orphaned || '|' ||
                   database_slot_exhausted || '|' || COALESCE(worker_pid, 0)
              FROM pgcontext.query_telemetry_queue_stats()
        ")"
        IFS='|' read -r pending dropped_orphaned slot_exhausted new_worker_pid \
            <<<"${recreation_state}"
        if [[ "${pending}" == "0" && "${dropped_orphaned}" =~ ^[1-9][0-9]*$ ]]; then
            break
        fi
        sleep 0.05
    done
    if [[ "${pending}" != "0" || ! "${dropped_orphaned}" =~ ^[1-9][0-9]*$ \
          || "${slot_exhausted}" != "0" || "${new_worker_pid}" == "${old_worker_pid}" ]]; then
        echo "unsafe extension-generation slot transition: ${recreation_state}, old_worker=${old_worker_pid}" >&2
        exit 1
    fi
    assert_sql_equals \
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = ${old_worker_pid})::text" \
        "true"
    assert_sql_equals \
        "SELECT namespace.nspname FROM pg_catalog.pg_extension AS extension JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = extension.extnamespace WHERE extension.extname = 'pgcontext'" \
        "pgcontext"
    echo "upgrade_drop_recreate_exercised: ${CURRENT_VERSION} (purged ${dropped_orphaned} stale events)"
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
    validate_visibility_barriers
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
validate_visibility_barriers
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

if [[ "${UPGRADE_MATRIX_STAGING_ONLY:-0}" == "1" ]]; then
    for version in "${previous_versions[@]}"; do
        stage_previous_control "${version}"
        psql_db -c "CREATE EXTENSION pgcontext VERSION '${version}'"
        restore_current_control
        echo "upgrade_staging_exercised: ${version} -> ${CURRENT_VERSION}"
    done
    exit 0
fi

for version in "${previous_versions[@]}"; do
    validate_pgvector_first_upgrade_preflight "${version}"
    validate_non_superuser_upgrade_refusal "${version}"
    reset_database
    stage_previous_control "${version}"
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
    restore_current_control

    validate_lifecycle_state
    load_representative_legacy_state

    psql_db -c "ALTER EXTENSION pgcontext UPDATE TO '${CURRENT_VERSION}'"
    echo "upgrade_path_exercised: ${version} -> ${CURRENT_VERSION}"

    validate_legacy_automatic_reclassification
    validate_upgrade_catalog_transition
    validate_lifecycle_state
    validate_visibility_barriers
    validate_representative_behavior
    validate_vector_metadata_compatibility
    validate_upgraded_dump_restore
    validate_failed_update_rollback_path
    validate_upgraded_drop_recreate
done
