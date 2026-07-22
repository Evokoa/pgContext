#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_late_interaction_ann}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.late_ann_docs (
    id bigint PRIMARY KEY,
    token_vectors vector[] NOT NULL
);

INSERT INTO public.late_ann_docs (id, token_vectors)
VALUES (10, ARRAY['[1,0]'::vector, '[0,1]'::vector]),
       (20, ARRAY['[0.8,0.1]'::vector, '[0.1,0.7]'::vector]),
       (30, ARRAY['[1,0]'::vector, '[1,0]'::vector]),
       (40, ARRAY['[0,0]'::vector]);

SELECT pgcontext.create_collection('late_ann_docs', 'public.late_ann_docs');
SELECT pgcontext.upsert_points('late_ann_docs', ARRAY['10', '20', '30', '40']);
SELECT pgcontext.register_late_interaction(
    'late_ann_docs',
    'public.late_ann_docs',
    'token_vectors'
);

DO $$
DECLARE
    actual_keys text[];
    actual_scores numeric[];
    deleted_count integer;
BEGIN
    WITH ranked AS (
        SELECT row_number() OVER () AS ord, source_key, score
          FROM pgcontext.search_late_interaction_ann(
              'late_ann_docs',
              ARRAY['[1,0]'::vector, '[0,1]'::vector],
              3,
              3
          )
    )
    SELECT array_agg(source_key ORDER BY ord),
           array_agg(round(score::numeric, 3) ORDER BY ord)
      INTO actual_keys, actual_scores
      FROM ranked;

    IF actual_keys <> ARRAY['10', '20', '30'] THEN
        RAISE EXCEPTION 'late-interaction ANN candidate keys mismatch: %',
            actual_keys;
    END IF;
    IF actual_scores <> ARRAY[2.000, 1.500, 1.000]::numeric[] THEN
        RAISE EXCEPTION 'late-interaction ANN rerank scores mismatch: %',
            actual_scores;
    END IF;

    UPDATE public.late_ann_docs
       SET token_vectors = ARRAY['[0,0]'::vector]
     WHERE id = 20;

    WITH ranked AS (
        SELECT row_number() OVER () AS ord, source_key, score
          FROM pgcontext.search_late_interaction_ann(
              'late_ann_docs',
              ARRAY['[1,0]'::vector, '[0,1]'::vector],
              10,
              3
          )
    )
    SELECT array_agg(source_key ORDER BY ord),
           array_agg(round(score::numeric, 3) ORDER BY ord)
      INTO actual_keys, actual_scores
      FROM ranked;

    IF actual_keys <> ARRAY['10', '30', '20'] THEN
        RAISE EXCEPTION 'late-interaction ANN source recheck keys mismatch: %',
            actual_keys;
    END IF;
    IF actual_scores <> ARRAY[2.000, 1.000, 0.000]::numeric[] THEN
        RAISE EXCEPTION 'late-interaction ANN source recheck scores mismatch: %',
            actual_scores;
    END IF;

    PERFORM pgcontext.delete_points('late_ann_docs', ARRAY['30']);

    WITH ranked AS (
        SELECT source_key
          FROM pgcontext.search_late_interaction_ann(
              'late_ann_docs',
              ARRAY['[1,0]'::vector, '[0,1]'::vector],
              10,
              4
          )
    )
    SELECT count(*) FILTER (WHERE source_key = '30'),
           array_agg(source_key ORDER BY source_key)
      INTO deleted_count, actual_keys
      FROM ranked;

    IF deleted_count <> 0 THEN
        RAISE EXCEPTION 'late-interaction ANN returned deleted source key 30';
    END IF;
    IF actual_keys <> ARRAY['10', '20', '40'] THEN
        RAISE EXCEPTION 'late-interaction ANN deleted recheck keys mismatch: %',
            actual_keys;
    END IF;
END
$$;

CREATE TABLE public.late_ann_budget_docs (
    id bigint PRIMARY KEY,
    token_vectors vector[] NOT NULL
);

INSERT INTO public.late_ann_budget_docs (id, token_vectors)
VALUES (10, array_fill('[1,0]'::vector, ARRAY[1000]));

SELECT pgcontext.create_collection(
    'late_ann_budget_docs',
    'public.late_ann_budget_docs'
);
SELECT pgcontext.upsert_points('late_ann_budget_docs', ARRAY['10']);
SELECT pgcontext.register_late_interaction(
    'late_ann_budget_docs',
    'public.late_ann_budget_docs',
    'token_vectors'
);

DO $$
BEGIN
    PERFORM *
      FROM pgcontext.search_late_interaction_ann(
          'late_ann_budget_docs',
          array_fill('[1,0]'::vector, ARRAY[1001]),
          1000,
          1
      );
    RAISE EXCEPTION 'late-interaction ANN budget rejection did not fire';
EXCEPTION
    WHEN SQLSTATE '54000' THEN
        IF SQLERRM <> 'late interaction comparison budget exceeded: 1001000 > 1000000' THEN
            RAISE EXCEPTION 'unexpected late-interaction ANN budget message: %',
                SQLERRM;
        END IF;
END
$$;

CHECKPOINT;
SQL

cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"

restart_keys="$(psql_db -At <<'SQL' | tail -n 1
SELECT pg_catalog.string_agg(source_key, ',' ORDER BY score DESC, point_id)
  FROM pgcontext.search_late_interaction_ann(
      'late_ann_docs',
      ARRAY['[1,0]'::vector, '[0,1]'::vector],
      10,
      4
  );
SQL
)"
if [[ "${restart_keys}" != "10,20,40" ]]; then
    echo "late-interaction owned generation failed restart recheck: ${restart_keys}" >&2
    exit 1
fi

psql_db <<'SQL'
SELECT pgcontext.repair_late_interaction('late_ann_docs', 2);
DO $$
DECLARE
    deleted_point_count bigint;
BEGIN
    SELECT pg_catalog.count(*)
      INTO deleted_point_count
      FROM pgcontext.search_late_interaction_ann(
          'late_ann_docs',
          ARRAY['[1,0]'::vector, '[0,1]'::vector],
          10,
          4
      )
     WHERE source_key = '30';
    IF deleted_point_count <> 0 THEN
        RAISE EXCEPTION 'late-interaction repair reactivated deleted source key 30';
    END IF;
END
$$;
SQL

storage_measurement="$(psql_db -At <<'SQL' | tail -n 1
SELECT pg_catalog.concat_ws(
           '|',
           registrations.point_count,
           registrations.token_count,
           coalesce(pg_catalog.sum(pg_catalog.pg_column_size(tokens)), 0),
           pg_catalog.pg_relation_size(registrations.hnsw_index_oid)
       )
  FROM pgcontext._collection_late_interaction AS registrations
  JOIN pgcontext._collections AS collections USING (collection_id)
  LEFT JOIN pgcontext._collection_late_interaction_tokens AS tokens
    USING (collection_id)
 WHERE collections.collection_name = 'late_ann_docs'
 GROUP BY registrations.point_count,
          registrations.token_count,
          registrations.hnsw_index_oid;
SQL
)"

wal_bytes="$(psql_db -At <<'SQL' | tail -n 1
CREATE TEMP TABLE late_interaction_measurement_lsn AS
SELECT pg_catalog.pg_current_wal_insert_lsn() AS before_lsn;
UPDATE public.late_ann_docs
   SET token_vectors = ARRAY['[0.25,0.75]'::vector, '[0.75,0.25]'::vector]
 WHERE id = 40;
SELECT pg_catalog.pg_wal_lsn_diff(
           pg_catalog.pg_current_wal_insert_lsn(),
           before_lsn
       )::bigint
  FROM late_interaction_measurement_lsn;
SQL
)"

printf 'late_interaction_ann_candidates_deduped\n'
printf 'late_interaction_ann_exact_rerank_scores\n'
printf 'late_interaction_ann_source_recheck\n'
printf 'late_interaction_ann_deleted_recheck\n'
printf 'late_interaction_ann_budget_rejected\n'
printf 'late_interaction_owned_restart_recheck\n'
printf 'late_interaction_owned_repair_preserves_tombstones\n'
printf 'late_interaction_owned_storage: points|tokens|token_row_bytes|hnsw_index_bytes=%s\n' "${storage_measurement}"
printf 'late_interaction_owned_update_wal_bytes: %s\n' "${wal_bytes}"
