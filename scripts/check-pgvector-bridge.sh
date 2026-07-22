#!/usr/bin/env bash
# Live certification gate for the separately packaged pgcontext_pgvector bridge.
#
# Preconditions: a PostgreSQL 17 server with vector, pgcontext, and
# pgcontext_pgvector artifacts installed. The database named below is disposable.
set -euo pipefail

PSQL=${PGCONTEXT_BRIDGE_PSQL:-psql}
DB=${PGCONTEXT_BRIDGE_DB:-pgcontext_pgvector_check}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

q() {
  ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq -c "$1"
}

expect_failure() {
  local description=$1
  local sql=$2
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-bridge-failure.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

q "CREATE EXTENSION vector; CREATE EXTENSION pgcontext; CREATE EXTENSION pgcontext_pgvector" >/dev/null

bridge_dependencies=$(q "SELECT pg_catalog.string_agg(required.extname, ',' ORDER BY required.extname)
                           FROM pg_catalog.pg_depend AS dependency
                           JOIN pg_catalog.pg_extension AS bridge
                             ON dependency.classid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                            AND dependency.objid = bridge.oid
                           JOIN pg_catalog.pg_extension AS required
                             ON dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                            AND dependency.refobjid = required.oid
                          WHERE bridge.extname = 'pgcontext_pgvector'")
[[ "${bridge_dependencies}" == "pgcontext,vector" ]] \
  || fail "bridge dependencies are '${bridge_dependencies}', expected pgcontext,vector"

bridge_opclasses=$(q "SELECT count(*)
                        FROM pg_catalog.pg_opclass AS opclass
                        JOIN pg_catalog.pg_depend AS dependency
                          ON dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                         AND dependency.objid = opclass.oid
                         AND dependency.deptype = 'e'
                        JOIN pg_catalog.pg_extension AS extension
                          ON extension.oid = dependency.refobjid
                       WHERE extension.extname = 'pgcontext_pgvector'")
[[ "${bridge_opclasses}" == "8" ]] || fail "bridge owns ${bridge_opclasses} opclasses, expected 8"

invalid_bridge_opclasses=$(q "SELECT count(*)
                                FROM pg_catalog.pg_opclass AS opclass
                                JOIN pg_catalog.pg_depend AS dependency
                                  ON dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                                 AND dependency.objid = opclass.oid
                                 AND dependency.deptype = 'e'
                                JOIN pg_catalog.pg_extension AS extension
                                  ON extension.oid = dependency.refobjid
                               WHERE extension.extname = 'pgcontext_pgvector'
                                 AND NOT pg_catalog.amvalidate(opclass.oid)")
[[ "${invalid_bridge_opclasses}" == "0" ]] \
  || fail "${invalid_bridge_opclasses} certified bridge opclasses failed amvalidate"

bridge_functions=$(q "SELECT count(*)
                        FROM pg_catalog.pg_proc AS procedure
                        JOIN pg_catalog.pg_depend AS dependency
                          ON dependency.classid = 'pg_catalog.pg_proc'::pg_catalog.regclass
                         AND dependency.objid = procedure.oid
                         AND dependency.deptype = 'e'
                        JOIN pg_catalog.pg_extension AS extension
                          ON extension.oid = dependency.refobjid
                       WHERE extension.extname = 'pgcontext_pgvector'")
[[ "${bridge_functions}" == "8" ]] || fail "bridge owns ${bridge_functions} support functions, expected 8"

binary_casts=$(q "SELECT count(*)
                    FROM pg_catalog.pg_cast
                   WHERE castsource IN ('public.vector'::pg_catalog.regtype,
                                        'public.halfvec'::pg_catalog.regtype)
                     AND casttarget IN ('pgcontext.vector'::pg_catalog.regtype,
                                        'pgcontext.halfvec'::pg_catalog.regtype)
                     AND castmethod = 'b'")
[[ "${binary_casts}" == "2" ]] || fail "bridge exposes ${binary_casts} certified binary casts, expected 2"

sparse_bridge_objects=$(q "SELECT
    (SELECT count(*) FROM pg_catalog.pg_cast
      WHERE castsource = 'public.sparsevec'::pg_catalog.regtype
        AND casttarget = 'pgcontext.sparsevec'::pg_catalog.regtype)
  + (SELECT count(*) FROM pg_catalog.pg_opclass
      WHERE opcname LIKE '%pgvector%'
        AND opcintype = 'public.sparsevec'::pg_catalog.regtype)")
[[ "${sparse_bridge_objects}" == "0" ]] \
  || fail "uncertified sparsevec bridge objects were installed"

vector_fixture=$(q "WITH fixture(value) AS (
                       VALUES ('[1.25,-2.5,3]'::public.vector),
                              ('[0,0,0]'::public.vector),
                              ('[-0.125,4.5,9.75]'::public.vector)
                     )
                     SELECT pg_catalog.bool_and(
                              value::text = (value::pgcontext.vector)::text
                              AND pg_catalog.pg_column_size(value)
                                  = pg_catalog.pg_column_size(value::pgcontext.vector))
                       FROM fixture")
[[ "${vector_fixture}" == "t" ]] || fail "vector cross-extension fixture changed value or size"

halfvec_fixture=$(q "WITH fixture(value) AS (
                        VALUES ('[1.25,-2.5,3]'::public.halfvec),
                               ('[0,0,0]'::public.halfvec),
                               ('[-0.125,4.5,9.75]'::public.halfvec)
                      )
                      SELECT pg_catalog.bool_and(
                               value::text = (value::pgcontext.halfvec)::text
                               AND pg_catalog.pg_column_size(value)
                                   = pg_catalog.pg_column_size(value::pgcontext.halfvec))
                        FROM fixture")
[[ "${halfvec_fixture}" == "t" ]] || fail "halfvec cross-extension fixture changed value or size"

q "CREATE TABLE bridge_vector_docs (
     id bigint PRIMARY KEY,
     embedding public.vector(3) NOT NULL
   );
   INSERT INTO bridge_vector_docs VALUES
     (1, '[1,0,0]'), (2, '[0,1,0]'), (3, '[0,0,1]'), (4, '[-1,0,0]');
   CREATE INDEX bridge_vector_l2 ON bridge_vector_docs USING pgcontext_hnsw
     (embedding pgcontext.vector_hnsw_pgvector_l2_ops);
   CREATE INDEX bridge_vector_ip ON bridge_vector_docs USING pgcontext_hnsw
     (embedding pgcontext.vector_hnsw_pgvector_ip_ops);
   CREATE INDEX bridge_vector_cosine ON bridge_vector_docs USING pgcontext_hnsw
     (embedding pgcontext.vector_hnsw_pgvector_cosine_ops);
   CREATE INDEX bridge_vector_l1 ON bridge_vector_docs USING pgcontext_hnsw
     (embedding pgcontext.vector_hnsw_pgvector_l1_ops);

   CREATE TABLE bridge_halfvec_docs (
     id bigint PRIMARY KEY,
     embedding public.halfvec(3) NOT NULL
   );
   INSERT INTO bridge_halfvec_docs SELECT id, embedding::public.halfvec
     FROM bridge_vector_docs;
   CREATE INDEX bridge_halfvec_l2 ON bridge_halfvec_docs USING pgcontext_hnsw
     (embedding pgcontext.halfvec_hnsw_pgvector_l2_ops);
   CREATE INDEX bridge_halfvec_ip ON bridge_halfvec_docs USING pgcontext_hnsw
     (embedding pgcontext.halfvec_hnsw_pgvector_ip_ops);
   CREATE INDEX bridge_halfvec_cosine ON bridge_halfvec_docs USING pgcontext_hnsw
     (embedding pgcontext.halfvec_hnsw_pgvector_cosine_ops);
   CREATE INDEX bridge_halfvec_l1 ON bridge_halfvec_docs USING pgcontext_hnsw
     (embedding pgcontext.halfvec_hnsw_pgvector_l1_ops)" >/dev/null

q "INSERT INTO bridge_vector_docs
     SELECT series + 100,
            ARRAY[
              pg_catalog.sin(series::double precision)::real,
              pg_catalog.cos((series * 0.7)::double precision)::real,
              ((series % 11) - 5)::real / 7::real
            ]::public.vector
       FROM pg_catalog.generate_series(1, 64) AS series;
   INSERT INTO bridge_halfvec_docs
     SELECT id, embedding::public.halfvec
       FROM bridge_vector_docs
      WHERE id >= 100" >/dev/null

for type_name in vector halfvec; do
  table_name=bridge_${type_name}_docs
  for operator in '<->' '<#>' '<=>' '<+>'; do
    nearest=$(q "SET LOCAL enable_seqscan = off;
                  SELECT id FROM ${table_name}
                   ORDER BY embedding OPERATOR(public.${operator})
                            '[1,0,0]'::public.${type_name}
                   LIMIT 1")
    [[ "${nearest}" == "1" ]] \
      || fail "${type_name} ${operator} returned id ${nearest}, expected 1"
  done
done

for type_name in vector halfvec; do
  table_name=bridge_${type_name}_docs
  for metric_operator in 'l2:<->' 'ip:<#>' 'cosine:<=>' 'l1:<+>'; do
    metric=${metric_operator%%:*}
    operator=${metric_operator#*:}
    exact_ids=$(q "SET LOCAL enable_indexscan = off;
                   SET LOCAL enable_bitmapscan = off;
                   SELECT pg_catalog.string_agg(id::text, ',' ORDER BY ordinal)
                     FROM (
                       SELECT id, pg_catalog.row_number() OVER () AS ordinal
                         FROM (
                           SELECT id FROM ${table_name}
                            ORDER BY embedding OPERATOR(public.${operator})
                                     '[0.123,-0.456,0.789]'::public.${type_name}
                            LIMIT 10
                         ) AS exact_rows
                     ) AS ordered_exact")
    indexed_ids=$(q "SET LOCAL enable_seqscan = off;
                     SELECT pg_catalog.string_agg(id::text, ',' ORDER BY ordinal)
                       FROM (
                         SELECT id, pg_catalog.row_number() OVER () AS ordinal
                           FROM (
                             SELECT id FROM ${table_name}
                              ORDER BY embedding OPERATOR(public.${operator})
                                       '[0.123,-0.456,0.789]'::public.${type_name}
                              LIMIT 10
                           ) AS indexed_rows
                       ) AS ordered_indexed")
    [[ "${indexed_ids}" == "${exact_ids}" ]] \
      || fail "${type_name} ${operator} indexed order ${indexed_ids} differs from exact oracle ${exact_ids}"

    plan=$(q "SET LOCAL enable_seqscan = off;
              EXPLAIN (COSTS OFF)
              SELECT id FROM ${table_name}
               ORDER BY embedding OPERATOR(public.${operator})
                        '[0.123,-0.456,0.789]'::public.${type_name}
               LIMIT 10")
    expected_index=bridge_${type_name}_${metric}
    [[ "${plan}" == *"Index Scan using ${expected_index}"* ]] \
      || fail "${type_name} ${operator} did not select ${expected_index}: ${plan}"
  done
done

q "INSERT INTO bridge_vector_docs VALUES (5, '[0.99,0.01,0]');
   UPDATE bridge_vector_docs SET embedding = '[0.98,0.02,0]' WHERE id = 5;
   DELETE FROM bridge_vector_docs WHERE id = 4;
   REINDEX TABLE bridge_vector_docs;
   INSERT INTO bridge_halfvec_docs VALUES (5, '[0.99,0.01,0]');
   UPDATE bridge_halfvec_docs SET embedding = '[0.98,0.02,0]' WHERE id = 5;
   DELETE FROM bridge_halfvec_docs WHERE id = 4;
   REINDEX TABLE bridge_halfvec_docs" >/dev/null
q "VACUUM bridge_vector_docs" >/dev/null
q "VACUUM bridge_halfvec_docs" >/dev/null

q "CREATE TABLE bridge_adopt_docs (
     id bigint PRIMARY KEY,
     embedding public.vector(3) NOT NULL
   );
   INSERT INTO bridge_adopt_docs VALUES (1, '[1,0,0]'), (2, '[0,1,0]');
   CREATE INDEX bridge_adopt_native
     ON bridge_adopt_docs USING hnsw (embedding public.vector_cosine_ops)" >/dev/null
adopted=$(q "SELECT pg_catalog.count(*)
               FROM pgcontext.adopt_pgvector(
                      'bridge_adopt_docs'::pg_catalog.regclass,
                      false,
                      false
                    )
              WHERE action = 'created' AND executed")
[[ "${adopted}" == "1" ]] || fail "adopt_pgvector executed ${adopted} replacement plans, expected 1"
adopt_opclass=$(q "SELECT opclass.opcname
                     FROM pg_catalog.pg_index AS index
                     JOIN pg_catalog.pg_class AS relation ON relation.oid = index.indexrelid
                     JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
                    WHERE relation.relname = 'bridge_adopt_native_pgc'")
[[ "${adopt_opclass}" == "vector_hnsw_pgvector_cosine_ops" ]] \
  || fail "adopt_pgvector selected ${adopt_opclass}, expected bridge cosine opclass"

suggested=$(q "CREATE TABLE bridge_unindexed_docs (
                 id bigint PRIMARY KEY,
                 embedding public.vector(3) NOT NULL
               );
               SELECT suggested_command
                 FROM pgcontext.migration_report()
                WHERE table_name = 'bridge_unindexed_docs'
                  AND column_name = 'embedding'")
[[ "${suggested}" == *"pgcontext.vector_hnsw_pgvector_cosine_ops"* ]] \
  || fail "unindexed migration suggestion did not select a bridge opclass: ${suggested}"

q "CREATE SCHEMA bridge_attack;
   CREATE FUNCTION bridge_attack.fake_support(public.vector, public.vector)
   RETURNS double precision LANGUAGE SQL IMMUTABLE STRICT
   RETURN 0::double precision;
   CREATE OPERATOR CLASS bridge_attack.fake_support_ops
     FOR TYPE public.vector USING pgcontext_hnsw AS
     OPERATOR 1 public.<=> (public.vector, public.vector)
       FOR ORDER BY pg_catalog.float_ops,
     FUNCTION 1 bridge_attack.fake_support(public.vector, public.vector),
     STORAGE pgcontext.vector" >/dev/null
fake_support_valid=$(q "SELECT pg_catalog.amvalidate(opclass.oid)
                          FROM pg_catalog.pg_opclass AS opclass
                         WHERE opclass.opcname = 'fake_support_ops'")
[[ "${fake_support_valid}" == "f" ]] \
  || fail "counterfeit support opclass passed amvalidate"
expect_failure "counterfeit support function index build" \
  "CREATE INDEX bridge_fake_support ON bridge_vector_docs USING pgcontext_hnsw (embedding bridge_attack.fake_support_ops)"

q "CREATE FUNCTION bridge_attack.fake_cosine(public.vector, public.vector)
   RETURNS double precision LANGUAGE SQL IMMUTABLE STRICT
   RETURN 0::double precision;
   CREATE OPERATOR bridge_attack.<=> (
     LEFTARG = public.vector,
     RIGHTARG = public.vector,
     FUNCTION = bridge_attack.fake_cosine,
     COMMUTATOR = OPERATOR(bridge_attack.<=>)
   );
   CREATE OPERATOR CLASS bridge_attack.fake_operator_ops
     FOR TYPE public.vector USING pgcontext_hnsw AS
     OPERATOR 1 bridge_attack.<=> (public.vector, public.vector)
       FOR ORDER BY pg_catalog.float_ops,
     FUNCTION 1 pgcontext._pgvector_vector_cosine_support(public.vector, public.vector),
     STORAGE pgcontext.vector" >/dev/null
fake_operator_valid=$(q "SELECT pg_catalog.amvalidate(opclass.oid)
                           FROM pg_catalog.pg_opclass AS opclass
                          WHERE opclass.opcname = 'fake_operator_ops'")
[[ "${fake_operator_valid}" == "f" ]] \
  || fail "counterfeit operator opclass passed amvalidate"
expect_failure "counterfeit strategy operator index build" \
  "CREATE INDEX bridge_fake_operator ON bridge_vector_docs USING pgcontext_hnsw (embedding bridge_attack.fake_operator_ops)"

expect_failure "DROP EXTENSION vector with bridge installed" "DROP EXTENSION vector"
expect_failure "DROP EXTENSION pgcontext with bridge installed" "DROP EXTENSION pgcontext"
expect_failure "DROP bridge while bridge indexes exist" "DROP EXTENSION pgcontext_pgvector"

q "DROP TABLE bridge_vector_docs, bridge_halfvec_docs,
              bridge_adopt_docs, bridge_unindexed_docs;
   DROP SCHEMA bridge_attack CASCADE;
   DROP EXTENSION pgcontext_pgvector" >/dev/null

remaining_bridge_objects=$(q "SELECT
    (SELECT count(*) FROM pg_catalog.pg_opclass WHERE opcname LIKE '%pgvector%')
  + (SELECT count(*) FROM pg_catalog.pg_proc WHERE proname LIKE '_pgvector_%_support')
  + (SELECT count(*) FROM pg_catalog.pg_cast
      WHERE castsource IN ('public.vector'::pg_catalog.regtype,
                           'public.halfvec'::pg_catalog.regtype)
        AND casttarget IN ('pgcontext.vector'::pg_catalog.regtype,
                           'pgcontext.halfvec'::pg_catalog.regtype))")
[[ "${remaining_bridge_objects}" == "0" ]] \
  || fail "dropping the bridge left ${remaining_bridge_objects} bridge objects"

extensions_left=$(q "SELECT pg_catalog.string_agg(extname, ',' ORDER BY extname)
                       FROM pg_catalog.pg_extension
                      WHERE extname IN ('pgcontext', 'vector')")
[[ "${extensions_left}" == "pgcontext,vector" ]] \
  || fail "bridge removal damaged parent extensions: ${extensions_left}"

q "DROP EXTENSION vector;
   CREATE TABLE canonical_after_bridge_drop (
     id bigint PRIMARY KEY,
     embedding pgcontext.vector(3) NOT NULL
   );
   INSERT INTO canonical_after_bridge_drop VALUES (1, '[1,0,0]'), (2, '[0,1,0]');
   CREATE INDEX canonical_after_bridge_drop_hnsw
     ON canonical_after_bridge_drop USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_cosine_ops)" >/dev/null

canonical_nearest=$(q "SELECT id FROM canonical_after_bridge_drop
                        ORDER BY embedding OPERATOR(pgcontext.<=>)
                                 '[1,0,0]'::pgcontext.vector
                        LIMIT 1")
[[ "${canonical_nearest}" == "1" ]] \
  || fail "canonical pgContext HNSW failed after bridge and pgvector removal"

${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${DB};" >/dev/null
echo "pgvector bridge verification passed (8 opclasses, binary casts, certification, lifecycle, and clean removal)"
