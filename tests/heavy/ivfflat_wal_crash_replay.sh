#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_ivfflat_wal_crash}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ -z "${PGRX_DATA_DIR:-}" ]]; then
    echo "PGRX_DATA_DIR must identify the exact isolated pgrx data directory" >&2
    exit 2
fi

PG_CTL="$(pg_bin pg_ctl)"
start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.ivf_crash_docs (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(4) NOT NULL
);
INSERT INTO public.ivf_crash_docs
SELECT id, format('[%s,%s,%s,%s]', id%31, id%29, id%23, id%19)::pgcontext.vector
  FROM generate_series(1, 20000) id;

CREATE TABLE public.ivf_crash_wide (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(3000) NOT NULL
);
CREATE INDEX ivf_crash_wide_idx
    ON public.ivf_crash_wide USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops) WITH (lists = 2);
CHECKPOINT;

SET maintenance_work_mem = '2MB';
SET pgcontext.ivfflat_build_parallel_workers = 4;
CREATE INDEX ivf_crash_docs_idx
    ON public.ivf_crash_docs USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 64, quantization = sq8);
UPDATE public.ivf_crash_docs SET embedding = '[0,0,0,0]' WHERE id = 7;
DELETE FROM public.ivf_crash_docs WHERE id IN (8,9,10);
INSERT INTO public.ivf_crash_wide
SELECT 1, ('[' || string_agg('0', ',') || ']')::pgcontext.vector
  FROM generate_series(1, 3000);
SQL

"${PG_CTL}" -D "${PGRX_DATA_DIR}" stop -m immediate
cargo pgrx start "${PG_VERSION}"

psql_db <<'SQL'
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.ivfflat_iterative_scan = strict_order;
SET pgcontext.ivfflat_max_probes = 64;

DO $$
DECLARE
    nearest bigint;
    wide_nearest bigint;
BEGIN
    IF (pgcontext.ivfflat_index_info('public.ivf_crash_docs_idx'::regclass)->>'verified')::boolean IS NOT TRUE THEN
        RAISE EXCEPTION 'IVFFlat generation failed verification after crash replay';
    END IF;
    SELECT id INTO nearest
      FROM public.ivf_crash_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0,0,0]'::pgcontext.vector
     LIMIT 1;
    IF nearest <> 7 THEN RAISE EXCEPTION 'unexpected replayed nearest row: %', nearest; END IF;
    SELECT id INTO wide_nearest
      FROM public.ivf_crash_wide
     ORDER BY embedding OPERATOR(pgcontext.<->)
              (SELECT ('[' || string_agg('0', ',') || ']')::pgcontext.vector
                 FROM generate_series(1, 3000))
     LIMIT 1;
    IF wide_nearest <> 1 THEN RAISE EXCEPTION 'multi-page delta did not replay'; END IF;
END
$$;
SQL

printf 'ivfflat_build_wal_replay_verified\n'
printf 'ivfflat_delta_wal_replay_verified\n'
printf 'ivfflat_multi_page_delta_replay_verified\n'
