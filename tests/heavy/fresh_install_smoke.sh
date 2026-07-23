#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_fresh_install_smoke}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

hostile_role=pgcontext_hostile_schema_owner
drop_role_if_exists "${hostile_role}"
psql_postgres -c "CREATE ROLE ${hostile_role}"
psql_db -c "CREATE SCHEMA pgcontext AUTHORIZATION ${hostile_role}" \
    -c "GRANT CREATE ON SCHEMA pgcontext TO PUBLIC"
if psql_db -c "CREATE EXTENSION pgcontext" \
    >"${HEAVY_TMPDIR}/fresh_install_hostile_schema.out" 2>&1; then
    echo "pgcontext unexpectedly installed into a hostile schema" >&2
    exit 1
fi
grep -q 'pgcontext schema must be owned by the extension installer' \
    "${HEAVY_TMPDIR}/fresh_install_hostile_schema.out"
psql_db -c "DROP SCHEMA pgcontext"
drop_role_if_exists "${hostile_role}"

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL,
    tenant text NOT NULL
);

INSERT INTO public.docs (id, embedding, body, tenant)
VALUES
    (1, '[0,0]'::vector, 'database internals', 'acme'),
    (2, '[1,0]'::vector, 'query planning', 'acme'),
    (3, '[5,5]'::vector, 'gardening notes', 'other');

SELECT * FROM pgcontext.create_collection('fresh_docs', 'public.docs');
SELECT * FROM pgcontext.register_vector('fresh_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.register_filter_column('fresh_docs', 'tenant', 'tenant');
SELECT * FROM pgcontext.upsert_points('fresh_docs', ARRAY['1', '2', '3']);

DO $$
DECLARE
    nearest_source_key text;
    filtered_count bigint;
    query_count bigint;
BEGIN
    SELECT source_key
      INTO nearest_source_key
      FROM pgcontext.search('fresh_docs', '[0,0]'::vector, 1);

    IF nearest_source_key <> '1' THEN
        RAISE EXCEPTION 'unexpected nearest source key: %', nearest_source_key;
    END IF;

    SELECT count(*)
      INTO filtered_count
      FROM pgcontext.search(
          'fresh_docs',
          '[0,0]'::vector,
          '{"must":[{"key":"tenant","match":"acme"}]}',
          10
      );

    IF filtered_count <> 2 THEN
        RAISE EXCEPTION 'unexpected filtered result count: %', filtered_count;
    END IF;

    SELECT count(*)
      INTO query_count
      FROM pgcontext.query('fresh_docs', '[0,0]'::vector, 'database', 'body', 10);

    IF query_count < 1 THEN
        RAISE EXCEPTION 'hybrid query returned no rows';
    END IF;
END
$$;

DROP TABLE public.docs;
DROP EXTENSION pgcontext;
SQL
