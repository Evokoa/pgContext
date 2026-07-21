#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_partitioned_collections}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.partitioned_docs (
    id bigint NOT NULL,
    embedding vector NOT NULL,
    tenant text NOT NULL,
    status text NOT NULL,
    body text NOT NULL,
    PRIMARY KEY (tenant, id)
) PARTITION BY LIST (tenant);

CREATE TABLE public.partitioned_docs_acme
    PARTITION OF public.partitioned_docs FOR VALUES IN ('acme');
CREATE TABLE public.partitioned_docs_other
    PARTITION OF public.partitioned_docs FOR VALUES IN ('other');
CREATE TABLE public.partitioned_docs_eu
    PARTITION OF public.partitioned_docs FOR VALUES IN ('eu');

INSERT INTO public.partitioned_docs (id, embedding, tenant, status, body)
VALUES
    (10, '[10,0]'::vector, 'acme', 'open', 'acme far'),
    (20, '[2,0]'::vector, 'other', 'open', 'other near'),
    (30, '[3,0]'::vector, 'acme', 'closed', 'acme near'),
    (40, '[1,0]'::vector, 'eu', 'open', 'eu nearest'),
    (50, '[5,0]'::vector, 'acme', 'open', 'acme middle');

SELECT pgcontext.create_collection('partitioned_docs', 'public.partitioned_docs');
SELECT pgcontext.register_vector('partitioned_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('partitioned_docs', 'tenant', 'tenant');
SELECT pgcontext.register_filter_column('partitioned_docs', 'status', 'status');
SELECT pgcontext.upsert_points('partitioned_docs', ARRAY['10', '20', '30', '40', '50']);

DO $$
DECLARE
    all_order text;
    acme_order text;
    acme_count bigint;
    tenant_facet text;
    source_rows bigint;
BEGIN
    SELECT string_agg(source_key, ',' ORDER BY ordinal)
      INTO all_order
      FROM (
          SELECT row_number() OVER () AS ordinal, source_key
            FROM pgcontext.search('partitioned_docs', '[0,0]'::vector, 5)
      ) rows;
    IF all_order <> '40,20,30,50,10' THEN
        RAISE EXCEPTION 'partitioned exact order mismatch: %', all_order;
    END IF;
    RAISE NOTICE 'partitioned_exact_order_verified';

    SELECT string_agg(source_key, ',' ORDER BY ordinal)
      INTO acme_order
      FROM (
          SELECT row_number() OVER () AS ordinal, source_key
            FROM pgcontext.search(
                'partitioned_docs',
                '[0,0]'::vector,
                '{"must":[{"key":"tenant","match":"acme"}]}',
                5
            )
      ) rows;
    IF acme_order <> '30,50,10' THEN
        RAISE EXCEPTION 'partitioned tenant-filtered order mismatch: %', acme_order;
    END IF;
    RAISE NOTICE 'partitioned_tenant_filter_verified';

    SELECT pgcontext.count(
        'partitioned_docs',
        '{"must":[{"key":"tenant","match":"acme"}]}'
    )
      INTO acme_count;
    IF acme_count <> 3 THEN
        RAISE EXCEPTION 'partitioned tenant count mismatch: %', acme_count;
    END IF;
    RAISE NOTICE 'partitioned_count_verified';

    SELECT string_agg(value || ':' || count::text, ',' ORDER BY count DESC, value)
      INTO tenant_facet
      FROM pgcontext.facet('partitioned_docs', 'tenant', NULL, 10);
    IF tenant_facet <> 'acme:3,eu:1,other:1' THEN
        RAISE EXCEPTION 'partitioned tenant facet mismatch: %', tenant_facet;
    END IF;
    RAISE NOTICE 'partitioned_facet_verified';

    PERFORM pgcontext.delete_points('partitioned_docs', ARRAY['30']);

    SELECT string_agg(source_key, ',' ORDER BY ordinal)
      INTO acme_order
      FROM (
          SELECT row_number() OVER () AS ordinal, source_key
            FROM pgcontext.search(
                'partitioned_docs',
                '[0,0]'::vector,
                '{"must":[{"key":"tenant","match":"acme"}]}',
                5
            )
      ) rows;
    IF acme_order <> '50,10' THEN
        RAISE EXCEPTION 'deleted partitioned point was returned: %', acme_order;
    END IF;
    RAISE NOTICE 'partitioned_delete_visibility_verified';

    DROP TABLE public.partitioned_docs_other;

    SELECT count(*) INTO source_rows FROM public.partitioned_docs;
    IF source_rows <> 4 THEN
        RAISE EXCEPTION 'partition maintenance source row count mismatch: %',
            source_rows;
    END IF;

    SELECT string_agg(source_key, ',' ORDER BY ordinal)
      INTO all_order
      FROM (
          SELECT row_number() OVER () AS ordinal, source_key
            FROM pgcontext.search('partitioned_docs', '[0,0]'::vector, 5)
      ) rows;
    IF all_order <> '40,50,10' THEN
        RAISE EXCEPTION 'stale partition point returned after drop: %', all_order;
    END IF;
    RAISE NOTICE 'partitioned_drop_recheck_verified';
END
$$;

DO $$
BEGIN
    PERFORM *
      FROM pgcontext.search(
          'partitioned_docs',
          '[0,0]'::vector,
          '{"must":[{"key":"missing","match":"acme"}]}',
          1
      );
    RAISE EXCEPTION 'unknown partition filter unexpectedly succeeded';
EXCEPTION WHEN invalid_parameter_value THEN
    RAISE NOTICE 'partitioned_unknown_filter_rejected';
END
$$;
SQL
