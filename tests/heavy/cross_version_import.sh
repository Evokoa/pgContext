#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_cross_version_import}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

CURRENT_VERSION="$(sed -n "s/^default_version = '\([^']*\)'/\1/p" "${REPO_ROOT}/crates/context-pg/pgcontext.control")"
if [[ -z "${CURRENT_VERSION}" ]]; then
    echo "could not determine current extension version" >&2
    exit 2
fi

DUMP_DIR="${HEAVY_TMPDIR}/cross-version-import"
mkdir -p "${DUMP_DIR}"

discover_source_versions() {
    find "${REPO_ROOT}/sql" -maxdepth 1 -type f -name 'pgcontext--*.sql' ! -name 'pgcontext--*--*.sql' \
        | sed -n 's/.*pgcontext--\(.*\)\.sql$/\1/p' \
        | sort
}

if [[ -n "${SOURCE_VERSIONS:-}" ]]; then
    # shellcheck disable=SC2206
    SOURCE_VERSION_LIST=(${SOURCE_VERSIONS})
else
    SOURCE_VERSION_LIST=()
    while IFS= read -r version; do
        SOURCE_VERSION_LIST+=("${version}")
    done < <(discover_source_versions)
fi

if [[ "${#SOURCE_VERSION_LIST[@]}" -eq 0 ]]; then
    SOURCE_VERSION_LIST=("${CURRENT_VERSION}")
fi

version_label() {
    printf '%s' "$1" | tr -c 'A-Za-z0-9_' '_'
}

load_import_fixture() {
    local source_version="$1"
    psql_db <<SQL
CREATE EXTENSION pgcontext VERSION '${source_version}';

CREATE TABLE public.docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    sparse_terms sparsevec NOT NULL,
    body text NOT NULL,
    tenant text NOT NULL,
    metadata jsonb NOT NULL
);

INSERT INTO public.docs (id, embedding, sparse_terms, body, tenant, metadata)
VALUES
    (1, '[0,0]'::vector, pgcontext.sparsevec('{1:0.9,3:0.1}/16'), 'database internals', 'acme', '{"priority":"high","lang":"en"}'),
    (2, '[1,0]'::vector, pgcontext.sparsevec('{2:0.8,4:0.2}/16'), 'query planning', 'acme', '{"priority":"low","lang":"en"}'),
    (3, '[5,5]'::vector, pgcontext.sparsevec('{8:1}/16'), 'gardening notes', 'other', '{"priority":"low","lang":"fr"}');

SELECT * FROM pgcontext.create_collection('import_docs', 'public.docs');
SELECT * FROM pgcontext.register_vector('import_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.configure_vector(
    'import_docs',
    'embedding',
    '{"m":16,"ef_search":64}'::jsonb,
    '{"metadata_version":1,"mode":"scalar","levels":256}'::jsonb,
    'ready'
);
SELECT * FROM pgcontext.register_sparse_vector('import_docs', 'lexical', 'sparse_terms', 16, 'inner_product');
SELECT * FROM pgcontext.configure_sparse_vector(
    'import_docs',
    'lexical',
    '{"format":"source_table_column"}'::jsonb,
    '{"strategy":"exact"}'::jsonb,
    'ready'
);
SELECT * FROM pgcontext.register_filter_column('import_docs', 'tenant', 'tenant');
SELECT * FROM pgcontext.register_jsonb_path('import_docs', 'priority', 'metadata', ARRAY['priority']);
SELECT * FROM pgcontext.upsert_points('import_docs', ARRAY['1', '2', '3']);
SELECT * FROM pgcontext.record_query_stat('import_docs', 'tenant:acme', 'search_filtered', 2, 3, 1.25);
SELECT * FROM pgcontext.register_model_version('import_docs', 'embed-small', 'v1', 2, 'l2');
SELECT * FROM pgcontext.register_model_version('import_docs', 'embed-small', 'v2', 2, 'l2');
SELECT * FROM pgcontext.create_embedding_migration('import_docs', 'embed-small', 'v1', 'embed-small', 'v2', 3);
CREATE INDEX docs_embedding_hnsw_idx ON public.docs USING pgcontext_hnsw (embedding);
SQL
}

update_imported_extension() {
    local restored_version
    restored_version="$(psql_db -Atc "SELECT extversion FROM pg_catalog.pg_extension WHERE extname = 'pgcontext'")"
    if [[ "${restored_version}" != "${CURRENT_VERSION}" ]]; then
        psql_db -c "ALTER EXTENSION pgcontext UPDATE TO '${CURRENT_VERSION}'"
    fi
}

validate_imported_fixture() {
    local source_version="$1"
    psql_db <<SQL
DO \$\$
DECLARE
    restored_version text;
    nearest_source_key text;
    filtered_count bigint;
    sparse_source_key text;
    fused_count bigint;
    dense_options jsonb;
    sparse_options jsonb;
    point_count bigint;
    model_count bigint;
    migration_count bigint;
    restored_query_count bigint;
    telemetry_status text;
    restored_hnsw_indexes bigint;
BEGIN
    SELECT extversion
      INTO restored_version
      FROM pg_catalog.pg_extension
     WHERE extname = 'pgcontext';
    IF restored_version <> '${CURRENT_VERSION}' THEN
        RAISE EXCEPTION 'restored extension version %, expected ${CURRENT_VERSION}', restored_version;
    END IF;

    SELECT source_key
      INTO nearest_source_key
      FROM pgcontext.search('import_docs', '[0,0]'::vector, 1);
    IF nearest_source_key <> '1' THEN
        RAISE EXCEPTION 'unexpected imported nearest source key from ${source_version}: %', nearest_source_key;
    END IF;

    SELECT count(*)
      INTO filtered_count
      FROM pgcontext.search(
          'import_docs',
          '[0,0]'::vector,
          '{"must":[{"key":"tenant","match":"acme"}]}',
          10
      );
    IF filtered_count <> 2 THEN
        RAISE EXCEPTION 'unexpected imported filtered count from ${source_version}: %', filtered_count;
    END IF;

    SELECT source_key
      INTO sparse_source_key
      FROM pgcontext.search_sparse(
          'import_docs',
          'lexical',
          pgcontext.sparsevec('{1:0.9,3:0.1}/16'),
          1
      );
    IF sparse_source_key <> '1' THEN
        RAISE EXCEPTION 'unexpected imported sparse source key from ${source_version}: %', sparse_source_key;
    END IF;

    SELECT count(*)
      INTO fused_count
      FROM pgcontext.query(
          'import_docs',
          '[0,0]'::vector,
          'lexical',
          pgcontext.sparsevec('{1:0.9,3:0.1}/16'),
          2
      );
    IF fused_count <> 2 THEN
        RAISE EXCEPTION 'unexpected imported dense+sparse fused count from ${source_version}: %', fused_count;
    END IF;

    SELECT quantization_options
      INTO dense_options
      FROM pgcontext.collection_vectors('import_docs')
     WHERE vector_name = 'embedding';
    IF dense_options <> '{"metadata_version": 1, "mode": "scalar", "levels": 256}'::jsonb THEN
        RAISE EXCEPTION 'unexpected imported dense metadata from ${source_version}: %', dense_options;
    END IF;

    SELECT storage_options
      INTO sparse_options
      FROM pgcontext.collection_sparse_vectors('import_docs')
     WHERE vector_name = 'lexical';
    IF sparse_options <> '{"format": "source_table_column"}'::jsonb THEN
        RAISE EXCEPTION 'unexpected imported sparse metadata from ${source_version}: %', sparse_options;
    END IF;

    SELECT count(*) INTO point_count FROM pgcontext.scroll('import_docs', NULL, 10);
    IF point_count <> 3 THEN
        RAISE EXCEPTION 'unexpected imported point count from ${source_version}: %', point_count;
    END IF;

    SELECT count(*) INTO model_count FROM pgcontext.model_versions()
     WHERE collection_name = 'import_docs';
    IF model_count <> 2 THEN
        RAISE EXCEPTION 'unexpected imported model version count from ${source_version}: %', model_count;
    END IF;

    SELECT count(*) INTO migration_count FROM pgcontext.embedding_migrations()
     WHERE collection_name = 'import_docs'
       AND status::text = 'Planned';
    IF migration_count <> 1 THEN
        RAISE EXCEPTION 'unexpected imported migration count from ${source_version}: %', migration_count;
    END IF;

    SELECT query_count
      INTO restored_query_count
      FROM pgcontext.query_cohort_stats()
     WHERE collection_name = 'import_docs'
       AND cohort = 'tenant:acme'
       AND query_kind = 'search_filtered';
    IF restored_query_count <> 1 THEN
        RAISE EXCEPTION 'unexpected imported query stat count from ${source_version}: %', restored_query_count;
    END IF;

    SELECT status::text, hnsw_indexes
      INTO telemetry_status, restored_hnsw_indexes
      FROM pgcontext.telemetry()
     WHERE collection_name = 'import_docs';
    IF telemetry_status <> 'Active' OR restored_hnsw_indexes <> 1 THEN
        RAISE EXCEPTION 'unexpected imported telemetry from ${source_version}: status %, hnsw indexes %',
            telemetry_status, restored_hnsw_indexes;
    END IF;

    PERFORM 1 FROM pgcontext.index_status('public.docs_embedding_hnsw_idx')
     WHERE access_method = 'pgcontext_hnsw'
       AND status::text = 'Ready';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'imported HNSW index metadata from ${source_version} was not ready';
    END IF;
END
\$\$;
SQL
}

start_and_install_extension

for source_version in "${SOURCE_VERSION_LIST[@]}"; do
    label="$(version_label "${source_version}")"
    source_db="${DBNAME}_source_${label}"
    import_db="${DBNAME}_import_${label}"
    dump_file="${DUMP_DIR}/${source_db}.dump"

    require_simple_identifier "${source_db}" "source database name"
    require_simple_identifier "${import_db}" "import database name"

    DBNAME="${source_db}"
    reset_database
    load_import_fixture "${source_version}"
    pg_dump -h "${PGHOST}" -p "${PGPORT}" -Fc -d "${source_db}" -f "${dump_file}"

    drop_database "${import_db}"
    create_database "${import_db}"
    pg_restore -h "${PGHOST}" -p "${PGPORT}" -d "${import_db}" --exit-on-error "${dump_file}"

    DBNAME="${import_db}"
    update_imported_extension
    validate_imported_fixture "${source_version}"
done
