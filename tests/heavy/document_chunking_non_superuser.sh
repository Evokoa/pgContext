#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_document_chunking_non_superuser}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

SOURCE_OWNER="p13_live_source_owner"
APP_ROLE="p13_live_app"

start_and_install_extension
reset_database
drop_role_if_exists "${APP_ROLE}"
drop_role_if_exists "${SOURCE_OWNER}"

psql_db -v source_owner="${SOURCE_OWNER}" -v app_role="${APP_ROLE}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE ROLE :"source_owner";
CREATE ROLE :"app_role";
GRANT USAGE ON SCHEMA pgcontext TO :"app_role";
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO :"app_role";
GRANT CREATE ON SCHEMA public TO :"app_role";

CREATE TABLE public.p13_live_docs (
    id text PRIMARY KEY,
    body text NOT NULL,
    source_version bigint NOT NULL
);
INSERT INTO public.p13_live_docs VALUES ('a','alpha beta gamma delta',1);
ALTER TABLE public.p13_live_docs OWNER TO :"source_owner";
GRANT SELECT ON public.p13_live_docs TO :"app_role";

SET SESSION AUTHORIZATION :"app_role";
SELECT pgcontext.create_collection('p13_live','public.p13_live_docs');
SELECT pgcontext.create_document_chunk_projection('public.p13_live_chunks');
SELECT pgcontext.register_chunking_profile(
    'p13_live_a','plain_text_v1',4,4,1,0,8388608,false
);
SELECT pgcontext.register_chunking_profile(
    'p13_live_b','plain_text_v1',2,2,1,0,8388608,false
);
SELECT pgcontext.register_document_source(
    'p13_live','body','body','source_version',
    'public.p13_live_chunks','p13_live_a'
);
SELECT pgcontext.enqueue_document_chunking('p13_live','body',ARRAY['a']);
CREATE TEMP TABLE p13_live_claim AS
SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'live-app-worker');
SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
  FROM p13_live_claim;

DO $verify_publication$
BEGIN
    IF (SELECT pg_catalog.count(*)
          FROM pgcontext.current_document_chunks('p13_live','body',ARRAY['a'])) <> 1 THEN
        RAISE EXCEPTION 'non-superuser publication did not become current';
    END IF;
END
$verify_publication$;

SELECT pgcontext.prepare_chunking_profile_alias('p13_live_a','p13_live_b');
SELECT pgcontext.promote_chunking_profile_alias('p13_live_a','p13_live_b');
DO $verify_fallback$
BEGIN
    IF (SELECT pg_catalog.count(*)
          FROM pgcontext.current_document_chunks('p13_live','body',ARRAY['a'])) <> 1 THEN
        RAISE EXCEPTION 'retained fallback disappeared after promotion';
    END IF;
END
$verify_fallback$;
RESET SESSION AUTHORIZATION;

SELECT 'document_chunking_non_superuser_passed' AS result;
SQL
