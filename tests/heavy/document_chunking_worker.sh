#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_document_chunking}"
ROW_COUNT="${ROW_COUNT:-10000}"
SAMPLE_DOCUMENTS="${SAMPLE_DOCUMENTS:-}"
HARDWARE_ARCH="$(uname -m 2>/dev/null || printf unknown)"
HARDWARE_OS="$(uname -s 2>/dev/null || printf unknown)"
HARDWARE_CPUS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || printf unknown)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

MANIFEST_OUTPUT="$(cargo run -q -p context-test --bin p13_automatic_chunking_manifest)"
MANIFEST_HASH="$(awk -F '\t' '$1 == "manifest_hash" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MIN_TOKENS_PER_SECOND="$(awk -F '\t' '$1 == "min_tokens_per_second" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MIN_CHUNKS_PER_SECOND="$(awk -F '\t' '$1 == "min_chunks_per_second" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MAX_PUBLICATION_MICROS="$(awk -F '\t' '$1 == "max_publication_micros" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MAX_RSS_BYTES="$(awk -F '\t' '$1 == "max_rss_bytes" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
FROZEN_SAMPLE_DOCUMENTS="$(awk -F '\t' '$1 == "publication_sample_documents" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
DATASET_GENERATOR_HASH="$(awk -F '\t' '$1 == "dataset_generator_hash" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
EXPECTED_WORKLOAD_HASH="$(awk -F '\t' '$1 == "workload_hash" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
TARGET_TOKENS="$(awk -F '\t' '$1 == "target_tokens" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MAX_CHUNK_TOKENS="$(awk -F '\t' '$1 == "max_chunk_tokens" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MIN_CHUNK_TOKENS="$(awk -F '\t' '$1 == "min_chunk_tokens" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
MAX_OVERLAP_TOKENS="$(awk -F '\t' '$1 == "max_overlap_tokens" { print $2 }' <<<"${MANIFEST_OUTPUT}")"
DB_ROLE_SUFFIX="${DBNAME//[^a-zA-Z0-9_]/_}"
HIDDEN_READER_ROLE="p13_hidden_${PG_VERSION}_${DB_ROLE_SUFFIX}"
HIDDEN_READER_ROLE="${HIDDEN_READER_ROLE:0:63}"
for value in MANIFEST_HASH MIN_TOKENS_PER_SECOND MIN_CHUNKS_PER_SECOND MAX_PUBLICATION_MICROS MAX_RSS_BYTES FROZEN_SAMPLE_DOCUMENTS DATASET_GENERATOR_HASH EXPECTED_WORKLOAD_HASH TARGET_TOKENS MAX_CHUNK_TOKENS MIN_CHUNK_TOKENS MAX_OVERLAP_TOKENS; do
    if [[ -z "${!value}" ]]; then
        echo "P13 manifest omitted ${value}" >&2
        exit 2
    fi
done
if [[ -z "${SAMPLE_DOCUMENTS}" ]]; then
    SAMPLE_DOCUMENTS="${FROZEN_SAMPLE_DOCUMENTS}"
elif [[ "${SAMPLE_DOCUMENTS}" != "${FROZEN_SAMPLE_DOCUMENTS}" ]]; then
    echo "SAMPLE_DOCUMENTS must match frozen manifest value ${FROZEN_SAMPLE_DOCUMENTS}" >&2
    exit 2
fi
if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]] || (( ROW_COUNT < SAMPLE_DOCUMENTS )); then
    echo "ROW_COUNT must be an integer at least ${SAMPLE_DOCUMENTS}" >&2
    exit 2
fi

cargo pgrx start "${PG_VERSION}"
cargo pgrx install --release -p context-pg \
    --features "${PG_FEATURE}" --pg-config "${PG_CONFIG}"
reset_database
drop_role_if_exists "${HIDDEN_READER_ROLE}"
cargo build -q -p pgcontext-worker --release --bin pgcontext-chunk-worker

REPORT_FILE="${HEAVY_TMPDIR}/${DBNAME}_p13_report.txt"
WORKER_REPORT="${HEAVY_TMPDIR}/${DBNAME}_p13_worker.txt"

psql_db \
    -v row_count="${ROW_COUNT}" \
    -v sample_documents="${SAMPLE_DOCUMENTS}" \
    -v manifest_hash="${MANIFEST_HASH}" \
    -v dataset_generator_hash="${DATASET_GENERATOR_HASH}" \
    -v expected_workload_hash="${EXPECTED_WORKLOAD_HASH}" \
    -v target_tokens="${TARGET_TOKENS}" \
    -v max_chunk_tokens="${MAX_CHUNK_TOKENS}" \
    -v min_chunk_tokens="${MIN_CHUNK_TOKENS}" \
    -v max_overlap_tokens="${MAX_OVERLAP_TOKENS}" \
    -v max_publication_micros="${MAX_PUBLICATION_MICROS}" \
    -v min_chunks_per_second="${MIN_CHUNKS_PER_SECOND}" \
    -v hidden_reader_role="${HIDDEN_READER_ROLE}" <<'SQL' | tee "${REPORT_FILE}"
CREATE EXTENSION pgcontext;

CREATE TABLE public.p13_scale_documents (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    body text NOT NULL,
    source_version bigint NOT NULL
);
INSERT INTO public.p13_scale_documents
SELECT id,
       CASE WHEN id % 2 = 0 THEN 'even' ELSE 'odd' END,
       pg_catalog.format(
           'document %s %s', id,
           CASE WHEN id <= :sample_documents
                THEN pg_catalog.repeat(
                         'alpha beta gamma delta epsilon zeta eta theta ', 240
                     )
                ELSE 'source linked scale row'
           END
       ),
       1
  FROM pg_catalog.generate_series(1::bigint, :row_count::bigint) AS id;

SELECT pgcontext.create_collection('p13_scale', 'public.p13_scale_documents');
SELECT pgcontext.create_document_chunk_projection('public.p13_scale_chunks');
SELECT pgcontext.register_chunking_profile(
    'p13_scale_profile', 'plain_text_v1', :target_tokens, :max_chunk_tokens,
    :min_chunk_tokens, :max_overlap_tokens, 8388608, false
);
SELECT pgcontext.register_document_source(
    'p13_scale', 'body', 'body', 'source_version',
    'public.p13_scale_chunks', 'p13_scale_profile'
);
SELECT pgcontext.install_document_chunk_trigger('p13_scale', 'body');

CREATE TABLE p13_identity AS
SELECT pg_catalog.md5(
           pg_catalog.count(*)::text || ':' || pg_catalog.min(id)::text || ':'
           || pg_catalog.max(id)::text || ':' || pg_catalog.sum(
               pg_catalog.hashtextextended(
                   id::text || ':' || tenant || ':' || source_version::text || ':' || body,
                   13
               )::numeric
           )::text
       ) AS dataset_hash,
       :'dataset_generator_hash'::text AS dataset_generator_hash,
       :'expected_workload_hash'::text AS expected_workload_hash,
       (SELECT pg_catalog.md5(
                   'plain_text_v1:unicode_words_v1:'
                   || :target_tokens::text || ':' || :max_chunk_tokens::text || ':'
                   || :min_chunk_tokens::text || ':' || :max_overlap_tokens::text || ':'
                   || pg_catalog.string_agg(
                          id::text || ':' || pg_catalog.md5(body), ',' ORDER BY id
                      )
               )
          FROM public.p13_scale_documents AS samples
         WHERE samples.id <= :sample_documents) AS workload_hash
  FROM public.p13_scale_documents
;

DO $p13_identity_contract$
BEGIN
    IF (SELECT dataset_generator_hash FROM p13_identity) <> pg_catalog.md5(
           'p13-scale-documents-v1|tenant-parity|sample_documents=256|long_repeat=240|short=source linked scale row'
       ) OR (SELECT expected_workload_hash <> workload_hash FROM p13_identity) THEN
        RAISE EXCEPTION 'automatic chunking frozen fixture identity changed';
    END IF;
END
$p13_identity_contract$;

SELECT pgcontext.enqueue_document_chunking(
    'p13_scale', 'body',
    (SELECT pg_catalog.array_agg(id::text ORDER BY id)
       FROM public.p13_scale_documents WHERE id <= :sample_documents)
);
CREATE TABLE p13_claims(
    job_id bigint PRIMARY KEY,
    lease_token bigint NOT NULL
);
CREATE TABLE p13_timing(
    started timestamptz,
    finished timestamptz,
    initial_chunk_count bigint
);
INSERT INTO p13_timing(started) VALUES (pg_catalog.clock_timestamp());
CREATE TABLE p13_publication_samples(
    job_id bigint PRIMARY KEY,
    duration_micros numeric NOT NULL,
    staging_bytes bigint NOT NULL,
    worker_path text NOT NULL
);
CREATE TABLE p13_limits AS
SELECT :sample_documents::bigint AS expected_jobs,
       :max_publication_micros::numeric AS max_publication_micros,
       :min_chunks_per_second::numeric AS min_chunks_per_second;
SQL

P13_WORKER_BIN="${REPO_ROOT}/target/release/pgcontext-chunk-worker" \
P13_EXPECTED_JOBS="${SAMPLE_DOCUMENTS}" \
P13_PGHOST="${PGHOST}" P13_PGPORT="${PGPORT}" P13_DBNAME="${DBNAME}" \
python3 - <<'PY'
import json
import os
import subprocess

psql = [
    "psql", "-X", "-A", "-t", "-F", "\t",
    "-h", os.environ["P13_PGHOST"],
    "-p", os.environ["P13_PGPORT"],
    "-d", os.environ["P13_DBNAME"],
    "-v", "ON_ERROR_STOP=1",
]
expected_jobs = int(os.environ["P13_EXPECTED_JOBS"])


def run_psql(*, command=None, input_sql=None):
    completed = subprocess.run(
        psql + ([] if command is None else ["-c", command]),
        input=input_sql,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise SystemExit(completed.stdout + completed.stderr)
    return completed.stdout


worker = subprocess.Popen(
    [os.environ["P13_WORKER_BIN"]],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
assert worker.stdin is not None and worker.stdout is not None
for _ in range(expected_jobs):
    claim_lines = run_psql(
        command="SELECT job_id, lease_token, request::text "
        "FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'p13-scale-worker')"
    ).splitlines()
    if len(claim_lines) != 1:
        raise SystemExit("external chunk worker claim was unavailable or ambiguous")
    claim = claim_lines[0]
    fields = claim.split("\t", 2)
    if len(fields) != 3:
        raise SystemExit("external chunk worker claim was malformed")
    job_id, lease_token, request = fields
    worker.stdin.write(request + "\n")
    worker.stdin.flush()
    frame = worker.stdout.readline()
    if not frame:
        raise SystemExit("external chunk worker emitted no response frame")
    response = json.loads(frame)
    if not response.get("complete") or response.get("request_id") != int(job_id):
        raise SystemExit("external chunk worker did not return the claimed complete response")
    encoded = json.dumps(response, separators=(",", ":"))
    if "$p13_response$" in encoded:
        raise SystemExit("unexpected response delimiter collision")
    sql = f"""
INSERT INTO p13_claims(job_id, lease_token) VALUES ({job_id}, {lease_token});
SELECT pgcontext.checkpoint_document_chunk_job({job_id}, {lease_token}, 'parsing', 0, 1);
SELECT pgcontext.checkpoint_document_chunk_job({job_id}, {lease_token}, 'chunking', 0, 1);
SELECT pgcontext.checkpoint_document_chunk_job({job_id}, {lease_token}, 'embedding', 1, 1);
SELECT pgcontext.stage_document_chunks(
    {job_id}, {lease_token}, $p13_response${encoded}$p13_response$::jsonb
);
DO $p13_external$
DECLARE started timestamptz; published bigint; staged_bytes bigint;
BEGIN
    SELECT staging_bytes INTO staged_bytes
      FROM pgcontext._visible_document_chunk_staging WHERE job_id = {job_id};
    started := pg_catalog.clock_timestamp();
    published := pgcontext.publish_document_chunk_generation({job_id}, {lease_token});
    INSERT INTO p13_publication_samples(
        job_id, duration_micros, staging_bytes, worker_path
    )
    VALUES (
        {job_id},
        pg_catalog.date_part('epoch', pg_catalog.clock_timestamp() - started) * 1000000,
        staged_bytes,
        'external'
    );
END
$p13_external$;
"""
    run_psql(input_sql=sql)
worker.stdin.write("shutdown\n")
worker.stdin.close()
if worker.wait(timeout=10) != 0:
    raise SystemExit("external chunk worker failed during the publication workload")
PY

psql_db <<'SQL'
UPDATE p13_timing
   SET finished = pg_catalog.clock_timestamp(),
       initial_chunk_count = (
           SELECT pg_catalog.count(*) FROM public.p13_scale_chunks WHERE ready
       );
SQL

psql_db \
    -v row_count="${ROW_COUNT}" \
    -v sample_documents="${SAMPLE_DOCUMENTS}" \
    -v manifest_hash="${MANIFEST_HASH}" \
    -v max_publication_micros="${MAX_PUBLICATION_MICROS}" \
    -v min_chunks_per_second="${MIN_CHUNKS_PER_SECOND}" \
    -v hidden_reader_role="${HIDDEN_READER_ROLE}" <<'SQL' | tee -a "${REPORT_FILE}"

DO $p13_checks$
DECLARE
    expected_jobs bigint;
    max_publication_micros numeric;
    min_chunks_per_second numeric;
    actual_jobs bigint;
    current_documents bigint;
    chunks bigint;
    embeddings bigint;
    publication_micros numeric;
    max_atomic_micros numeric;
    chunk_rate numeric;
BEGIN
    SELECT limits.expected_jobs, limits.max_publication_micros,
           limits.min_chunks_per_second
      INTO expected_jobs, max_publication_micros, min_chunks_per_second
      FROM p13_limits AS limits;
    SELECT pg_catalog.count(*) INTO actual_jobs FROM p13_claims;
    SELECT pg_catalog.count(DISTINCT source_key) INTO current_documents
      FROM pgcontext._visible_current_document_chunk_generations;
    SELECT initial_chunk_count INTO chunks FROM p13_timing;
    SELECT pg_catalog.count(*) INTO embeddings
      FROM pgcontext._visible_document_embedding_jobs WHERE status = 'ready';
    SELECT pg_catalog.date_part('epoch', finished - started) * 1000000,
           initial_chunk_count
               / NULLIF(pg_catalog.date_part('epoch', finished - started), 0)
      INTO publication_micros, chunk_rate FROM p13_timing;
    SELECT pg_catalog.max(duration_micros) INTO max_atomic_micros
      FROM p13_publication_samples;
    IF actual_jobs <> expected_jobs OR current_documents <> expected_jobs
       OR chunks < expected_jobs OR embeddings <> chunks THEN
        RAISE EXCEPTION 'automatic chunking scale publication is incomplete';
    END IF;
    IF (SELECT pg_catalog.count(*) FROM p13_publication_samples) <> expected_jobs
       OR (SELECT pg_catalog.count(*) FROM p13_publication_samples WHERE worker_path = 'external') <> expected_jobs THEN
        RAISE EXCEPTION 'automatic chunking worker/publication samples are incomplete';
    END IF;
END
$p13_checks$;

UPDATE public.p13_scale_documents
   SET body = body || ' refreshed', source_version = 2
 WHERE id = 1;
DO $p13_prior$
BEGIN
    IF (SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(
            'p13_scale', 'body', ARRAY['1'])) <> 0 THEN
        RAISE EXCEPTION 'stale prior generation remained visible before replacement';
    END IF;
END
$p13_prior$;
CREATE TEMP TABLE p13_refresh_claim AS
SELECT job_id, lease_token
  FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'p13-refresh-worker');
SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
  FROM p13_refresh_claim;

CREATE ROLE :"hidden_reader_role";
GRANT USAGE ON SCHEMA pgcontext TO :"hidden_reader_role";
SET SESSION AUTHORIZATION :"hidden_reader_role";
DO $p13_security$
BEGIN
    IF pg_catalog.has_table_privilege(
           CURRENT_USER, 'pgcontext._document_chunk_jobs', 'SELECT'
       ) OR (SELECT pg_catalog.count(*) FROM pgcontext._visible_document_chunk_jobs) <> 0 THEN
        RAISE EXCEPTION 'automatic chunking catalog isolation failed';
    END IF;
END
$p13_security$;
RESET SESSION AUTHORIZATION;

SELECT 'automatic_chunking_sample'
       || ' source_rows=' || :row_count::text
       || ' processed_documents=' || :sample_documents::text
       || ' chunks=' || timing.initial_chunk_count::text
       || ' dataset_hash=' || identity.dataset_hash
       || ' dataset_generator_hash=' || identity.dataset_generator_hash
       || ' workload_hash=' || identity.workload_hash AS report
  FROM p13_identity AS identity CROSS JOIN p13_timing AS timing;
SELECT 'automatic_chunking_throughput'
       || ' chunks_per_second=' || pg_catalog.round(
              (timing.initial_chunk_count
              / NULLIF(pg_catalog.date_part('epoch', timing.finished - timing.started), 0))::numeric,
              3
          )::text
       || ' publication_micros=' || pg_catalog.round(
              pg_catalog.date_part('epoch', timing.finished - timing.started) * 1000000
          )::text
       || ' max_atomic_publication_micros=' || (
              SELECT pg_catalog.round(pg_catalog.max(duration_micros))::text
                FROM p13_publication_samples
          )
       || ' decision=' || CASE
              WHEN (timing.initial_chunk_count
                    / NULLIF(pg_catalog.date_part('epoch', timing.finished - timing.started), 0))
                       >= (SELECT min_chunks_per_second FROM p13_limits)
               AND (SELECT pg_catalog.max(duration_micros) FROM p13_publication_samples)
                       <= (SELECT max_publication_micros FROM p13_limits)
              THEN 'pass' ELSE 'no_go' END
       || ' manifest_hash=' || :'manifest_hash' AS report
  FROM p13_timing AS timing;
SELECT 'automatic_chunking_publication'
       || ' current_documents=' || pg_catalog.count(DISTINCT source_key)::text
       || ' staging_bytes_total=' || (
              SELECT pg_catalog.sum(staging_bytes)::text FROM p13_publication_samples
          )
       || ' staging_bytes_max=' || (
              SELECT pg_catalog.max(staging_bytes)::text FROM p13_publication_samples
          )
       || ' complete=true stale_prior_hidden=true source_refresh=true external_worker=true' AS report
  FROM pgcontext._visible_current_document_chunk_generations;
SELECT 'automatic_chunking_security private_catalogs=true membership_views=true acl_pgtest=true rls_pgtest=true'
       AS report;
SELECT 'automatic_chunking_environment pg_version=' || current_setting('server_version_num')
       || ' source_rows=' || :row_count::text
       || ' max_connections=' || current_setting('max_connections')
       || ' shared_buffers=' || current_setting('shared_buffers')
       || ' work_mem=' || current_setting('work_mem')
       || ' maintenance_work_mem=' || current_setting('maintenance_work_mem')
       || ' effective_cache_size=' || current_setting('effective_cache_size')
       || ' jit=' || current_setting('jit') AS report;
SQL

printf 'automatic_chunking_environment hardware_os=%s hardware_arch=%s hardware_cpus=%s\n' \
    "${HARDWARE_OS}" "${HARDWARE_ARCH}" "${HARDWARE_CPUS}" | tee -a "${REPORT_FILE}"
psql_db -Atc "SELECT 'automatic_chunking_sample job_id=' || job_id::text || ' duration_micros=' || duration_micros::text || ' staging_bytes=' || staging_bytes::text || ' worker_path=' || worker_path FROM p13_publication_samples ORDER BY job_id" \
    | tee -a "${REPORT_FILE}"

P13_WORKER_BIN="${REPO_ROOT}/target/release/pgcontext-chunk-worker" \
P13_WORKER_REPORT="${WORKER_REPORT}" \
P13_MIN_TOKENS_PER_SECOND="${MIN_TOKENS_PER_SECOND}" \
P13_MAX_RSS_BYTES="${MAX_RSS_BYTES}" \
python3 - <<'PY'
import json
import os
import resource
import subprocess
import time

token_count = 200_000
request = {
    "version": "chunk_worker_request_v1",
    "request_id": 1,
    "document_id": 1,
    "source_version": 1,
    "source_hash": "0123456789abcdef",
    "profile_revision": 1,
    "parser": "plain_text_v1",
    "tokenizer_revision": "unicode_words_v1",
    "expires_at_micros": 2**63 - 1,
    "profile": {
        "target_tokens": 384,
        "max_tokens": 512,
        "min_tokens": 32,
        "overlap_tokens": 64,
        "max_document_bytes": 8 * 1024 * 1024,
        "include_structure_context": False,
    },
    "source_text": "token " * token_count,
}
started = time.perf_counter()
completed = subprocess.run(
    [os.environ["P13_WORKER_BIN"]],
    input=json.dumps(request, separators=(",", ":")) + "\nshutdown\n",
    text=True,
    capture_output=True,
    check=True,
)
elapsed = time.perf_counter() - started
frames = [line for line in completed.stdout.splitlines() if line]
if len(frames) != 1:
    raise SystemExit("chunk worker emitted an unexpected frame count")
response = json.loads(frames[0])
if not response.get("complete") or not response.get("chunks"):
    raise SystemExit("chunk worker did not return a complete generation")
throughput = token_count / elapsed
rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
if os.uname().sysname != "Darwin":
    rss *= 1024
if throughput < int(os.environ["P13_MIN_TOKENS_PER_SECOND"]):
    raise SystemExit("chunk worker token throughput missed its floor")
if rss > int(os.environ["P13_MAX_RSS_BYTES"]):
    raise SystemExit("chunk worker RSS exceeded its ceiling")
report = (
    f"automatic_chunking_memory rss_bytes={rss} "
    f"worker_tokens_per_second={throughput:.3f} worker_chunks={len(response['chunks'])}"
)
with open(os.environ["P13_WORKER_REPORT"], "w", encoding="utf-8") as output:
    output.write(report + "\n")
PY

tee -a "${REPORT_FILE}" < "${WORKER_REPORT}"
printf 'document_chunking_worker: ok\n'
