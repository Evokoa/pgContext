#!/usr/bin/env bash
# Live pgvector-coexist verification against an installed pgContext.
#
# Preconditions: a running PostgreSQL with BOTH extensions installed
# (pgvector as `vector`, pgContext with the coexist transform applied to
# its installed SQL artifact). Creates and drops its own database.
#
# Environment:
#   PGCONTEXT_COEXIST_PSQL  psql invocation (default: psql). CI uses
#                           "sudo -u postgres psql"; macOS dev boxes use
#                           /opt/homebrew/opt/postgresql@17/bin/psql.
set -euo pipefail

PSQL="${PGCONTEXT_COEXIST_PSQL:-psql}"
DB=pgcontext_coexist_check

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

q() { # query -> single-value/rows output, errors fatal
  ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq -c "$1"
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

# 1. Install order: pgvector first, pgContext second (coexist mode).
q "CREATE EXTENSION vector" >/dev/null
q "CREATE EXTENSION pgcontext" >/dev/null

# In coexist mode our conflicting types must NOT have been created: the
# public type names stay pgvector's.
vector_owner=$(q "SELECT e.extname FROM pg_type t
                    JOIN pg_depend d ON d.classid='pg_type'::regclass AND d.objid=t.oid AND d.deptype='e'
                    JOIN pg_extension e ON e.oid=d.refobjid
                   WHERE t.typname='vector' AND t.typnamespace='public'::regnamespace")
[[ "${vector_owner}" == "vector" ]] || fail "public.vector is owned by '${vector_owner}', expected pgvector"

# 2. Data in pgvector's types.
q "CREATE TABLE docs (id bigint PRIMARY KEY, embedding vector(8), hemb halfvec(8))" >/dev/null
q "INSERT INTO docs
   SELECT n,
          (SELECT ('[' || string_agg(((n*7+d) % 10)::text, ',') || ']') FROM generate_series(1,8) d)::vector,
          (SELECT ('[' || string_agg(((n*7+d) % 10)::text || '.5', ',') || ']') FROM generate_series(1,8) d)::halfvec
     FROM generate_series(1, 300) n" >/dev/null

# 3. pgContext index directly on the pgvector-typed column; the advisory
# nudge NOTICE must fire (once) and the GUC must silence it.
notice=$(${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c \
  "CREATE INDEX docs_pgc ON docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)" 2>&1)
grep -q "pgcontext: this index serves a column typed by the pgvector extension" <<<"${notice}" \
  || fail "coexist nudge NOTICE did not fire: ${notice}"
silent=$(${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c \
  "SET pgcontext.pgvector_compat_warnings = off; DROP INDEX docs_pgc;
   CREATE INDEX docs_pgc ON docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)" 2>&1)
grep -q "pgcontext:" <<<"${silent}" && fail "nudge NOTICE fired despite the GUC being off"

# 4. ANN over their column matches the exact same-operator oracle.
ann=$(q "SET LOCAL enable_seqscan = off;
         SELECT string_agg(id::text, ',') FROM (
           SELECT id FROM docs ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 3
         ) t")
exact=$(q "SET LOCAL enable_indexscan = off; SET LOCAL enable_bitmapscan = off;
           SELECT string_agg(id::text, ',') FROM (
             SELECT id FROM docs ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 3
           ) t")
[[ "${ann}" == "${exact}" ]] || fail "ANN top-3 '${ann}' != exact oracle '${exact}'"

# 5. halfvec coexist serving: binary16 layout parity.
half_zero=$(q "SELECT pgcontext.halfvec_l2_distance('[1.5,2.5,3.5,4.5,5.5,6.5,7.5,8.5]'::halfvec, hemb)
                 FROM docs ORDER BY 1 LIMIT 1")
[[ "${half_zero}" == "0" ]] || fail "halfvec nearest distance '${half_zero}', expected exact 0"
half_rt=$(q "SELECT hemb = (pgcontext.halfvec_to_vector(hemb)::text::halfvec) FROM docs LIMIT 1")
[[ "${half_rt}" == "t" ]] || fail "halfvec round trip through pgcontext failed"

# 6. migration_report sees the columns; adopt_pgvector migrates indexing.
q "CREATE INDEX docs_pgv ON docs USING hnsw (embedding vector_cosine_ops)" >/dev/null
report_rows=$(q "SELECT count(*) FROM pgcontext.migration_report() WHERE table_name='docs'")
[[ "${report_rows}" -ge 1 ]] || fail "migration_report returned no docs rows"
dry=$(q "SELECT count(*) FROM pgcontext.adopt_pgvector('docs') WHERE NOT executed")
[[ "${dry}" -ge 1 ]] || fail "adopt_pgvector dry run proposed nothing"

# 7. compare_indexes measures both families on the shared column.
measured=$(q "SET pgcontext.pgvector_compat_warnings = off;
              SELECT count(*) FROM pgcontext.compare_indexes('docs','embedding',5)
               WHERE p50_ms IS NOT NULL AND recall_at_10 IS NOT NULL")
[[ "${measured}" -eq 2 ]] || fail "compare_indexes measured ${measured} indexes, expected 2"

${PSQL} -d postgres -c "DROP DATABASE ${DB};" >/dev/null
echo "pgvector coexist verification passed (types, nudge+GUC, ANN=oracle, halfvec parity, report/adopt, compare_indexes)"
