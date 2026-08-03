#!/usr/bin/env bash
# Conflict-safe pgvector-spelled access-method and opclass facade gate.
set -euo pipefail

PSQL=${PGCONTEXT_FACADE_PSQL:-psql}
PG_DUMP=${PGCONTEXT_FACADE_PG_DUMP:-pg_dump}
PG_RESTORE=${PGCONTEXT_FACADE_PG_RESTORE:-pg_restore}
DB=${PGCONTEXT_FACADE_DB:-pgcontext_pgvector_facade_check}
RESTORE_DB=${PGCONTEXT_FACADE_RESTORE_DB:-${DB}_restore}
DUMP_FILE=${TMPDIR:-/tmp}/${DB}.dump

for database_name in "${DB}" "${RESTORE_DB}"; do
  if [[ ! "${database_name}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "FAIL: facade database names must be simple SQL identifiers" >&2
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

expect_failure_matching() {
  local description=$1
  local sql=$2
  local expected=$3
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-facade-failure-${DB}.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
  if ! grep -Fq "${expected}" "${output_file}"; then
    fail "${description} failed for the wrong reason; expected '${expected}'"
  fi
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

q "CREATE EXTENSION pgcontext;
   SELECT pgcontext.enable_pgvector_name_facade();
   SELECT pgcontext.enable_pgvector_name_facade();
   SET search_path = pgcontext, public;
   CREATE TABLE public.facade_items (
     id bigint PRIMARY KEY,
     embedding vector(3) NOT NULL
   );
   INSERT INTO public.facade_items VALUES
     (1, '[1,0,0]'), (2, '[0,1,0]'), (3, '[0,0,1]');
   CREATE INDEX facade_hnsw
     ON public.facade_items USING hnsw (embedding vector_cosine_ops);
   CREATE INDEX facade_ivfflat
     ON public.facade_items USING ivfflat (embedding vector_l2_ops) WITH (lists = 1)" >/dev/null

bindings=$(q "SELECT pg_catalog.string_agg(
                       index_relation.relname || ':' || access_method.amname || ':' || opclass.opcname,
                       ',' ORDER BY index_relation.relname
                     )
                FROM pg_catalog.pg_class AS index_relation
                JOIN pg_catalog.pg_index AS index
                  ON index.indexrelid = index_relation.oid
                JOIN pg_catalog.pg_am AS access_method
                  ON access_method.oid = index_relation.relam
                JOIN pg_catalog.pg_opclass AS opclass
                  ON opclass.oid = index.indclass[0]
               WHERE index_relation.relname IN ('facade_hnsw', 'facade_ivfflat')")
[[ "${bindings}" == "facade_hnsw:hnsw:vector_cosine_ops,facade_ivfflat:ivfflat:vector_l2_ops" ]] \
  || fail "facade indexes have unexpected bindings: ${bindings}"

nearest=$(q "SET LOCAL search_path = pgcontext, public;
              SET LOCAL enable_seqscan = off;
              SELECT id FROM public.facade_items
               ORDER BY embedding <=> '[1,0,0]'::vector
               LIMIT 1")
[[ "${nearest}" == "1" ]] || fail "facade query returned ${nearest}, expected 1"

ivf_nearest=$(q "SET LOCAL search_path = pgcontext, public;
                  SET LOCAL enable_seqscan = off;
                  SELECT id FROM public.facade_items
                   ORDER BY embedding <-> '[0,1,0]'::vector
                   LIMIT 1")
[[ "${ivf_nearest}" == "2" ]] \
  || fail "IVFFlat facade query returned ${ivf_nearest}, expected 2"

inventory=$(q "SELECT count(*) = 12
                 FROM pgcontext.pgvector_compatibility_inventory()
                WHERE status IN ('compatible', 'translated', 'unsupported')")
[[ "${inventory}" == "t" ]] || fail "published compatibility inventory is incomplete"

# Facade access methods and opclasses are standalone, dump-visible objects.
# Verify an enabled facade plus live indexes round-trips into an empty database.
${PG_DUMP} -Fc -d "${DB}" -f "${DUMP_FILE}"
${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${RESTORE_DB};" \
  -c "CREATE DATABASE ${RESTORE_DB};" >/dev/null
${PG_RESTORE} --exit-on-error --no-owner -d "${RESTORE_DB}" "${DUMP_FILE}"

restored_methods=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SELECT count(*)
     FROM pg_catalog.pg_am AS facade
     JOIN pg_catalog.pg_am AS native
       ON native.amname = CASE facade.amname
                           WHEN 'hnsw' THEN 'pgcontext_hnsw'
                           WHEN 'ivfflat' THEN 'pgcontext_ivfflat'
                         END
    WHERE facade.amname IN ('hnsw', 'ivfflat')
      AND facade.amhandler = native.amhandler")
[[ "${restored_methods}" == "2" ]] \
  || fail "restored facade access methods have invalid handlers: ${restored_methods}"
restored_opclasses=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SELECT count(*) FILTER (WHERE pg_catalog.amvalidate(opclass.oid)), count(*)
     FROM pg_catalog.pg_opclass AS opclass
     JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = opclass.opcnamespace
     JOIN pg_catalog.pg_am AS method ON method.oid = opclass.opcmethod
    WHERE namespace.nspname = 'pgcontext'
      AND method.amname IN ('hnsw', 'ivfflat')")
[[ "${restored_opclasses}" == "24|24" ]] \
  || fail "restored facade opclass inventory is invalid: ${restored_opclasses}"
restored_nearest=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq -c \
  "SET LOCAL search_path = pgcontext, public;
   SET LOCAL enable_seqscan = off;
   SELECT id FROM public.facade_items
    ORDER BY embedding <=> '[1,0,0]'::vector
    LIMIT 1")
[[ "${restored_nearest}" == "1" ]] \
  || fail "restored facade index returned ${restored_nearest}, expected 1"
${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${RESTORE_DB};" >/dev/null
rm -f "${DUMP_FILE}"

expect_failure_matching "disable facade with live indexes" \
  "SELECT pgcontext.disable_pgvector_name_facade()" \
  "cannot drop"
q "DO \$sqlstate\$
   BEGIN
     BEGIN
       PERFORM pgcontext.disable_pgvector_name_facade();
       RAISE EXCEPTION 'disable unexpectedly succeeded';
     EXCEPTION WHEN OTHERS THEN
       IF SQLSTATE <> '2BP01' THEN
         RAISE;
       END IF;
     END;
   END
   \$sqlstate\$" >/dev/null

q "DROP TABLE public.facade_items;
   SELECT pgcontext.disable_pgvector_name_facade();
   SELECT pgcontext.disable_pgvector_name_facade()" >/dev/null

remaining=$(q "SELECT count(*) FROM pg_catalog.pg_am
                WHERE amname IN ('hnsw', 'ivfflat')")
[[ "${remaining}" == "0" ]] || fail "facade access methods were not removed"

${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${DB};" >/dev/null
echo "pgvector name facade verification passed (conflict-safe AM aliases, pgvector opclass spellings, lifecycle, inventory)"
