#!/usr/bin/env bash
# Live ownership-boundary verification for pgContext and pgvector.
#
# Preconditions: a running PostgreSQL with both extension artifacts installed.
# The bridge extension is tested separately; this gate proves that the main
# pgContext extension owns only canonical pgcontext.* vector types and has no
# catalog dependency on pgvector.
set -euo pipefail

PSQL="${PGCONTEXT_COEXIST_PSQL:-psql}"
DB=pgcontext_coexist_check

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

q() {
  ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq -c "$1"
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

# Install pgvector first, then the canonical main extension.
q "CREATE EXTENSION vector" >/dev/null
q "CREATE EXTENSION pgcontext" >/dev/null

for type_name in vector halfvec sparsevec; do
  public_owner=$(q "SELECT extension.extname
                      FROM pg_catalog.pg_type AS type
                      JOIN pg_catalog.pg_namespace AS namespace
                        ON namespace.oid = type.typnamespace
                      JOIN pg_catalog.pg_depend AS dependency
                        ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                       AND dependency.objid = type.oid
                       AND dependency.deptype = 'e'
                      JOIN pg_catalog.pg_extension AS extension
                        ON extension.oid = dependency.refobjid
                     WHERE namespace.nspname = 'public'
                       AND type.typname = '${type_name}'")
  [[ "${public_owner}" == "vector" ]] \
    || fail "public.${type_name} is owned by '${public_owner}', expected vector"

  canonical_owner=$(q "SELECT extension.extname
                         FROM pg_catalog.pg_type AS type
                         JOIN pg_catalog.pg_namespace AS namespace
                           ON namespace.oid = type.typnamespace
                         JOIN pg_catalog.pg_depend AS dependency
                           ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                          AND dependency.objid = type.oid
                          AND dependency.deptype = 'e'
                         JOIN pg_catalog.pg_extension AS extension
                           ON extension.oid = dependency.refobjid
                        WHERE namespace.nspname = 'pgcontext'
                          AND type.typname = '${type_name}'")
  [[ "${canonical_owner}" == "pgcontext" ]] \
    || fail "pgcontext.${type_name} is owned by '${canonical_owner}', expected pgcontext"

  distinct_oids=$(q "SELECT 'public.${type_name}'::pg_catalog.regtype::oid <>
                            'pgcontext.${type_name}'::pg_catalog.regtype::oid")
  [[ "${distinct_oids}" == "t" ]] \
    || fail "public.${type_name} and pgcontext.${type_name} unexpectedly share an OID"
done

bit_owner=$(q "SELECT extension.extname
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 JOIN pg_catalog.pg_depend AS dependency
                   ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                  AND dependency.objid = type.oid
                  AND dependency.deptype = 'e'
                 JOIN pg_catalog.pg_extension AS extension
                   ON extension.oid = dependency.refobjid
                WHERE namespace.nspname = 'pgcontext'
                  AND type.typname = 'bitvec'")
[[ "${bit_owner}" == "pgcontext" ]] \
  || fail "pgcontext.bitvec is owned by '${bit_owner}', expected pgcontext"

# Neither the extension nor any of its member objects may depend on pgvector.
direct_dependencies=$(q "SELECT count(*)
                           FROM pg_catalog.pg_depend AS dependency
                           JOIN pg_catalog.pg_extension AS source
                             ON dependency.classid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                            AND dependency.objid = source.oid
                           JOIN pg_catalog.pg_extension AS target
                             ON dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                            AND dependency.refobjid = target.oid
                          WHERE source.extname = 'pgcontext'
                            AND target.extname = 'vector'")
[[ "${direct_dependencies}" == "0" ]] \
  || fail "pgcontext extension has ${direct_dependencies} direct dependencies on vector"

member_dependencies=$(q "WITH pgcontext_members AS (
                            SELECT dependency.classid, dependency.objid
                              FROM pg_catalog.pg_depend AS dependency
                              JOIN pg_catalog.pg_extension AS extension
                                ON extension.oid = dependency.refobjid
                             WHERE dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                               AND dependency.deptype = 'e'
                               AND extension.extname = 'pgcontext'
                          ), vector_members AS (
                            SELECT dependency.classid, dependency.objid
                              FROM pg_catalog.pg_depend AS dependency
                              JOIN pg_catalog.pg_extension AS extension
                                ON extension.oid = dependency.refobjid
                             WHERE dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                               AND dependency.deptype = 'e'
                               AND extension.extname = 'vector'
                          )
                          SELECT count(*)
                            FROM pg_catalog.pg_depend AS dependency
                            JOIN pgcontext_members AS source
                              ON source.classid = dependency.classid
                             AND source.objid = dependency.objid
                            JOIN vector_members AS target
                              ON target.classid = dependency.refclassid
                             AND target.objid = dependency.refobjid")
[[ "${member_dependencies}" == "0" ]] \
  || fail "pgcontext member objects have ${member_dependencies} dependencies on vector objects"

# Canonical pgContext types and HNSW remain independently usable.
q "CREATE TABLE canonical_docs (
     id bigint PRIMARY KEY,
     embedding pgcontext.vector(3) NOT NULL
   );
   INSERT INTO canonical_docs VALUES
     (1, '[1,0,0]'::pgcontext.vector),
     (2, '[0,1,0]'::pgcontext.vector),
     (3, '[0,0,1]'::pgcontext.vector);
   CREATE INDEX canonical_docs_hnsw
     ON canonical_docs USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_cosine_ops)" >/dev/null
nearest=$(q "SET LOCAL enable_seqscan = off;
              SELECT id FROM canonical_docs
               ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector
               LIMIT 1")
[[ "${nearest}" == "1" ]] || fail "canonical pgContext ANN returned id ${nearest}, expected 1"

# F0 inventory continues to see pgvector-owned columns without binding core
# pgContext objects to them. Existing-column serving is the bridge's job.
q "CREATE TABLE pgvector_docs (
     id bigint PRIMARY KEY,
     embedding public.vector(3),
     embedding_array public.vector(3)[]
   );
   CREATE INDEX pgvector_docs_hnsw
     ON pgvector_docs USING hnsw (embedding vector_cosine_ops)
     WITH (m=8, ef_construction=40)" >/dev/null
report_rows=$(q "SELECT count(*)
                   FROM pgcontext.migration_report()
                  WHERE table_name = 'pgvector_docs'")
[[ "${report_rows}" -eq 2 ]] \
  || fail "migration_report returned ${report_rows} pgvector_docs rows, expected 2"
array_blocker=$(q "SELECT blockers::text
                     FROM pgcontext.migration_report()
                    WHERE table_name = 'pgvector_docs'
                      AND column_name = 'embedding_array'")
[[ "${array_blocker}" == *"array columns require element-wise conversion"* ]] \
  || fail "array dependency blocker missing: ${array_blocker}"

# Removing pgvector must not cascade into, or disable, canonical pgContext.
q "DROP TABLE pgvector_docs; DROP EXTENSION vector" >/dev/null
pgcontext_present=$(q "SELECT count(*) FROM pg_catalog.pg_extension WHERE extname = 'pgcontext'")
[[ "${pgcontext_present}" == "1" ]] || fail "dropping vector removed pgcontext"
nearest_after_drop=$(q "SELECT id FROM canonical_docs
                         ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector
                         LIMIT 1")
[[ "${nearest_after_drop}" == "1" ]] \
  || fail "canonical pgContext query failed after DROP EXTENSION vector"

# Reverse install order is also valid now that the type namespaces are disjoint.
q "CREATE EXTENSION vector" >/dev/null
reverse_distinct=$(q "SELECT 'public.vector'::pg_catalog.regtype::oid <>
                             'pgcontext.vector'::pg_catalog.regtype::oid")
[[ "${reverse_distinct}" == "t" ]] || fail "reverse install order did not preserve distinct vector types"

q "CREATE TABLE pgvector_survivor (
     id bigint PRIMARY KEY,
     embedding public.vector(3) NOT NULL
   );
   INSERT INTO pgvector_survivor VALUES
     (1, '[1,0,0]'::public.vector),
     (2, '[0,1,0]'::public.vector);
   CREATE INDEX pgvector_survivor_hnsw
     ON pgvector_survivor USING hnsw (embedding vector_cosine_ops)" >/dev/null

if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 \
  -c "DROP EXTENSION pgcontext" \
  >"${TMPDIR:-/tmp}/pgcontext-coexist-restrict.out" 2>&1; then
  fail "DROP EXTENSION pgcontext unexpectedly ignored canonical table dependencies"
fi

q "DROP TABLE canonical_docs; DROP EXTENSION pgcontext" >/dev/null
vector_present=$(q "SELECT count(*) FROM pg_catalog.pg_extension WHERE extname = 'vector'")
[[ "${vector_present}" == "1" ]] || fail "dropping pgcontext removed vector"
pgvector_nearest=$(q "SET LOCAL enable_seqscan = off;
                       SELECT id FROM pgvector_survivor
                        ORDER BY embedding OPERATOR(public.<=>) '[1,0,0]'::public.vector
                        LIMIT 1")
[[ "${pgvector_nearest}" == "1" ]] \
  || fail "pgvector query failed after DROP EXTENSION pgcontext"

${PSQL} -d postgres -c "DROP DATABASE ${DB};" >/dev/null
echo "pgvector coexist verification passed (canonical ownership, zero dependency, both install orders, symmetric drops)"
