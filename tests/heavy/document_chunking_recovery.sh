#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_document_chunking_recovery}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.p13_recovery_docs (
    id bigint PRIMARY KEY,
    body text NOT NULL,
    source_version bigint NOT NULL
);
INSERT INTO public.p13_recovery_docs VALUES
    (1, 'restart and lease takeover preserve authoritative publication', 1);
SELECT pgcontext.create_collection('p13_recovery', 'public.p13_recovery_docs');
SELECT pgcontext.create_document_chunk_projection('public.p13_recovery_chunks');
SELECT pgcontext.register_chunking_profile(
    'p13_recovery_profile', 'plain_text_v1', 64, 96, 8, 8, 8388608, false
);
SELECT pgcontext.register_document_source(
    'p13_recovery', 'body', 'body', 'source_version',
    'public.p13_recovery_chunks', 'p13_recovery_profile'
);
SELECT pgcontext.enqueue_document_chunking('p13_recovery', 'body', ARRAY['1']);
CREATE TABLE public.p13_abandoned_claim AS
SELECT job_id, lease_token
  FROM pgcontext.claim_document_chunk_jobs(1, 1000, 'p13-crashed-worker');
SQL

cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"

for _ in {1..30}; do
    if psql_db -Atc "SELECT lease_expires_at < pg_catalog.clock_timestamp() FROM pgcontext._document_chunk_jobs LIMIT 1" | grep -qx 't'; then
        break
    fi
    sleep 0.1
done
psql_db -Atc "SELECT lease_expires_at < pg_catalog.clock_timestamp() FROM pgcontext._document_chunk_jobs LIMIT 1" | grep -qx 't'

psql_db <<'SQL'
DO $p13_takeover$
DECLARE
    old_token bigint;
    new_token bigint;
    claimed_job bigint;
BEGIN
    SELECT lease_token INTO old_token FROM public.p13_abandoned_claim;
    SELECT job_id, lease_token
      INTO claimed_job, new_token
      FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'p13-recovery-worker');
    IF claimed_job IS NULL OR new_token IS NULL OR new_token = old_token THEN
        RAISE EXCEPTION 'expired automatic-chunking lease did not fence the crashed worker';
    END IF;
    PERFORM pgcontext.fake_process_document_chunk_job(claimed_job, new_token);
END
$p13_takeover$;
SQL

cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"

psql_db <<'SQL'
DO $p13_ready$
DECLARE
    current_count bigint;
    ready_jobs bigint;
BEGIN
    SELECT count(*) INTO current_count
      FROM pgcontext.current_document_chunks('p13_recovery', 'body', ARRAY['1']);
    SELECT count(*) INTO ready_jobs
      FROM pgcontext._visible_document_chunk_jobs
     WHERE status = 'ready';
    IF current_count <> 1 OR ready_jobs <> 1 THEN
        RAISE EXCEPTION 'automatic-chunking publication did not survive restart';
    END IF;
END
$p13_ready$;
SQL

printf 'document_chunking_restart_takeover: passed\n'
