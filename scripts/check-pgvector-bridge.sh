#!/usr/bin/env bash
# Live certification gate for main-extension-owned pgvector bindings.
#
# Preconditions: a PostgreSQL 17 or 18 server with vector and pgcontext.
set -euo pipefail

PSQL=${PGCONTEXT_BRIDGE_PSQL:-psql}
PG_DUMP=${PGCONTEXT_BRIDGE_PG_DUMP:-pg_dump}
PG_RESTORE=${PGCONTEXT_BRIDGE_PG_RESTORE:-pg_restore}
DB=${PGCONTEXT_BRIDGE_DB:-pgcontext_pgvector_check}
RESTORE_DB=${PGCONTEXT_BRIDGE_RESTORE_DB:-${DB}_restore}
DUMP_FILE=${TMPDIR:-/tmp}/${DB}-${$}.dump

for database_name in "${DB}" "${RESTORE_DB}"; do
  if [[ ! "${database_name}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "FAIL: bridge database names must be simple SQL identifiers" >&2
    exit 2
  fi
done

cleanup() {
  ${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS ${RESTORE_DB};" >/dev/null 2>&1 || true
  rm -f "${DUMP_FILE}"
}
trap cleanup EXIT

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
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-bridge-failure-${DB}.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
}

expect_failure_matching() {
  local description=$1
  local sql=$2
  local expected=$3
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-bridge-failure-${DB}.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
  if ! grep -Fq "${expected}" "${output_file}"; then
    fail "${description} failed for the wrong reason; expected '${expected}'"
  fi
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP ROLE IF EXISTS bridge_member_a;" \
  -c "DROP ROLE IF EXISTS bridge_member_b;" \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

q "CREATE EXTENSION vector;
   CREATE EXTENSION pgcontext" >/dev/null

# Membership alone does not authorize optional-object creation: callers must
# explicitly SET ROLE to the exact extension owner so every object has one
# deterministic owner across sessions.
extension_owner=$(q "SELECT pg_catalog.format('%I', role.rolname)
                        FROM pg_catalog.pg_extension AS extension
                        JOIN pg_catalog.pg_roles AS role ON role.oid = extension.extowner
                       WHERE extension.extname = 'pgcontext'")
${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "CREATE ROLE bridge_member_a;" \
  -c "CREATE ROLE bridge_member_b;" \
  -c "GRANT ${extension_owner} TO bridge_member_a, bridge_member_b;" >/dev/null
expect_failure_matching \
  "extension-owner member without SET ROLE" \
  "SET SESSION AUTHORIZATION bridge_member_a; SELECT pgcontext.enable_pgvector_binding()" \
  "must SET ROLE to the pgcontext extension owner"

expect_failure_matching \
  "facade installation over pgvector-owned access-method names" \
  "SELECT pgcontext.enable_pgvector_name_facade()" \
  "cannot install pgvector name facade because hnsw or ivfflat is already owned"

q "SET SESSION AUTHORIZATION bridge_member_a;
   SET ROLE ${extension_owner};
   SELECT pgcontext.enable_pgvector_binding();
   SELECT pgcontext.enable_pgvector_binding();
   RESET ROLE;
   RESET SESSION AUTHORIZATION" >/dev/null

retired_companion=$(q "SELECT count(*) FROM pg_catalog.pg_extension
                        WHERE extname = 'pgcontext_pgvector'")
[[ "${retired_companion}" == "0" ]] \
  || fail "retired pgcontext_pgvector companion was installed"

bridge_opclasses=$(q "SELECT count(*)
                        FROM pg_catalog.pg_opclass AS opclass
                        JOIN pg_catalog.pg_namespace AS namespace
                          ON namespace.oid = opclass.opcnamespace
                        JOIN pg_catalog.pg_extension AS extension
                          ON extension.extname = 'pgcontext'
                       WHERE namespace.nspname = 'pgcontext'
                         AND opclass.opcowner = extension.extowner
                         AND opclass.opcname LIKE '%_hnsw_pgvector_%_ops'")
[[ "${bridge_opclasses}" == "12" ]] || fail "bridge owns ${bridge_opclasses} opclasses, expected 12"

invalid_bridge_opclasses=$(q "SELECT count(*)
                                FROM pg_catalog.pg_opclass AS opclass
                                JOIN pg_catalog.pg_namespace AS namespace
                                  ON namespace.oid = opclass.opcnamespace
                                JOIN pg_catalog.pg_extension AS extension
                                  ON extension.extname = 'pgcontext'
                               WHERE namespace.nspname = 'pgcontext'
                                 AND opclass.opcowner = extension.extowner
                                 AND opclass.opcname LIKE '%_hnsw_pgvector_%_ops'
                                 AND NOT pg_catalog.amvalidate(opclass.oid)")
[[ "${invalid_bridge_opclasses}" == "0" ]] \
  || fail "${invalid_bridge_opclasses} certified bridge opclasses failed amvalidate"

bridge_functions=$(q "SELECT count(*)
                        FROM pg_catalog.pg_proc AS procedure
                        JOIN pg_catalog.pg_namespace AS namespace
                          ON namespace.oid = procedure.pronamespace
                        JOIN pg_catalog.pg_extension AS extension
                          ON extension.extname = 'pgcontext'
                       WHERE namespace.nspname = 'pgcontext'
                         AND procedure.proowner = extension.extowner
                         AND (
                             procedure.proname LIKE '_pgvector_%_support'
                             OR procedure.proname IN (
                                 '_pgvector_sparsevec_to_pgcontext',
                                 '_pgcontext_sparsevec_to_pgvector'
                             )
                         )")
[[ "${bridge_functions}" == "14" ]] || fail "bridge owns ${bridge_functions} functions, expected 14"

binary_casts=$(q "SELECT count(*)
                    FROM pg_catalog.pg_cast
                   WHERE castsource IN ('public.vector'::pg_catalog.regtype,
                                        'public.halfvec'::pg_catalog.regtype)
                     AND casttarget IN ('pgcontext.vector'::pg_catalog.regtype,
                                        'pgcontext.halfvec'::pg_catalog.regtype)
                     AND castmethod = 'b'")
[[ "${binary_casts}" == "2" ]] || fail "bridge exposes ${binary_casts} certified binary casts, expected 2"

sparse_function_casts=$(q "SELECT count(*)
                              FROM pg_catalog.pg_cast
                             WHERE (
                                      castsource = 'public.sparsevec'::pg_catalog.regtype
                                  AND casttarget = 'pgcontext.sparsevec'::pg_catalog.regtype
                                   OR castsource = 'pgcontext.sparsevec'::pg_catalog.regtype
                                  AND casttarget = 'public.sparsevec'::pg_catalog.regtype
                                   )
                               AND castmethod = 'f'
                               AND castcontext = 'a'")
[[ "${sparse_function_casts}" == "2" ]] \
  || fail "bridge exposes ${sparse_function_casts} sparsevec conversion casts, expected 2"

sparse_bridge_opclasses=$(q "SELECT count(*)
                               FROM pg_catalog.pg_opclass
                              WHERE opcname LIKE 'sparsevec_hnsw_pgvector_%'
                                AND opcintype = 'public.sparsevec'::pg_catalog.regtype")
[[ "${sparse_bridge_opclasses}" == "4" ]] \
  || fail "bridge exposes ${sparse_bridge_opclasses} sparsevec opclasses, expected 4"

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

sparsevec_fixture=$(q "WITH fixture(value) AS (
                          VALUES ('{1:1.25,3:-2.5}/4'::public.sparsevec),
                                 ('{}/4'::public.sparsevec),
                                 ('{2:-0.125,4:9.75}/4'::public.sparsevec)
                        )
                        SELECT pg_catalog.bool_and(
                                 value::text = (value::pgcontext.sparsevec)::text
                                 AND value::text =
                                     ((value::pgcontext.sparsevec)::public.sparsevec)::text)
                          FROM fixture")
[[ "${sparsevec_fixture}" == "t" ]] || fail "sparsevec conversion fixture changed values"

q "CREATE TABLE bridge_sparsevec_oversized (
     id bigint PRIMARY KEY,
     embedding public.sparsevec(16001) NOT NULL
   );
   INSERT INTO bridge_sparsevec_oversized VALUES (1, '{1:1}/16001')" >/dev/null
expect_failure_matching "oversized sparsevec index" \
  "CREATE INDEX bridge_sparsevec_oversized_hnsw
     ON bridge_sparsevec_oversized USING pgcontext_hnsw
       (embedding pgcontext.sparsevec_hnsw_pgvector_cosine_ops)" \
  "large-dimension sparse support is planned"

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
     (embedding pgcontext.halfvec_hnsw_pgvector_l1_ops);

   CREATE TABLE bridge_sparsevec_docs (
     id bigint PRIMARY KEY,
     embedding public.sparsevec(4) NOT NULL
   );
   INSERT INTO bridge_sparsevec_docs VALUES
     (1, '{1:1}/4'), (2, '{2:1}/4'), (3, '{3:1}/4'), (4, '{1:-1}/4');
   CREATE INDEX bridge_sparsevec_l2 ON bridge_sparsevec_docs USING pgcontext_hnsw
     (embedding pgcontext.sparsevec_hnsw_pgvector_l2_ops);
   CREATE INDEX bridge_sparsevec_ip ON bridge_sparsevec_docs USING pgcontext_hnsw
     (embedding pgcontext.sparsevec_hnsw_pgvector_ip_ops);
   CREATE INDEX bridge_sparsevec_cosine ON bridge_sparsevec_docs USING pgcontext_hnsw
     (embedding pgcontext.sparsevec_hnsw_pgvector_cosine_ops);
   CREATE INDEX bridge_sparsevec_l1 ON bridge_sparsevec_docs USING pgcontext_hnsw
     (embedding pgcontext.sparsevec_hnsw_pgvector_l1_ops)" >/dev/null

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
      WHERE id >= 100;
   INSERT INTO bridge_sparsevec_docs
     SELECT series + 100,
            ARRAY[
              pg_catalog.sin(series::double precision)::real,
              pg_catalog.cos((series * 0.7)::double precision)::real,
              0::real,
              ((series % 11) - 5)::real / 7::real
            ]::public.vector::public.sparsevec
       FROM pg_catalog.generate_series(1, 64) AS series" >/dev/null

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

for metric_operator in 'l2:<->' 'ip:<#>' 'cosine:<=>' 'l1:<+>'; do
  metric=${metric_operator%%:*}
  operator=${metric_operator#*:}
  exact_ids=$(q "SET LOCAL enable_indexscan = off;
                 SET LOCAL enable_bitmapscan = off;
                 SELECT pg_catalog.string_agg(id::text, ',' ORDER BY ordinal)
                   FROM (
                     SELECT id, pg_catalog.row_number() OVER () AS ordinal
                       FROM (
                         SELECT id FROM bridge_sparsevec_docs
                          ORDER BY embedding OPERATOR(public.${operator})
                                   '{1:0.123,2:-0.456,4:0.789}/4'::public.sparsevec
                          LIMIT 10
                       ) AS exact_rows
                   ) AS ordered_exact")
  indexed_ids=$(q "SET LOCAL enable_seqscan = off;
                   SELECT pg_catalog.string_agg(id::text, ',' ORDER BY ordinal)
                     FROM (
                       SELECT id, pg_catalog.row_number() OVER () AS ordinal
                         FROM (
                           SELECT id FROM bridge_sparsevec_docs
                            ORDER BY embedding OPERATOR(public.${operator})
                                     '{1:0.123,2:-0.456,4:0.789}/4'::public.sparsevec
                            LIMIT 10
                         ) AS indexed_rows
                     ) AS ordered_indexed")
  [[ "${indexed_ids}" == "${exact_ids}" ]] \
    || fail "sparsevec ${operator} indexed order ${indexed_ids} differs from exact oracle ${exact_ids}"
  plan=$(q "SET LOCAL enable_seqscan = off;
            EXPLAIN (COSTS OFF)
            SELECT id FROM bridge_sparsevec_docs
             ORDER BY embedding OPERATOR(public.${operator})
                      '{1:0.123,2:-0.456,4:0.789}/4'::public.sparsevec
             LIMIT 10")
  expected_index=bridge_sparsevec_${metric}
  [[ "${plan}" == *"Index Scan using ${expected_index}"* ]] \
    || fail "sparsevec ${operator} did not select ${expected_index}: ${plan}"
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
                    WHERE relation.relname LIKE 'bridge_adopt_native_pgc_%'")
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

# Quoted, multibyte, and near-NAMEDATALEN identifiers must survive EXPLAIN
# parsing and produce a unique byte-bounded replacement name.
q "CREATE TABLE \"bridge 名称\" (
     id bigint PRIMARY KEY,
     embedding public.vector(3) NOT NULL
   );
   INSERT INTO \"bridge 名称\" VALUES (1, '[1,0,0]'), (2, '[0,1,0]');
   CREATE INDEX \"索引 \"\"quoted\"\" 多字节_abcdefghijklmnopqrstuvwxyz_0123456789\"
     ON \"bridge 名称\" USING hnsw (embedding public.vector_cosine_ops)" >/dev/null
quoted_adopted=$(q "SELECT count(*)
                       FROM pgcontext.adopt_pgvector(
                              '\"bridge 名称\"'::pg_catalog.regclass,
                              false,
                              false
                            )
                      WHERE action = 'created' AND executed")
[[ "${quoted_adopted}" == "1" ]] \
  || fail "quoted/multibyte adoption executed ${quoted_adopted} plans, expected 1"
quoted_replacement=$(q "SELECT count(*)
                          FROM pg_catalog.pg_class AS index_relation
                          JOIN pg_catalog.pg_index AS index
                            ON index.indexrelid = index_relation.oid
                          JOIN pg_catalog.pg_am AS method ON method.oid = index_relation.relam
                         WHERE index.indrelid = '\"bridge 名称\"'::pg_catalog.regclass
                           AND method.amname = 'pgcontext_hnsw'
                           AND pg_catalog.octet_length(index_relation.relname) <= 63")
[[ "${quoted_replacement}" == "1" ]] \
  || fail "quoted/multibyte adoption did not create one byte-bounded replacement"

# Optional binding objects deliberately stay outside extension membership so
# pg_dump emits their DDL. Prove that an enabled binding and its live indexes
# restore into a clean database before exercising hostile-object rejection.
# The configured pg_dump may run as a different OS user (for example,
# `sudo -u postgres pg_dump` in CI). Let this shell create the artifact so the
# user running the gate also owns and can remove it.
${PG_DUMP} -Fc -d "${DB}" > "${DUMP_FILE}"
${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${RESTORE_DB};" \
  -c "CREATE DATABASE ${RESTORE_DB};" >/dev/null
${PG_RESTORE} --exit-on-error --no-owner -d "${RESTORE_DB}" "${DUMP_FILE}"

restored_extensions=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SELECT pg_catalog.string_agg(extname, ',' ORDER BY extname)
     FROM pg_catalog.pg_extension
    WHERE extname IN ('pgcontext', 'vector')")
[[ "${restored_extensions}" == "pgcontext,vector" ]] \
  || fail "restored binding database has unexpected extensions: ${restored_extensions}"
restored_opclasses=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SELECT count(*) FILTER (WHERE pg_catalog.amvalidate(opclass.oid)), count(*)
     FROM pg_catalog.pg_opclass AS opclass
     JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = opclass.opcnamespace
    WHERE namespace.nspname = 'pgcontext'
      AND opclass.opcname LIKE '%_hnsw_pgvector_%_ops'")
[[ "${restored_opclasses}" == "12|12" ]] \
  || fail "restored binding opclass inventory is invalid: ${restored_opclasses}"
restored_nearest=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SET LOCAL enable_seqscan = off;
   SELECT id FROM bridge_vector_docs
    ORDER BY embedding OPERATOR(public.<=>) '[1,0,0]'::public.vector
    LIMIT 1")
[[ "${restored_nearest}" == "1" ]] \
  || fail "restored binding index returned ${restored_nearest}, expected 1"
${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${RESTORE_DB};" >/dev/null
rm -f "${DUMP_FILE}"

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

expect_failure "DROP EXTENSION vector with binding installed" "DROP EXTENSION vector"
expect_failure "DROP EXTENSION pgcontext with binding indexes" "DROP EXTENSION pgcontext"
expect_failure "disable binding while binding indexes exist" \
  "SELECT pgcontext.disable_pgvector_binding()"
q "DO \$sqlstate\$
   BEGIN
     BEGIN
       PERFORM pgcontext.disable_pgvector_binding();
       RAISE EXCEPTION 'disable unexpectedly succeeded';
     EXCEPTION WHEN OTHERS THEN
       IF SQLSTATE <> '2BP01' THEN
         RAISE;
       END IF;
     END;
   END
   \$sqlstate\$" >/dev/null

q "DROP TABLE bridge_vector_docs, bridge_halfvec_docs,
              bridge_sparsevec_docs,
              bridge_sparsevec_oversized,
              bridge_adopt_docs, bridge_unindexed_docs, \"bridge 名称\";
   DROP SCHEMA bridge_attack CASCADE;
   SET SESSION AUTHORIZATION bridge_member_b;
   SET ROLE ${extension_owner};
   SELECT pgcontext.disable_pgvector_binding();
   RESET ROLE;
   RESET SESSION AUTHORIZATION" >/dev/null

remaining_bridge_objects=$(q "SELECT
    (SELECT count(*) FROM pg_catalog.pg_opclass WHERE opcname LIKE '%pgvector%')
  + (SELECT count(*) FROM pg_catalog.pg_proc WHERE proname LIKE '_pgvector_%_support')
  + (SELECT count(*) FROM pg_catalog.pg_cast
      WHERE (castsource = 'public.vector'::pg_catalog.regtype
         AND casttarget = 'pgcontext.vector'::pg_catalog.regtype)
         OR (castsource = 'public.halfvec'::pg_catalog.regtype
         AND casttarget = 'pgcontext.halfvec'::pg_catalog.regtype)
         OR (castsource = 'public.sparsevec'::pg_catalog.regtype
         AND casttarget = 'pgcontext.sparsevec'::pg_catalog.regtype)
         OR (castsource = 'pgcontext.sparsevec'::pg_catalog.regtype
         AND casttarget = 'public.sparsevec'::pg_catalog.regtype))")
[[ "${remaining_bridge_objects}" == "0" ]] \
  || fail "disabling the binding left ${remaining_bridge_objects} compatibility objects"

extensions_left=$(q "SELECT pg_catalog.string_agg(extname, ',' ORDER BY extname)
                       FROM pg_catalog.pg_extension
                      WHERE extname IN ('pgcontext', 'vector')")
[[ "${extensions_left}" == "pgcontext,vector" ]] \
  || fail "binding removal damaged parent extensions: ${extensions_left}"

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

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE ${DB};" \
  -c "DROP ROLE bridge_member_a;" \
  -c "DROP ROLE bridge_member_b;" >/dev/null
echo "pgvector bridge verification passed (12 opclasses, dense binary casts, sparse conversion casts, certification, lifecycle, and clean removal)"
