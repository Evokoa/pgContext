#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_concurrent_read_write}"
ROUNDS="${ROUNDS:-8}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.concurrent_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.concurrent_hnsw_docs (id, embedding, body)
SELECT value,
       format('[%s,0]', value)::vector,
       format('seed %s', value)
  FROM generate_series(1, 16) AS value;

CREATE INDEX concurrent_hnsw_docs_embedding_idx
    ON public.concurrent_hnsw_docs USING pgcontext_hnsw (embedding);
SQL

writer_one_sql="${HEAVY_TMPDIR}/${DBNAME}_writer_one.sql"
writer_two_sql="${HEAVY_TMPDIR}/${DBNAME}_writer_two.sql"
reader_sql="${HEAVY_TMPDIR}/${DBNAME}_reader.sql"
lock_holder_sql="${HEAVY_TMPDIR}/${DBNAME}_lock_holder.sql"
lock_ready="${HEAVY_TMPDIR}/${DBNAME}_lock_ready"
lock_release="${HEAVY_TMPDIR}/${DBNAME}_lock_release"
background_pids=()
cleanup_sql() {
    touch "${lock_release}"
    local pid
    for pid in "${background_pids[@]}"; do
        kill "${pid}" 2>/dev/null || true
    done
    for pid in "${background_pids[@]}"; do
        wait "${pid}" 2>/dev/null || true
    done
    rm -f "${writer_one_sql}" "${writer_two_sql}" "${reader_sql}" "${lock_holder_sql}" "${lock_ready}" "${lock_release}"
}
trap cleanup_sql EXIT

index_oid="$(psql_db -Atc "SELECT 'public.concurrent_hnsw_docs_embedding_idx'::regclass::oid")"
lock_namespace=$((0x50474358))
rm -f "${lock_ready}" "${lock_release}"
cat >"${lock_holder_sql}" <<SQL
SELECT pg_advisory_lock(${lock_namespace}, ${index_oid}::integer);
\! touch '${lock_ready}'
\! while [ ! -f '${lock_release}' ]; do sleep 0.05; done
SELECT pg_advisory_unlock(${lock_namespace}, ${index_oid}::integer);
SQL

cat >"${writer_one_sql}" <<SQL
DO \$\$
DECLARE
    round integer;
BEGIN
    FOR round IN 1..${ROUNDS} LOOP
        INSERT INTO public.concurrent_hnsw_docs (id, embedding, body)
        VALUES (1000 + round, format('[%s,0]', 1000 + round)::vector, format('writer %s', round));

        UPDATE public.concurrent_hnsw_docs
           SET embedding = format('[%s,0]', 4000 + round)::vector
         WHERE id = round;
    END LOOP;
END
\$\$;
SQL

cat >"${writer_two_sql}" <<SQL
DO \$\$
DECLARE
    round integer;
BEGIN
    FOR round IN 1..${ROUNDS} LOOP
        INSERT INTO public.concurrent_hnsw_docs (id, embedding, body)
        VALUES (2000 + round, format('[%s,0]', 2000 + round)::vector, format('writer two %s', round));

        UPDATE public.concurrent_hnsw_docs
           SET embedding = format('[%s,0]', 5000 + round)::vector
         WHERE id = 8 + round;
    END LOOP;
END
\$\$;
SQL

cat >"${reader_sql}" <<SQL
SET enable_seqscan = off;
DO \$\$
DECLARE
    round integer;
    nearest_id bigint;
BEGIN
    FOR round IN 1..${ROUNDS} LOOP
        SELECT id
          INTO nearest_id
          FROM public.concurrent_hnsw_docs
         ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector
         LIMIT 1;
        IF nearest_id IS NULL THEN
            RAISE EXCEPTION 'concurrent HNSW read returned no rows at round %', round;
        END IF;
    END LOOP;
END
\$\$;
SQL

psql_db -f "${lock_holder_sql}" >/dev/null &
lock_holder_pid=$!
background_pids+=("${lock_holder_pid}")
for _attempt in {1..60}; do
    [[ -f "${lock_ready}" ]] && break
    sleep 0.05
done
if [[ ! -f "${lock_ready}" ]]; then
    echo "timed out waiting for HNSW writer lock fixture" >&2
    exit 1
fi

psql_db -f "${writer_one_sql}" &
writer_one_pid=$!
background_pids+=("${writer_one_pid}")
psql_db -f "${writer_two_sql}" &
writer_two_pid=$!
background_pids+=("${writer_two_pid}")
psql_db -f "${reader_sql}" &
reader_pid=$!
background_pids+=("${reader_pid}")

blocked_writers=0
for _attempt in {1..40}; do
    blocked_writers="$(psql_db -Atc "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND classid = ${lock_namespace}::oid AND objid = ${index_oid}::oid AND NOT granted")"
    [[ "${blocked_writers}" -ge 2 ]] && break
    sleep 0.05
done
if [[ "${blocked_writers}" -lt 2 ]]; then
    echo "competing HNSW writers did not block on the per-index allocator lock" >&2
    exit 1
fi
printf 'concurrent_hnsw_competing_writers_blocked\n'

touch "${lock_release}"
wait "${lock_holder_pid}"

wait "${writer_one_pid}"
printf 'concurrent_hnsw_writer_one_completed\n'
wait "${writer_two_pid}"
printf 'concurrent_hnsw_writer_two_completed\n'
printf 'concurrent_hnsw_writer_completed\n'
wait "${reader_pid}"
printf 'concurrent_hnsw_reader_completed\n'

psql_db <<SQL
SET enable_seqscan = off;

DO \$\$
DECLARE
    row_count bigint;
    nearest_id bigint;
BEGIN
    SELECT count(*) INTO row_count FROM public.concurrent_hnsw_docs;
    IF row_count <> 16 + (2 * ${ROUNDS}) THEN
        RAISE EXCEPTION 'unexpected concurrent row count: %', row_count;
    END IF;

    SELECT id
      INTO nearest_id
      FROM public.concurrent_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) format('[%s,0]', 1000 + ${ROUNDS})::vector
     LIMIT 1;
    IF nearest_id <> 1000 + ${ROUNDS} THEN
        RAISE EXCEPTION 'concurrent HNSW index did not see inserted row: %', nearest_id;
    END IF;

    SELECT id
      INTO nearest_id
      FROM public.concurrent_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) format('[%s,0]', 2000 + ${ROUNDS})::vector
     LIMIT 1;
    IF nearest_id <> 2000 + ${ROUNDS} THEN
        RAISE EXCEPTION 'concurrent HNSW index did not see second writer row: %', nearest_id;
    END IF;
END
\$\$;
SQL

printf 'concurrent_hnsw_row_count_verified\n'
printf 'concurrent_hnsw_competing_inserts_visible\n'
printf 'concurrent_hnsw_insert_visible\n'
