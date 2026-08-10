#!/usr/bin/env bash
set -euo pipefail

# Reproducible PostgreSQL-native lexical lifecycle smoke.
#
# A small collection proves the exact path matches a direct PostgreSQL oracle
# and that the GIN, GiST, and post-drop fallback paths return the same ranked
# answer. A larger collection proves the indexed path still serves a corpus the
# exact path cannot finish inside the default elapsed budget, that the canonical
# index expression is planner-matchable, and that the exact fallback fails
# closed there instead of silently truncating.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_indexed_lexical_hybrid}"
ROW_COUNT="${ROW_COUNT:-20000}"
SMALL_ROW_COUNT="${SMALL_ROW_COUNT:-300}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[0-9]+$ ]] || (( ROW_COUNT < 2000 )); then
    echo "ROW_COUNT must be an integer of at least 2000" >&2
    exit 2
fi
if [[ ! "${SMALL_ROW_COUNT}" =~ ^[0-9]+$ ]] || (( SMALL_ROW_COUNT < 100 )); then
    echo "SMALL_ROW_COUNT must be an integer of at least 100" >&2
    exit 2
fi

assert_equal() {
    local label="$1"
    local expected="$2"
    local actual="$3"
    if [[ "${expected}" != "${actual}" ]]; then
        echo "${label}: expected '${expected}', got '${actual}'" >&2
        exit 1
    fi
    echo "${label}: ok"
}

assert_nonempty() {
    local label="$1"
    local value="$2"
    if [[ -z "${value}" ]]; then
        echo "${label}: returned no rows" >&2
        exit 1
    fi
    echo "${label}: ok"
}

expect_sql_error() {
    local label="$1"
    local statement="$2"
    local expected="$3"
    local suffix="$4"
    local log_file="${HEAVY_TMPDIR}/${DBNAME}_${suffix}.log"
    rm -f "${log_file}"
    if psql_db -c "${statement}" >/dev/null 2>"${log_file}"; then
        echo "${label}: unexpectedly succeeded" >&2
        exit 1
    fi
    if ! grep -Fqi "${expected}" "${log_file}"; then
        echo "${label}: failed for an unexpected reason" >&2
        cat "${log_file}" >&2
        exit 1
    fi
    echo "${label}: ok"
}

scalar() {
    psql_db -tAc "$1" | tr -d '[:space:]'
}

lexical_keys() {
    local collection="$1"
    local plan="$2"
    psql_db -tAc "
        SELECT string_agg(source_key, ',' ORDER BY score DESC, point_id ASC)
          FROM pgcontext.execute_query('${collection}', ${plan})
    " | tr -d '[:space:]'
}

seed_collection() {
    local collection="$1"
    local rows="$2"
    psql_db <<SQL
CREATE TABLE public.${collection} (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    meta jsonb NOT NULL
);

INSERT INTO public.${collection} (id, embedding, title, body, meta)
SELECT id,
       ARRAY[(id % 7)::real, (id % 11)::real]::vector,
       CASE WHEN id % 5 = 0 THEN 'postgres storage internals ' || id
            ELSE 'unrelated heading ' || id
       END,
       CASE WHEN id % 997 = 0 THEN 'rarissimo sentinel marker ' || id
            WHEN id % 3 = 0 THEN 'the postgres storage engine writes pages ' || id
            WHEN id % 3 = 1 THEN 'rust programs call postgres directly ' || id
            ELSE 'entirely unrelated filler content ' || id
       END,
       jsonb_build_object('tags', jsonb_build_object('topic',
           CASE WHEN id % 3 = 0 THEN 'database' ELSE 'other' END))
  FROM pg_catalog.generate_series(1, ${rows}) AS id;

SELECT pgcontext.create_collection('${collection}', 'public.${collection}');
SELECT pgcontext.register_vector('${collection}', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.backfill_points('${collection}', ${rows});

SELECT pgcontext.register_lexical_source(
    '${collection}', 'article', ARRAY['title', 'body'],
    'pg_catalog.english', ARRAY['A', 'D'], NULL, 'ts_rank_cd', 0
);
SELECT pgcontext.register_lexical_source(
    '${collection}', 'topic', ARRAY['meta'], 'pg_catalog.simple',
    ARRAY['B'], ARRAY['tags.topic']
);
ANALYZE public.${collection};
SQL
}

start_and_install_extension
reset_database

psql_db -c "CREATE EXTENSION pgcontext" >/dev/null
seed_collection lexical_small "${SMALL_ROW_COUNT}" >/dev/null
seed_collection lexical_docs "${ROW_COUNT}" >/dev/null

plan_plain="pgcontext.query_lexical('article', jsonb_build_object('form','plain','text','postgres storage'), NULL, 25)"
plan_phrase="pgcontext.query_lexical('article', jsonb_build_object('form','phrase','text','postgres storage'), NULL, 25)"
plan_boolean="pgcontext.query_lexical('article', jsonb_build_object('form','boolean','operator','and','clauses', jsonb_build_array(jsonb_build_object('form','plain','text','postgres'), jsonb_build_object('form','boolean','operator','not','clauses', jsonb_build_array(jsonb_build_object('form','plain','text','rust'))))), NULL, 25)"
plan_weighted="pgcontext.query_lexical('article', jsonb_build_object('form','weight_restricted','weights', jsonb_build_array('a'), 'query', jsonb_build_object('form','plain','text','internals')), NULL, 25)"
plan_prefix="pgcontext.query_lexical('article', jsonb_build_object('form','prefix','term','postgr'), NULL, 25)"
plan_topic="pgcontext.query_lexical('topic', jsonb_build_object('form','plain','text','database'), NULL, 25)"
# Selective query: the shape an attached index exists to serve.
plan_selective="pgcontext.query_lexical('article', jsonb_build_object('form','plain','text','rarissimo sentinel'), NULL, 25)"

# ---------------------------------------------------------------------------
# Small collection: exact oracle parity, then index parity.
# ---------------------------------------------------------------------------

oracle_plain="$(psql_db -tAc "
    WITH document AS (
        SELECT points.point_id,
               points.source_key,
               pg_catalog.setweight(
                   pg_catalog.to_tsvector(
                       '\"pg_catalog\".\"english\"'::pg_catalog.regconfig,
                       coalesce(source.title::text, ''::text)
                   ), 'A'
               ) OPERATOR(pg_catalog.||)
               pg_catalog.setweight(
                   pg_catalog.to_tsvector(
                       '\"pg_catalog\".\"english\"'::pg_catalog.regconfig,
                       coalesce(source.body::text, ''::text)
                   ), 'D'
               ) AS vector
          FROM pgcontext._visible_collection_points AS points
          JOIN pgcontext._collection_acl AS acl
            ON acl.collection_id = points.collection_id
          JOIN public.lexical_small AS source
            ON source.id::text = points.source_key
         WHERE acl.collection_name = 'lexical_small'
           AND points.deleted_at IS NULL
    ),
    ranked AS (
        SELECT document.point_id,
               document.source_key,
               pg_catalog.ts_rank_cd(
                   ARRAY[0.1, 0.2, 0.4, 1.0]::real[],
                   document.vector,
                   pg_catalog.plainto_tsquery(
                       '\"pg_catalog\".\"english\"'::pg_catalog.regconfig, 'postgres storage'
                   ),
                   0
               )::double precision AS score
          FROM document
         WHERE document.vector OPERATOR(pg_catalog.@@) pg_catalog.plainto_tsquery(
                   '\"pg_catalog\".\"english\"'::pg_catalog.regconfig, 'postgres storage'
               )
         ORDER BY score DESC, document.point_id ASC
         LIMIT 25
    )
    SELECT string_agg(source_key, ',' ORDER BY score DESC, point_id ASC) FROM ranked
" | tr -d '[:space:]')"

exact_plain="$(lexical_keys lexical_small "${plan_plain}")"
assert_equal "exact lexical matches the direct PostgreSQL oracle" "${oracle_plain}" "${exact_plain}"

exact_phrase="$(lexical_keys lexical_small "${plan_phrase}")"
exact_boolean="$(lexical_keys lexical_small "${plan_boolean}")"
exact_weighted="$(lexical_keys lexical_small "${plan_weighted}")"
exact_prefix="$(lexical_keys lexical_small "${plan_prefix}")"
exact_topic="$(lexical_keys lexical_small "${plan_topic}")"

assert_nonempty "exact phrase form returns rows" "${exact_phrase}"
assert_nonempty "exact boolean form returns rows" "${exact_boolean}"
assert_nonempty "exact weight-restricted form returns rows" "${exact_weighted}"
assert_nonempty "exact prefix form returns rows" "${exact_prefix}"
assert_nonempty "exact JSON-path source returns rows" "${exact_topic}"

psql_db <<'SQL' >/dev/null
CREATE INDEX lexical_small_wrong_document_gin
    ON public.lexical_small USING gin ((
        pg_catalog.setweight(
            pg_catalog.to_tsvector(
                'pg_catalog.simple'::pg_catalog.regconfig,
                coalesce(title::text, ''::text)
            ),
            'A'
        ) OPERATOR(pg_catalog.||) pg_catalog.setweight(
            pg_catalog.to_tsvector(
                'pg_catalog.simple'::pg_catalog.regconfig,
                coalesce(body::text, ''::text)
            ),
            'D'
        )
    ));
SQL
expect_sql_error "an unrelated same-column lexical expression cannot attach" \
    "SELECT pgcontext.attach_lexical_index(
         'lexical_small', 'article', 'lexical_small_wrong_document_gin'
     )" \
    "lexical index does not match the registered document expression" \
    "wrong_lexical_expression"
psql_db -c "DROP INDEX public.lexical_small_wrong_document_gin" >/dev/null

small_index="$(scalar "SELECT pgcontext.create_lexical_index('lexical_small', 'article')")"
assert_nonempty "lexical index creation returns an index name" "${small_index}"
psql_db -c "ANALYZE public.lexical_small" >/dev/null

assert_equal "GIN index attaches to the registered lexical source" "gin" \
    "$(scalar "SELECT index_am_name FROM pgcontext._visible_collection_lexical_sources
                WHERE source_name = 'article'
                  AND collection_id = (SELECT collection_id FROM pgcontext._collection_acl
                                        WHERE collection_name = 'lexical_small')")"

assert_equal "GIN lexical path matches the exact path (plain)" \
    "${exact_plain}" "$(lexical_keys lexical_small "${plan_plain}")"
assert_equal "GIN lexical path matches the exact path (phrase)" \
    "${exact_phrase}" "$(lexical_keys lexical_small "${plan_phrase}")"
assert_equal "GIN lexical path matches the exact path (boolean)" \
    "${exact_boolean}" "$(lexical_keys lexical_small "${plan_boolean}")"
assert_equal "GIN lexical path matches the exact path (weight restricted)" \
    "${exact_weighted}" "$(lexical_keys lexical_small "${plan_weighted}")"
assert_equal "GIN lexical path matches the exact path (prefix)" \
    "${exact_prefix}" "$(lexical_keys lexical_small "${plan_prefix}")"

headline_rows="$(scalar "
    SELECT count(*)
      FROM pgcontext.lexical_headline(
          'lexical_small', 'article',
          (SELECT pg_catalog.array_agg(point_id)
             FROM (SELECT points.point_id
                     FROM pgcontext._visible_collection_points AS points
                     JOIN pgcontext._collection_acl AS acl
                       ON acl.collection_id = points.collection_id
                    WHERE acl.collection_name = 'lexical_small'
                      AND points.deleted_at IS NULL
                    ORDER BY points.point_id
                    LIMIT 50) AS selected),
          jsonb_build_object('form','plain','text','postgres')
      )")"
assert_equal "bounded headline hydration returns one row per point" "50" "${headline_rows}"

headline_marks_matches="$(scalar "
    SELECT bool_or(headline LIKE '%<b>%')
      FROM pgcontext.lexical_headline(
          'lexical_small', 'article',
          (SELECT pg_catalog.array_agg(point_id)
             FROM (SELECT points.point_id
                     FROM pgcontext._visible_collection_points AS points
                     JOIN pgcontext._collection_acl AS acl
                       ON acl.collection_id = points.collection_id
                    WHERE acl.collection_name = 'lexical_small'
                      AND points.deleted_at IS NULL
                    ORDER BY points.point_id
                    LIMIT 50) AS selected),
          jsonb_build_object('form','plain','text','postgres')
      )")"
assert_equal "headline output carries native ts_headline markup" "t" "${headline_marks_matches}"

fusion_rows="$(scalar "
    SELECT count(*)
      FROM pgcontext.query('lexical_small', '[0,0]'::vector, 'postgres storage', 'article', 20)")"
assert_equal "dense plus lexical fusion returns fused rows" "20" "${fusion_rows}"

psql_db -c "VACUUM FULL public.lexical_small" >/dev/null
refreshed="$(scalar "SELECT pgcontext.refresh_lexical_catalog('lexical_small')")"
if [[ "${refreshed}" == "0" ]]; then
    echo "catalog refresh reported no refreshed rows" >&2
    exit 1
fi
psql_db -c "ANALYZE public.lexical_small" >/dev/null
assert_equal "lexical results survive a table rewrite and OID refresh" \
    "${exact_plain}" "$(lexical_keys lexical_small "${plan_plain}")"

psql_db -c "SELECT pgcontext.detach_lexical_index('lexical_small', 'article')" >/dev/null
psql_db -c "DROP INDEX public.${small_index}" >/dev/null
gist_index="$(scalar "SELECT pgcontext.create_lexical_index('lexical_small', 'article', 'gist')")"
psql_db -c "ANALYZE public.lexical_small" >/dev/null
assert_equal "GiST lexical serving reports lossy candidates" "t" \
    "$(scalar "SELECT index_is_lossy FROM pgcontext._visible_collection_lexical_sources
                WHERE source_name = 'article'
                  AND collection_id = (SELECT collection_id FROM pgcontext._collection_acl
                                        WHERE collection_name = 'lexical_small')")"
assert_equal "GiST lexical path matches the exact path" \
    "${exact_plain}" "$(lexical_keys lexical_small "${plan_plain}")"

psql_db -c "DROP INDEX public.${gist_index}" >/dev/null
assert_equal "dropping the attached index restores the complete exact fallback" \
    "${exact_plain}" "$(lexical_keys lexical_small "${plan_plain}")"

# ---------------------------------------------------------------------------
# Large collection: the exact fallback fails closed, the indexed path serves.
# ---------------------------------------------------------------------------

expect_fails_closed() {
    local label="$1"
    local statement="$2"
    local log_file="${HEAVY_TMPDIR}/${DBNAME}_$3.log"
    rm -f "${log_file}"
    if psql_db -tAc "${statement}" >/dev/null 2>"${log_file}"; then
        echo "${label}: unexpectedly returned a complete result" >&2
        exit 1
    fi
    if ! grep -qiE "statement timeout|work budget" "${log_file}"; then
        echo "${label}: failed for an unexpected reason" >&2
        cat "${log_file}" >&2
        exit 1
    fi
    echo "${label}: ok"
}

# The exact fallback is O(corpus): it recomputes `to_tsvector` for every visible
# row. `query_timeout_ms` can only lower the elapsed budget, never raise it, so on
# a corpus this size the exact path cannot finish inside the 500 ms default. That
# is the contract, not a defect — it must fail closed rather than return a
# silently truncated answer. Exact-versus-indexed rank parity is proven on the
# small collection above, where the exact path completes deterministically.
expect_fails_closed "large-corpus exact fallback fails closed instead of truncating" \
    "SELECT count(*) FROM pgcontext.execute_query('lexical_docs', ${plan_plain})" \
    "large_exact_fallback"

large_index="$(scalar "SELECT pgcontext.create_lexical_index('lexical_docs', 'article')")"
assert_nonempty "large-corpus lexical index creation returns an index name" "${large_index}"
psql_db -c "ANALYZE public.lexical_docs" >/dev/null

assert_nonempty "indexed lexical path serves a corpus the exact path cannot finish" \
    "$(lexical_keys lexical_docs "${plan_selective}")"

expect_fails_closed "an indexed probe past its candidate allowance fails closed" \
    "SET pgcontext.lexical_candidate_budget = 1;
     SELECT count(*) FROM pgcontext.execute_query('lexical_docs', ${plan_plain})" \
    "large_probe_allowance"

plan_uses_index="$(psql_db -tAc "
    SET LOCAL enable_seqscan = off;
    EXPLAIN (COSTS OFF)
    SELECT source.id
      FROM public.lexical_docs AS source
     WHERE pg_catalog.setweight(
               pg_catalog.to_tsvector(
                   '\"pg_catalog\".\"english\"'::pg_catalog.regconfig,
                   coalesce(source.title::text, ''::text)
               ), 'A'
           ) OPERATOR(pg_catalog.||)
           pg_catalog.setweight(
               pg_catalog.to_tsvector(
                   '\"pg_catalog\".\"english\"'::pg_catalog.regconfig,
                   coalesce(source.body::text, ''::text)
               ), 'D'
           ) OPERATOR(pg_catalog.@@) pg_catalog.plainto_tsquery(
               '\"pg_catalog\".\"english\"'::pg_catalog.regconfig, 'postgres storage'
           )
" | grep -c "${large_index}" || true)"
if [[ "${plan_uses_index}" == "0" ]]; then
    echo "canonical lexical index expression was not planner-matchable" >&2
    exit 1
fi
echo "canonical lexical index expression is planner-matchable: ok"

# ---------------------------------------------------------------------------
# Weight-restricted negation: `ts_filter` removes lexemes, and removing a lexeme
# can make a negated clause become true, so a restricted match is NOT a subset
# of the unrestricted match. The indexed probe must use the restricted
# expression or it silently drops rows the exact path returns.
# ---------------------------------------------------------------------------

psql_db <<SQL >/dev/null
CREATE TABLE public.lexical_negation (
    id bigint PRIMARY KEY,
    title text NOT NULL,
    body text NOT NULL
);
INSERT INTO public.lexical_negation VALUES
    (1, 'alpha', 'beta'),
    (2, 'alpha', 'gamma'),
    (3, 'delta', 'beta');
SELECT pgcontext.create_collection('lexical_negation', 'public.lexical_negation');
SELECT pgcontext.backfill_points('lexical_negation', 100);
SELECT pgcontext.register_lexical_source(
    'lexical_negation', 'article', ARRAY['title', 'body'],
    'pg_catalog.simple', ARRAY['A', 'D']
);
SQL

plan_negation="pgcontext.query_lexical('article', jsonb_build_object('form','weight_restricted','weights', jsonb_build_array('a'), 'query', jsonb_build_object('form','boolean','operator','and','clauses', jsonb_build_array(jsonb_build_object('form','plain','text','alpha'), jsonb_build_object('form','boolean','operator','not','clauses', jsonb_build_array(jsonb_build_object('form','plain','text','beta')))))), NULL, 10)"

negation_exact="$(lexical_keys lexical_negation "${plan_negation}")"
assert_equal "weight-restricted negation matches on the exact path" "1,2" "${negation_exact}"

psql_db -c "SELECT pgcontext.create_lexical_index('lexical_negation', 'article')" >/dev/null
psql_db -c "ANALYZE public.lexical_negation" >/dev/null
assert_equal "weight-restricted negation keeps every row on the indexed path" \
    "${negation_exact}" "$(lexical_keys lexical_negation "${plan_negation}")"

# ---------------------------------------------------------------------------
# Optional trigram lane.
# ---------------------------------------------------------------------------

if [[ "$(scalar "SELECT count(*) FROM pg_catalog.pg_available_extensions WHERE name = 'pg_trgm'")" != "0" ]]; then
    psql_db <<SQL >/dev/null
CREATE SCHEMA trgm_home;
CREATE EXTENSION pg_trgm SCHEMA trgm_home;
CREATE TABLE public.fuzzy_docs (
    id bigint PRIMARY KEY,
    body text NOT NULL,
    other text NOT NULL,
    bad_value integer NOT NULL DEFAULT 0
);
INSERT INTO public.fuzzy_docs (id, body, other) VALUES
    (1, 'postgres', 'alpha'),
    (2, 'postgresql database', 'beta'),
    (3, 'completely different', 'gamma'),
    (4, 'postgrs', 'delta');
SELECT pgcontext.create_collection('fuzzy_docs', 'public.fuzzy_docs');
SELECT pgcontext.backfill_points('fuzzy_docs', 100);
SELECT pgcontext.register_fuzzy_source('fuzzy_docs', 'body_trgm', 'body');
SQL

    expect_sql_error "fuzzy registration rejects non-text columns" \
        "SELECT pgcontext.register_fuzzy_source('fuzzy_docs', 'bad_trgm', 'bad_value')" \
        "fuzzy source column must be text" \
        "fuzzy_non_text"

    psql_db -c \
        "CREATE INDEX fuzzy_docs_wrong_column_gin
             ON public.fuzzy_docs USING gin (other trgm_home.gin_trgm_ops)" >/dev/null
    expect_sql_error "a fuzzy index for another column cannot attach" \
        "SELECT pgcontext.attach_fuzzy_index(
             'fuzzy_docs', 'body_trgm', 'fuzzy_docs_wrong_column_gin'
         )" \
        "fuzzy index does not match the registered text column and pg_trgm operator class" \
        "wrong_fuzzy_column"
    psql_db -c "DROP INDEX public.fuzzy_docs_wrong_column_gin" >/dev/null

    psql_db -c "ALTER TABLE public.fuzzy_docs ALTER COLUMN body TYPE varchar(100)" >/dev/null
    expect_sql_error "fuzzy column type drift fails closed" \
        "SELECT count(*) FROM pgcontext.execute_query(
             'fuzzy_docs',
             pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.3, NULL, 10)
         )" \
        "registered fuzzy source text column drifted" \
        "fuzzy_type_drift"
    psql_db -c "ALTER TABLE public.fuzzy_docs ALTER COLUMN body TYPE text" >/dev/null

    assert_equal "fuzzy sources resolve a relocated pg_trgm schema" "trgm_home" \
        "$(scalar "SELECT trgm_schema FROM pgcontext.fuzzy_sources('fuzzy_docs')")"

    fuzzy_exact="$(psql_db -tAc "
        SELECT string_agg(source_key, ',' ORDER BY score DESC, point_id ASC)
          FROM pgcontext.execute_query('fuzzy_docs',
              pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.3, NULL, 10))
    " | tr -d '[:space:]')"
    assert_nonempty "exact fuzzy retrieval returns rows" "${fuzzy_exact}"

    psql_db -c "SELECT pgcontext.create_fuzzy_index('fuzzy_docs', 'body_trgm')" >/dev/null
    assert_equal "indexed fuzzy path matches the exact fuzzy path" "${fuzzy_exact}" \
        "$(psql_db -tAc "
            SELECT string_agg(source_key, ',' ORDER BY score DESC, point_id ASC)
              FROM pgcontext.execute_query('fuzzy_docs',
                  pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.3, NULL, 10))
        " | tr -d '[:space:]')"

    # Each mode must be restored to its own documented default, not a shared one.
    assert_equal "word_similarity probes restore their own mode default" "0.6" \
        "$(psql_db -tAc "
            SELECT 1 FROM pgcontext.execute_query('fuzzy_docs',
                pgcontext.query_fuzzy('body_trgm', 'postgrs', 'word_similarity', 0.9, NULL, 10))
            LIMIT 1;
            SELECT pg_catalog.current_setting('pg_trgm.word_similarity_threshold')
        " | tail -n 1 | tr -d '[:space:]')"

    assert_equal "an explicitly set trigram threshold is not leaked" "0.42" \
        "$(psql_db -tAc "
            SET pg_trgm.similarity_threshold = 0.42;
            SELECT 1 FROM pgcontext.execute_query('fuzzy_docs',
                pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.2, NULL, 10))
            LIMIT 1;
            SELECT pg_catalog.current_setting('pg_trgm.similarity_threshold')
        " | tail -n 1 | tr -d '[:space:]')"
else
    echo "pg_trgm is unavailable; skipping the fuzzy lane"
fi

cleanup_database
echo "indexed_lexical_hybrid: ok"
