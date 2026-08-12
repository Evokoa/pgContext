#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_replica_promotion}"
REPLICA_PORT="${REPLICA_PORT:-28917}"
REPLICA_HOST="${REPLICA_HOST:-127.0.0.1}"
REPLICA_USER="${REPLICA_USER:-pgcontext_hnsw_repl}"
PGRX_DATA_DIR="${PGRX_DATA_DIR:?set PGRX_DATA_DIR to the local pgrx primary data directory}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "DBNAME"
require_simple_identifier "${REPLICA_USER}" "REPLICA_USER"

PG_CTL="$(pg_bin pg_ctl)"
PG_BASEBACKUP="$(pg_bin pg_basebackup)"
REPLICA_DIR="${HEAVY_TMPDIR}/${DBNAME}_replica"
REPLICA_SOCKET="/tmp/pgctx_${DBNAME}_replica_sock"
REPLICA_LOG="${HEAVY_TMPDIR}/${DBNAME}_replica.log"
REPLICA_SLOT="${DBNAME}_slot"
HBA_MARKER="# pgcontext-hnsw-replica-${DBNAME}"
replica_started=0

cleanup() {
    local status=$?
    if [[ "${replica_started}" -eq 1 ]]; then
        "${PG_CTL}" -D "${REPLICA_DIR}" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    psql_postgres -Atc "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = '${REPLICA_SLOT}'" >/dev/null 2>&1 || true
    psql_postgres -c "DROP ROLE IF EXISTS ${REPLICA_USER}" >/dev/null 2>&1 || true
    /usr/bin/sed -i '' "/^${HBA_MARKER}$/,+1d" "${PGRX_DATA_DIR}/pg_hba.conf" >/dev/null 2>&1 || true
    "${PG_CTL}" -D "${PGRX_DATA_DIR}" reload >/dev/null 2>&1 || true
    if [[ "${status}" -eq 0 ]]; then
        rm -rf "${REPLICA_DIR}" "${REPLICA_SOCKET}" "${REPLICA_LOG}"
    else
        echo "preserved replica artifacts: ${REPLICA_DIR} ${REPLICA_LOG}" >&2
    fi
}
trap cleanup EXIT

psql_replica() {
    psql -h "${REPLICA_HOST}" -p "${REPLICA_PORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

start_replica() {
    mkdir -p "${REPLICA_SOCKET}"
    "${PG_CTL}" -D "${REPLICA_DIR}" -l "${REPLICA_LOG}" \
        -o "-p ${REPLICA_PORT} -h ${REPLICA_HOST} -k ${REPLICA_SOCKET}" -w start
    replica_started=1
}

validate_promoted_oracle() {
    local indexed exact
    exact="$(psql_replica -At <<'SQL' | tail -n 1
SET enable_indexscan = off;
SELECT pg_catalog.jsonb_build_object(
    'dense', (SELECT pg_catalog.array_agg(id ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::pgcontext.vector)
                FROM (SELECT id, embedding FROM public.hnsw_replica_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::pgcontext.vector LIMIT 3) AS ranked),
    'int8', (SELECT pg_catalog.array_agg(id ORDER BY int8_value OPERATOR(pgcontext.<->) pgcontext.int8vec('[9,0]'))
               FROM (SELECT id, int8_value FROM public.hnsw_replica_docs ORDER BY int8_value OPERATOR(pgcontext.<->) pgcontext.int8vec('[9,0]') LIMIT 3) AS ranked),
    'uint8', (SELECT pg_catalog.array_agg(id ORDER BY uint8_value OPERATOR(pgcontext.<->) pgcontext.uint8vec('[9,0]'))
                FROM (SELECT id, uint8_value FROM public.hnsw_replica_docs ORDER BY uint8_value OPERATOR(pgcontext.<->) pgcontext.uint8vec('[9,0]') LIMIT 3) AS ranked)
);
SQL
)"
    indexed="$(psql_replica -At <<'SQL' | tail -n 1
SET enable_seqscan = off;
SELECT pg_catalog.jsonb_build_object(
    'dense', (SELECT pg_catalog.array_agg(id ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::pgcontext.vector)
                FROM (SELECT id, embedding FROM public.hnsw_replica_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::pgcontext.vector LIMIT 3) AS ranked),
    'int8', (SELECT pg_catalog.array_agg(id ORDER BY int8_value OPERATOR(pgcontext.<->) pgcontext.int8vec('[9,0]'))
               FROM (SELECT id, int8_value FROM public.hnsw_replica_docs ORDER BY int8_value OPERATOR(pgcontext.<->) pgcontext.int8vec('[9,0]') LIMIT 3) AS ranked),
    'uint8', (SELECT pg_catalog.array_agg(id ORDER BY uint8_value OPERATOR(pgcontext.<->) pgcontext.uint8vec('[9,0]'))
                FROM (SELECT id, uint8_value FROM public.hnsw_replica_docs ORDER BY uint8_value OPERATOR(pgcontext.<->) pgcontext.uint8vec('[9,0]') LIMIT 3) AS ranked)
);
SQL
)"
    if [[ "${indexed}" != "${exact}" ]]; then
        echo "promoted HNSW oracle mismatch: indexed=${indexed}, exact=${exact}" >&2
        exit 1
    fi
    printf 'hnsw_replica_promotion_oracle: passed\n'
}

validate_promoted_document_chunks() {
    local current
    current="$(psql_replica -At <<'SQL' | tail -n 1
SELECT pg_catalog.jsonb_build_object(
    'count', pg_catalog.count(*),
    'versions', pg_catalog.array_agg(DISTINCT source_version ORDER BY source_version)
)
  FROM pgcontext.current_document_chunks(
      'replica_chunk_docs', 'body', ARRAY['1']
  );
SQL
)"
    if [[ "${current}" != '{"count": 1, "versions": [2]}' ]]; then
        echo "promoted automatic-chunking oracle mismatch: ${current}" >&2
        exit 1
    fi
    printf 'document_chunking_replica_promotion_oracle: passed\n'
}

start_and_install_extension
reset_database

printf '%s\nhost replication %s 127.0.0.1/32 trust\n' "${HBA_MARKER}" "${REPLICA_USER}" >>"${PGRX_DATA_DIR}/pg_hba.conf"
"${PG_CTL}" -D "${PGRX_DATA_DIR}" reload
psql_postgres -c "CREATE ROLE ${REPLICA_USER} REPLICATION LOGIN"

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.hnsw_replica_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    int8_value int8vec(2) NOT NULL,
    uint8_value uint8vec(2) NOT NULL
);
INSERT INTO public.hnsw_replica_docs VALUES
  (1, '[1,0]'::vector, '[1,0]'::int8vec, '[1,0]'::uint8vec),
  (2, '[2,0]'::vector, '[2,0]'::int8vec, '[2,0]'::uint8vec),
  (9, '[9,0]'::vector, '[9,0]'::int8vec, '[9,0]'::uint8vec);
CREATE INDEX hnsw_replica_docs_embedding_idx ON public.hnsw_replica_docs USING pgcontext_hnsw (embedding);
CREATE INDEX hnsw_replica_docs_int8_idx ON public.hnsw_replica_docs USING pgcontext_hnsw
    (int8_value pgcontext.int8vec_hnsw_ops);
CREATE INDEX hnsw_replica_docs_uint8_idx ON public.hnsw_replica_docs USING pgcontext_hnsw
    (uint8_value pgcontext.uint8vec_hnsw_ops);

CREATE TABLE public.replica_chunk_docs (
    id bigint PRIMARY KEY,
    body text NOT NULL,
    source_version bigint NOT NULL
);
INSERT INTO public.replica_chunk_docs VALUES (1, 'replica publication one', 1);
SELECT pgcontext.create_collection('replica_chunk_docs', 'public.replica_chunk_docs');
SELECT pgcontext.create_document_chunk_projection('public.replica_document_chunks');
SELECT pgcontext.register_chunking_profile(
    'replica_chunk_profile', 'plain_text_v1', 64, 96, 8, 8, 8388608, false
);
SELECT pgcontext.register_document_source(
    'replica_chunk_docs', 'body', 'body', 'source_version',
    'public.replica_document_chunks', 'replica_chunk_profile'
);
SELECT pgcontext.install_document_chunk_trigger('replica_chunk_docs', 'body');
SELECT pgcontext.enqueue_document_chunking('replica_chunk_docs', 'body', ARRAY['1']);
SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
  FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'replica-initial-worker');
CHECKPOINT;
SQL

rm -rf "${REPLICA_DIR}"
"${PG_BASEBACKUP}" -h "${PGHOST}" -p "${PGPORT}" -U "${REPLICA_USER}" \
    -D "${REPLICA_DIR}" -R -X stream -C -S "${REPLICA_SLOT}"
start_replica

psql_db -c "INSERT INTO public.hnsw_replica_docs VALUES (10, '[10,0]'::vector, '[10,0]'::int8vec, '[10,0]'::uint8vec)"
psql_db <<'SQL'
UPDATE public.replica_chunk_docs
   SET body = 'replica publication two', source_version = 2
 WHERE id = 1;
SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
  FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'replica-update-worker');
SQL
for _ in {1..30}; do
    if psql_replica -Atc "SELECT count(*) FROM public.hnsw_replica_docs" | grep -qx '4' \
       && psql_replica -Atc "SELECT count(*) FROM pgcontext._current_document_chunk_generations AS current JOIN pgcontext._document_chunk_generations AS generations USING (generation_id) WHERE generations.source_version = 2" | grep -qx '1'; then
        break
    fi
    sleep 1
done
psql_replica -Atc "SELECT count(*) FROM public.hnsw_replica_docs" | grep -qx '4'
psql_replica -Atc "SELECT count(*) FROM pgcontext._current_document_chunk_generations AS current JOIN pgcontext._document_chunk_generations AS generations USING (generation_id) WHERE generations.source_version = 2" | grep -qx '1'

"${PG_CTL}" -D "${REPLICA_DIR}" promote -w
validate_promoted_oracle
validate_promoted_document_chunks
