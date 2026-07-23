#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_named_sparse_ann_lifecycle}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

validate_sparse_ann() {
    local phase="$1"
    psql_db <<'SQL'
DO $$
DECLARE
    ann_keys text[];
    exact_keys text[];
    work record;
BEGIN
    SELECT array_agg(source_key ORDER BY score, point_id)
      INTO ann_keys
      FROM pgcontext.search_sparse(
           'named_sparse_restart_docs',
           'lexical',
           pgcontext.sparsevec('{1:256,2:3}/8'),
           10
      );

    SELECT array_agg(source_key ORDER BY score, point_id)
      INTO exact_keys
      FROM (
           SELECT points.point_id,
                  points.source_key,
                  pgcontext.sparsevec_l2_distance(
                      source.lexical,
                      pgcontext.sparsevec('{1:256,2:3}/8')
                  ) AS score
             FROM pgcontext._visible_collection_points AS points
             JOIN public.named_sparse_restart_docs AS source
               ON source.id::text = points.source_key
            WHERE points.deleted_at IS NULL
              AND points.collection_id = (
                  SELECT collection_id
                    FROM pgcontext._collection_acl
                   WHERE collection_name = 'named_sparse_restart_docs'
              )
            ORDER BY score, points.point_id
            LIMIT 10
      ) AS exact;

    IF ann_keys IS DISTINCT FROM exact_keys THEN
        RAISE EXCEPTION 'named sparse ANN mismatch: ANN %, exact %', ann_keys, exact_keys;
    END IF;

    SELECT * INTO work
      FROM pgcontext.explain_sparse(
           'named_sparse_restart_docs',
           'lexical',
           pgcontext.sparsevec('{1:256,2:3}/8'),
           10
      );
    IF work.strategy <> 'hnsw' THEN
        RAISE EXCEPTION 'expected named sparse HNSW strategy, got %', work.strategy;
    END IF;
    IF work.scored_count >= work.active_points THEN
        RAISE EXCEPTION 'named sparse HNSW did not bound scored work: % >= %',
            work.scored_count, work.active_points;
    END IF;
    IF work.candidate_count <> work.recheck_count OR work.candidate_count <= 0 THEN
        RAISE EXCEPTION 'named sparse candidates were not exactly rechecked: candidates %, rechecks %',
            work.candidate_count, work.recheck_count;
    END IF;
END
$$;
SQL
    printf 'named_sparse_ann_exact_oracle: %s\n' "${phase}"
    printf 'named_sparse_ann_bounded_work: %s\n' "${phase}"

    psql_db <<'SQL'
DO $$
DECLARE
    query_no integer;
    query_vector sparsevec;
    exact_keys text[];
    ann_keys text[];
    query_matches integer;
    matched integer := 0;
    expected integer := 0;
    recall double precision;
BEGIN
    FOR query_no IN 1..32 LOOP
        query_vector := format(
            '{1:%s,2:%s,4:%s,7:%s}/8',
            ((query_no * 47) % 512) + 1,
            (query_no * 13) % 29,
            (query_no * 17) % 31,
            (query_no * 19) % 37
        )::sparsevec;

        SELECT array_agg(source_key ORDER BY score, point_id)
          INTO ann_keys
          FROM pgcontext.search_sparse(
               'named_sparse_restart_docs', 'lexical', query_vector, 10
          );
        SELECT array_agg(source_key ORDER BY score, point_id)
          INTO exact_keys
          FROM (
               SELECT points.point_id,
                      points.source_key,
                      pgcontext.sparsevec_l2_distance(source.lexical, query_vector) AS score
                 FROM pgcontext._visible_collection_points AS points
                 JOIN public.named_sparse_restart_docs AS source
                   ON source.id::text = points.source_key
                WHERE points.deleted_at IS NULL
                  AND points.collection_id = (
                      SELECT collection_id
                        FROM pgcontext._collection_acl
                       WHERE collection_name = 'named_sparse_restart_docs'
                  )
                ORDER BY score, points.point_id
                LIMIT 10
          ) AS exact;
        SELECT count(*)
          INTO STRICT query_matches
          FROM unnest(ann_keys) AS ann(key)
          JOIN unnest(exact_keys) AS exact(key) USING (key);
        matched := matched + query_matches;
        expected := expected + cardinality(exact_keys);
    END LOOP;
    recall := matched::double precision / expected::double precision;
    IF recall < 0.95 THEN
        RAISE EXCEPTION 'named sparse recall@10 below threshold: % < 0.95 (% / %)',
            recall, matched, expected;
    END IF;
    RAISE NOTICE 'named sparse recall@10: % (% / %)', recall, matched, expected;
END
$$;
SQL
    printf 'named_sparse_ann_recall_threshold: %s\n' "${phase}"
}

validate_hot_row() {
    local phase="$1"
    psql_db <<'SQL'
DO $$
DECLARE
    nearest_key text;
BEGIN
    SELECT source_key
      INTO nearest_key
      FROM pgcontext.search_sparse(
           'named_sparse_restart_docs',
           'lexical',
           pgcontext.sparsevec('{1:257,2:25,4:9,7:35}/8'),
           1
      );
    IF nearest_key IS DISTINCT FROM '257' THEN
        RAISE EXCEPTION 'HOT-updated sparse row disappeared: expected 257, got %', nearest_key;
    END IF;
END
$$;
SQL
    printf 'named_sparse_ann_hot_visible: %s\n' "${phase}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.named_sparse_restart_docs (
    id bigint PRIMARY KEY,
    lexical sparsevec NOT NULL,
    note text NOT NULL DEFAULT 'before'
) WITH (fillfactor = 50);

INSERT INTO public.named_sparse_restart_docs (id, lexical)
SELECT value,
       format(
           '{1:%s,2:%s,4:%s,7:%s}/8',
           value, value % 29, value % 31, value % 37
       )::sparsevec
  FROM generate_series(1, 512) AS value;

SELECT pgcontext.create_collection(
    'named_sparse_restart_docs', 'public.named_sparse_restart_docs'
);
SELECT pgcontext.register_sparse_vector(
    'named_sparse_restart_docs', 'lexical', 'lexical', 8, 'l2'
);
SELECT pgcontext.upsert_points(
    'named_sparse_restart_docs',
    ARRAY(SELECT value::text FROM generate_series(1, 512) AS value)
);

CREATE INDEX named_sparse_restart_docs_hnsw
    ON public.named_sparse_restart_docs USING pgcontext_hnsw
    (lexical pgcontext.sparsevec_hnsw_ops);
SELECT pgcontext.attach_sparse_hnsw_index(
    'named_sparse_restart_docs', 'lexical',
    'public.named_sparse_restart_docs_hnsw'
);

UPDATE public.named_sparse_restart_docs
   SET lexical = pgcontext.sparsevec('{1:256,2:3}/8')
 WHERE id = 512;
UPDATE public.named_sparse_restart_docs
   SET note = 'after'
 WHERE id = 257;
DELETE FROM public.named_sparse_restart_docs WHERE id = 256;
SELECT pgcontext.delete_points('named_sparse_restart_docs', ARRAY['255']);
SQL

validate_sparse_ann "before_vacuum_reindex_delta"
validate_hot_row "before_vacuum"
psql_db -c "VACUUM (ANALYZE) public.named_sparse_restart_docs"
validate_hot_row "after_vacuum"
psql_db -c "REINDEX INDEX public.named_sparse_restart_docs_hnsw"
printf 'named_sparse_ann_vacuum_reindex\n'

validate_sparse_ann "before_restart"
cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"
validate_sparse_ann "after_restart"
printf 'named_sparse_ann_restart_complete\n'
