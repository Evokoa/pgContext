#!/usr/bin/env bash
# P2-S5 crash-safety gate: crash the server at every WAL boundary of the
# segmented write path and prove the index recovers correct and usable.
#
# Each case injects a physical failpoint, runs the operation so it aborts at
# that exact point with pages already written and WAL already emitted, then
# stops the postmaster with -m immediate (no checkpoint, shared buffers
# discarded) so recovery must replay from WAL. After restart the index must:
#
#   1. answer an ordered scan identically to a sequential-scan oracle over
#      the same rows — the generation-agnostic statement of "wholly the old
#      graph or wholly the new one, never a mix";
#   2. still accept writes, proving recovery did not wedge the allocator or
#      leave the metapage pointing at a half-written region.
#
# Requires the extension built with the pg_test feature: the failpoint setter
# is compiled out of release builds on purpose.
set -euo pipefail

PG_VERSION="${PG_VERSION:-pg17}"
PG_FEATURE="${PG_FEATURE:-pg17}"
PG_CONFIG="${PG_CONFIG:-${HOME}/.pgrx/17.10/pgrx-install/bin/pg_config}"
PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-28817}"
PGRX_HOME="${PGRX_HOME:-${HOME}/.pgrx}"
PGDATA="${PGDATA:-${PGRX_HOME}/data-${PG_VERSION#pg}}"
PG_CTL="${PG_CTL:-$(dirname "${PG_CONFIG}")/pg_ctl}"
DBNAME="${DBNAME:-pgcontext_hnsw_crash_check}"

if [[ ! "${DBNAME}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "DBNAME must be a simple SQL identifier" >&2
    exit 2
fi

psql_db() {
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

restart_after_stop=false
cleanup() {
    if [[ "${restart_after_stop}" == "true" ]]; then
        cargo pgrx start "${PG_VERSION}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

crash_and_restart() {
    # Force every WAL record the aborted operation emitted to disk first.
    #
    # Index page writes go through Generic WAL and are not transactional, so
    # they survive the abort — but `stop -m immediate` discards *unflushed*
    # WAL, and an aborted transaction never commits, so nothing forces a
    # flush. Without this the crash would roll the index back to its
    # pre-operation state and the gate would pass no matter how the write
    # order was broken. A committed write plus an explicit CHECKPOINT makes
    # the half-finished state durable, which is the state recovery must
    # actually cope with.
    psql_db -c "CREATE TABLE IF NOT EXISTS crash_wal_flush(n int)" \
            -c "INSERT INTO crash_wal_flush VALUES (1)" \
            -c "CHECKPOINT" >/dev/null
    restart_after_stop=true
    "${PG_CTL}" -D "${PGDATA}" stop -m immediate >/dev/null
    cargo pgrx start "${PG_VERSION}" >/dev/null
    restart_after_stop=false
}

cargo pgrx start "${PG_VERSION}" >/dev/null
# pg_test build: `pgcontext.test_set_hnsw_physical_failpoint` exists only here.
cargo pgrx install -p context-pg --features "${PG_FEATURE} pg_test" \
    --pg-config "${PG_CONFIG}" >/dev/null

psql -h "${PGHOST}" -p "${PGPORT}" -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS ${DBNAME}" \
    -c "CREATE DATABASE ${DBNAME}" >/dev/null

# Distinct prime per dimension, modulus above the row count: every row's
# vector is unique, so ordering assertions are not really tie-breaks.
PROBE_VECTOR="(SELECT '[' || string_agg(((n * p) % 211 + 1)::text, ',' ORDER BY ord) || ']'
     FROM unnest(ARRAY[13,29,41,53,67,79,89,101]) WITH ORDINALITY AS primes(p, ord))::vector"

psql_db >/dev/null <<SQL
CREATE EXTENSION pgcontext;

CREATE TABLE crash_items (id bigint PRIMARY KEY, embedding vector(8) NOT NULL);
INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(1, 150) n;
CREATE INDEX crash_items_hnsw ON crash_items
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
SQL

# Rows appended after the build land in the delta segment, so every case below
# crashes with a populated delta rather than an empty one.
seed_delta() {
    psql_db >/dev/null <<SQL
DELETE FROM crash_items WHERE id > 150;
INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(151, 165) n;
SQL
}

verify_recovered() {
    local label="$1"
    # The oracle is computed live against the same rows, forced off the index,
    # rather than snapshotted up front: the recovered index must agree with
    # the table as it stands now, including whatever the aborted operation
    # did or did not durably add.
    psql_db >/dev/null <<SQL
DO \$\$
DECLARE
    indexed bigint[];
    expected bigint[];
BEGIN
    SET LOCAL enable_indexscan = off;
    SET LOCAL enable_bitmapscan = off;
    SET LOCAL enable_seqscan = on;
    SELECT array_agg(id) INTO expected
      FROM (
        SELECT id FROM crash_items
         ORDER BY embedding OPERATOR(pgcontext.<=>)
                  (SELECT embedding FROM crash_items WHERE id = 42), id
         LIMIT 20
      ) ranked;

    SET LOCAL enable_indexscan = on;
    SET LOCAL enable_seqscan = off;
    SELECT array_agg(id) INTO indexed
      FROM (
        SELECT id FROM crash_items
         ORDER BY embedding OPERATOR(pgcontext.<=>)
                  (SELECT embedding FROM crash_items WHERE id = 42), id
         LIMIT 20
      ) ranked;

    IF indexed IS DISTINCT FROM expected THEN
        RAISE EXCEPTION
            '${label}: index disagrees with the oracle after recovery: % vs %',
            indexed, expected;
    END IF;
END
\$\$;

-- A recovered index must still take writes AND serve what it just accepted.
--
-- Accepting the write is not enough to prove anything: a crashed compaction
-- leaves orphan pages at the end of the relation, and the append path reuses
-- the last page of a kind, so a row can land on a page no reader will look
-- at. That fails silently — the INSERT succeeds and the row is simply gone
-- from the index — so the row is read back here rather than only written.
-- Ids 180-185, not 900-905: the probe vector is (n*p) % 211, so ids that
-- differ by 211 are the *same point* (900 is a duplicate of 56). Every id
-- used here stays inside one period and clear of the ranges the cases above
-- insert, so each row is a distinct point.
INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(180, 185) n;
DO \$\$
DECLARE
    served bigint;
BEGIN
    SET LOCAL enable_seqscan = off;
    SET LOCAL enable_indexscan = on;
    -- Membership in a full ordered scan, not "is each row its own nearest
    -- neighbour at LIMIT 1". The ANN candidate budget is sized from the LIMIT,
    -- so a k=1 probe can miss an exact match and return a neighbour instead --
    -- approximate-search behaviour, not a missing row. A full scan must
    -- enumerate the graph, so a row absent there is genuinely not served.
    SELECT count(*) INTO served
      FROM (
        SELECT id FROM crash_items
         ORDER BY embedding OPERATOR(pgcontext.<=>)
                  (SELECT embedding FROM crash_items WHERE id = 182), id
         LIMIT 1000
      ) ranked
     WHERE id BETWEEN 180 AND 185;
    IF served <> 6 THEN
        RAISE EXCEPTION
            '${label}: rows accepted after recovery but the index serves only '
            '% of 6', served;
    END IF;
END
\$\$;
DELETE FROM crash_items WHERE id BETWEEN 180 AND 185;

-- The same read-back again, but forced down the inline node-append path.
--
-- The rows above take the delta path, which allocates Delta-kind pages and so
-- never looks at the Node pages a crashed compaction orphaned. The inline path
-- does: it appends to the last Node page in the relation, which after an
-- interrupted compaction is an orphan stamped for a generation readers skip.
-- A row written there is accepted and then invisible. Setting the delta limit
-- to zero is the documented way to select that path.
SET pgcontext.hnsw_delta_segment_limit = 0;
INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(190, 195) n;
DO \$\$
DECLARE
    served bigint;
BEGIN
    SET LOCAL enable_seqscan = off;
    SET LOCAL enable_indexscan = on;
    SELECT count(*) INTO served
      FROM (
        SELECT id FROM crash_items
         ORDER BY embedding OPERATOR(pgcontext.<=>)
                  (SELECT embedding FROM crash_items WHERE id = 192), id
         LIMIT 1000
      ) ranked
     WHERE id BETWEEN 190 AND 195;
    IF served <> 6 THEN
        RAISE EXCEPTION
            '${label}: rows accepted on the inline path after recovery but the '
            'index serves only % of 6', served;
    END IF;
END
\$\$;
DELETE FROM crash_items WHERE id BETWEEN 190 AND 195;
RESET pgcontext.hnsw_delta_segment_limit;
SQL
    echo "  ok: ${label}"
}

# Each case: failpoint name, the statement that must abort at it, and a label.
# The statement is wrapped so the injected error is caught — the point is the
# page/WAL state it leaves behind, not the error itself.
run_case() {
    local failpoint="$1" statement="$2" label="$3"
    seed_delta
    psql_db >/dev/null <<SQL
SELECT pgcontext.test_set_hnsw_physical_failpoint('${failpoint}');
DO \$\$
BEGIN
    ${statement}
EXCEPTION WHEN OTHERS THEN
    -- Expected: the injected failpoint aborted the operation partway.
    NULL;
END
\$\$;
SQL
    crash_and_restart
    verify_recovered "${label}"
}

echo "P2-S5 crash recovery: delta append boundaries"
run_case before_delta_append \
    "INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(200, 204) n;" \
    "crash before a delta record is appended"
run_case after_delta_append \
    "INSERT INTO crash_items SELECT n, ${PROBE_VECTOR} FROM generate_series(210, 214) n;" \
    "crash after the delta append, before the metapage counter is published"

echo "P2-S5 crash recovery: compaction boundaries"
run_case before_compaction_write \
    "PERFORM * FROM pgcontext.compact('crash_items_hnsw'::regclass);" \
    "crash before compaction writes its first fresh page"
run_case after_compaction_write \
    "PERFORM * FROM pgcontext.compact('crash_items_hnsw'::regclass);" \
    "crash after the fresh base is written, before the metapage flip"
run_case before_metapage_publication \
    "PERFORM * FROM pgcontext.compact('crash_items_hnsw'::regclass);" \
    "crash inside the metapage flip, before its WAL record is sealed"
run_case after_metapage_publication \
    "PERFORM * FROM pgcontext.compact('crash_items_hnsw'::regclass);" \
    "crash after the metapage flip is sealed"
run_case after_compaction_publish \
    "PERFORM * FROM pgcontext.compact('crash_items_hnsw'::regclass);" \
    "crash after compaction publishes, before the caller sees the result"

echo "P2-S5 crash recovery: a clean compaction still survives a crash"
seed_delta
psql_db >/dev/null <<SQL
SELECT pgcontext.test_set_hnsw_physical_failpoint(NULL);
SELECT * FROM pgcontext.compact('crash_items_hnsw'::regclass);
SQL
crash_and_restart
verify_recovered "crash after a completed compaction"

echo "hnsw crash-recovery gate passed: 8 boundaries, index correct and writable after every crash"
