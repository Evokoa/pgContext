#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_integer_vector_lifecycle}"
RESTORE_DB="${RESTORE_DB:-pgcontext_integer_vector_restore}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

dump_path="${HEAVY_TMPDIR}/${DBNAME}.dump"
binary_path="${HEAVY_TMPDIR}/${DBNAME}.binary"
oversized_binary_path="${HEAVY_TMPDIR}/${DBNAME}.oversized.binary"
cleanup() {
    rm -f "${dump_path}" "${binary_path}" "${oversized_binary_path}"
}
trap cleanup EXIT

python3 - "${oversized_binary_path}" <<'PY'
import struct
import sys

payload = b"\xa1\x66values\x99\x3e\x81" + (b"\x00" * 16_001)
copy_stream = (
    b"PGCOPY\n\xff\r\n\x00"
    + struct.pack("!II", 0, 0)
    + struct.pack("!hI", 1, len(payload))
    + payload
    + struct.pack("!h", -1)
)
with open(sys.argv[1], "wb") as output:
    output.write(copy_stream)
PY

start_and_install_extension
reset_database
drop_database "${RESTORE_DB}"

psql_db -v binary_copy_path="'${binary_path}'" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.integer_vectors (
    id bigint PRIMARY KEY,
    signed_embedding pgcontext.int8vec(3) NOT NULL,
    unsigned_embedding pgcontext.uint8vec(3) NOT NULL,
    provider_embedding pgcontext.bitvec(10) NOT NULL
);

CREATE INDEX integer_vectors_signed_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (signed_embedding pgcontext.int8vec_hnsw_ops);
CREATE INDEX integer_vectors_unsigned_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (unsigned_embedding pgcontext.uint8vec_hnsw_ops);
CREATE INDEX integer_vectors_provider_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (provider_embedding pgcontext.bitvec_hnsw_hamming_ops);

SELECT pgcontext.create_collection('integer_profiles', 'public.integer_vectors');
SELECT pgcontext.register_embedding_profile(
    'integer_profiles',
    'provider_int8_v1',
    'signed_embedding',
    'public.integer_vectors_signed_hnsw',
    '{
       "representation":"int8",
       "dimensions":3,
       "normalization":"none",
       "metric":"l2",
       "provider":"fixture-provider",
       "model":"fixture-model",
       "revision":"signed-v1",
       "input_template":"document: {text}",
       "output_template":"int8[3]",
       "bit_order":null,
       "byte_order":null,
       "scale":0.5,
       "zero_point":0,
       "configuration_hash":"0000000000000006"
     }'::jsonb
);
SELECT pgcontext.register_embedding_profile(
    'integer_profiles',
    'provider_uint8_v1',
    'unsigned_embedding',
    'public.integer_vectors_unsigned_hnsw',
    '{
       "representation":"uint8",
       "dimensions":3,
       "normalization":"none",
       "metric":"l2",
       "provider":"fixture-provider",
       "model":"fixture-model",
       "revision":"unsigned-v1",
       "input_template":"document: {text}",
       "output_template":"uint8[3]",
       "bit_order":null,
       "byte_order":null,
       "scale":0.5,
       "zero_point":128,
       "configuration_hash":"0000000000000007"
     }'::jsonb
);
SELECT pgcontext.register_embedding_profile(
    'integer_profiles',
    'provider_binary_v1',
    'provider_embedding',
    'public.integer_vectors_provider_hnsw',
    '{
       "representation":"bit",
       "dimensions":10,
       "normalization":"none",
       "metric":"hamming",
       "provider":"fixture-provider",
       "model":"fixture-binary-model",
       "revision":"binary-v1",
       "input_template":"document: {text}",
       "output_template":"packed-bits[10]",
       "bit_order":"msb_first",
       "byte_order":"msb_first",
       "scale":null,
       "zero_point":null,
       "configuration_hash":"0000000000000008"
     }'::jsonb
);

INSERT INTO public.integer_vectors
SELECT n,
       pgcontext.int8vec_from_profile(
           'integer_profiles', 'provider_int8_v1',
           ARRAY[(n % 127), ((n * 3) % 127), ((n * 7) % 127)]::smallint[]
       ),
       pgcontext.uint8vec_from_profile(
           'integer_profiles', 'provider_uint8_v1',
           ARRAY[(n % 255), ((n * 3) % 255), ((n * 7) % 255)]::smallint[]
       ),
       pgcontext.bitvec_from_provider_bytes(
           'integer_profiles', 'provider_binary_v1',
           decode(lpad(to_hex((n % 1024) << 6), 4, '0'), 'hex')
       )
  FROM generate_series(1, 100) AS n;

CREATE INDEX integer_vectors_signed_ip_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (signed_embedding pgcontext.int8vec_hnsw_ip_ops);
CREATE INDEX integer_vectors_signed_cosine_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (signed_embedding pgcontext.int8vec_hnsw_cosine_ops);
CREATE INDEX integer_vectors_signed_l1_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (signed_embedding pgcontext.int8vec_hnsw_l1_ops);
CREATE INDEX integer_vectors_unsigned_ip_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (unsigned_embedding pgcontext.uint8vec_hnsw_ip_ops);
CREATE INDEX integer_vectors_unsigned_cosine_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (unsigned_embedding pgcontext.uint8vec_hnsw_cosine_ops);
CREATE INDEX integer_vectors_unsigned_l1_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (unsigned_embedding pgcontext.uint8vec_hnsw_l1_ops);

DO $$
DECLARE
    validated bigint;
BEGIN
    SELECT count(*) INTO validated
      FROM pg_catalog.pg_opclass AS opclass
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = opclass.opcnamespace
     WHERE namespace.nspname = 'pgcontext'
       AND opclass.opcname IN (
           'int8vec_hnsw_ops', 'int8vec_hnsw_ip_ops',
           'int8vec_hnsw_cosine_ops', 'int8vec_hnsw_l1_ops',
           'uint8vec_hnsw_ops', 'uint8vec_hnsw_ip_ops',
           'uint8vec_hnsw_cosine_ops', 'uint8vec_hnsw_l1_ops'
       )
       AND pg_catalog.amvalidate(opclass.oid);
    IF validated <> 8 THEN
        RAISE EXCEPTION 'expected all eight integer HNSW opclasses to validate, saw %', validated;
    END IF;
END
$$;

CREATE TEMP TABLE integer_vectors_binary_roundtrip (
    signed_embedding pgcontext.int8vec(3) NOT NULL,
    unsigned_embedding pgcontext.uint8vec(3) NOT NULL,
    provider_embedding pgcontext.bitvec(10) NOT NULL
);
CREATE TABLE public.integer_vectors_wrong_typmod (
    signed_embedding pgcontext.int8vec(2) NOT NULL,
    unsigned_embedding pgcontext.uint8vec(2) NOT NULL,
    provider_embedding pgcontext.bitvec(9) NOT NULL
);
CREATE TABLE public.integer_vectors_oversized_binary (
    signed_embedding pgcontext.int8vec NOT NULL
);
COPY (
    SELECT signed_embedding, unsigned_embedding, provider_embedding
      FROM public.integer_vectors
     ORDER BY id
     LIMIT 5
) TO :binary_copy_path WITH (FORMAT binary);
COPY integer_vectors_binary_roundtrip
  FROM :binary_copy_path WITH (FORMAT binary);
DO $$
DECLARE
    mismatch bigint;
BEGIN
    SELECT count(*) INTO mismatch
      FROM (
          (SELECT signed_embedding, unsigned_embedding, provider_embedding
             FROM public.integer_vectors
            ORDER BY id
            LIMIT 5)
          EXCEPT ALL
          SELECT signed_embedding, unsigned_embedding, provider_embedding
            FROM integer_vectors_binary_roundtrip
      ) AS differences;
    IF mismatch <> 0 THEN
        RAISE EXCEPTION 'provider-native binary COPY roundtrip changed % rows', mismatch;
    END IF;
END
$$;

ALTER TABLE pgcontext._embedding_profiles
    DISABLE TRIGGER embedding_profiles_immutable;
DO $$
DECLARE
    rejected integer := 0;
BEGIN
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET metric = 'l2'
         WHERE profile_name = 'provider_binary_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET metric = 'hamming'
         WHERE profile_name = 'provider_uint8_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET bit_order = 'msb_first'
         WHERE profile_name = 'provider_uint8_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET scale = NULL
         WHERE profile_name = 'provider_uint8_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET zero_point = NULL
         WHERE profile_name = 'provider_uint8_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    BEGIN
        UPDATE pgcontext._embedding_profiles
           SET zero_point = 256
         WHERE profile_name = 'provider_uint8_v1';
    EXCEPTION WHEN check_violation THEN rejected := rejected + 1;
    END;
    IF rejected <> 6 THEN
        RAISE EXCEPTION 'embedding profile SQL constraints rejected only 6/% invalid states', rejected;
    END IF;
END
$$;
ALTER TABLE pgcontext._embedding_profiles
    ENABLE TRIGGER embedding_profiles_immutable;

DO $$
DECLARE
    profile jsonb;
    rejected integer := 0;
BEGIN
    SELECT pgcontext.embedding_profile_explain(
        'integer_profiles', 'provider_uint8_v1'
    )->'profile' INTO profile;
    BEGIN
        PERFORM pgcontext.register_embedding_profile(
            'integer_profiles', 'bad_source_binding', 'signed_embedding',
            'public.integer_vectors_signed_hnsw', profile
        );
    EXCEPTION WHEN datatype_mismatch THEN rejected := rejected + 1;
    END;
    BEGIN
        PERFORM pgcontext.register_embedding_profile(
            'integer_profiles', 'bad_index_binding', 'unsigned_embedding',
            'public.integer_vectors_signed_hnsw', profile
        );
    EXCEPTION WHEN wrong_object_type THEN rejected := rejected + 1;
    END;
    IF rejected <> 2 THEN
        RAISE EXCEPTION 'profile binding validation rejected only 2/% mismatches', rejected;
    END IF;
END
$$;

DROP INDEX public.integer_vectors_unsigned_hnsw;
DO $$
DECLARE
    binding_valid boolean;
    stale_warning boolean;
BEGIN
    SELECT (pgcontext.embedding_profile_explain(
        'integer_profiles', 'provider_uint8_v1'
    )->>'binding_valid')::boolean INTO binding_valid;
    SELECT coalesce(pg_catalog.bool_or(
               recommendation = 'TuneHnswSettings'
               AND detail LIKE '%stale embedding-profile HNSW binding%'
           ), false)
      INTO stale_warning
      FROM pgcontext.index_advisor('integer_profiles');
    IF binding_valid THEN
        RAISE EXCEPTION 'dropped uint8 profile index still reports a valid binding';
    END IF;
    IF NOT stale_warning THEN
        RAISE EXCEPTION 'advisor missed a stale uint8 binding hidden by unrelated HNSW indexes';
    END IF;
END
$$;
CREATE INDEX integer_vectors_unsigned_hnsw
    ON public.integer_vectors USING pgcontext_hnsw
    (unsigned_embedding pgcontext.uint8vec_hnsw_ops);

DO $$
DECLARE
    signed_sum bigint[];
    signed_avg double precision[];
    unsigned_sum bigint[];
    unsigned_avg double precision[];
    profile_explain jsonb;
    advisor_false_unbound boolean;
    binary_protocol_types bigint;
BEGIN
    SELECT pgcontext.sum(value), pgcontext.avg(value)
      INTO signed_sum, signed_avg
      FROM (VALUES
          (pgcontext.int8vec('[1,2]')),
          (pgcontext.int8vec('[3,4]'))
      ) AS values(value);
    SELECT pgcontext.sum(value), pgcontext.avg(value)
      INTO unsigned_sum, unsigned_avg
      FROM (VALUES
          (pgcontext.uint8vec('[0,128]')),
          (pgcontext.uint8vec('[2,255]'))
      ) AS values(value);
    SELECT pgcontext.embedding_profile_explain(
        'integer_profiles', 'provider_uint8_v1'
    ) INTO profile_explain;
    SELECT coalesce(pg_catalog.bool_or(
               detail LIKE '%no registered pgcontext_hnsw index%'
           ), false)
      INTO advisor_false_unbound
      FROM pgcontext.index_advisor('integer_profiles');
    SELECT count(*) INTO binary_protocol_types
      FROM pg_catalog.pg_type AS types
      JOIN pg_catalog.pg_namespace AS namespaces
        ON namespaces.oid = types.typnamespace
     WHERE namespaces.nspname = 'pgcontext'
       AND types.typname IN ('int8vec', 'uint8vec', 'bitvec')
       AND types.typreceive <> 0
       AND types.typsend <> 0;

    IF signed_sum <> ARRAY[4,6]::bigint[]
       OR signed_avg <> ARRAY[2,3]::double precision[]
       OR unsigned_sum <> ARRAY[2,383]::bigint[]
       OR unsigned_avg <> ARRAY[1,191.5]::double precision[] THEN
        RAISE EXCEPTION 'integer aggregate contract failed: %/%/%/%',
            signed_sum, signed_avg, unsigned_sum, unsigned_avg;
    END IF;
    IF profile_explain->>'source_authority' <> 'provider_native'
       OR profile_explain->>'exact_score_representation' <> 'uint8'
       OR profile_explain->>'hnsw_opclass' <> 'uint8vec_hnsw_ops'
       OR profile_explain->>'binding_valid' <> 'true'
       OR profile_explain->>'source_column' <> 'public.integer_vectors.unsigned_embedding'
       OR profile_explain->>'hnsw_index' <> 'public.integer_vectors_unsigned_hnsw'
       OR profile_explain->>'final_score' <> 'authoritative_source' THEN
        RAISE EXCEPTION 'embedding profile explain contract failed: %', profile_explain;
    END IF;
    IF advisor_false_unbound THEN
        RAISE EXCEPTION 'index advisor falsely reported bound provider-native profiles as unbound';
    END IF;
    IF binary_protocol_types <> 3 THEN
        RAISE EXCEPTION 'expected binary send/receive for three provider-native types, saw %',
            binary_protocol_types;
    END IF;
END
$$;

DO $$
DECLARE
    padding_rejected boolean := false;
    mutation_rejected boolean := false;
BEGIN
    BEGIN
        PERFORM ARRAY[128]::integer[]::pgcontext.int8vec;
        RAISE EXCEPTION 'int8vec accepted an out-of-range coordinate';
    EXCEPTION WHEN numeric_value_out_of_range THEN NULL;
    END;
    BEGIN
        PERFORM pgcontext.bitvec_from_provider_bytes(
            'integer_profiles', 'provider_binary_v1', decode('a001', 'hex')
        );
    EXCEPTION WHEN OTHERS THEN
        padding_rejected := true;
    END;
    IF NOT padding_rejected THEN
        RAISE EXCEPTION 'provider binary import accepted nonzero padding';
    END IF;
    BEGIN
        UPDATE pgcontext._embedding_profiles SET dimensions = 4;
    EXCEPTION WHEN object_not_in_prerequisite_state THEN
        mutation_rejected := true;
    END;
    IF NOT mutation_rejected THEN
        RAISE EXCEPTION 'embedding profile catalog accepted mutation';
    END IF;
END
$$;

SET enable_seqscan = off;
DO $$
DECLARE
    signed_id bigint;
    unsigned_id bigint;
BEGIN
    SELECT id INTO signed_id
      FROM public.integer_vectors
     ORDER BY signed_embedding OPERATOR(pgcontext.<->)
              pgcontext.int8vec('[42,126,40]')
     LIMIT 1;
    SELECT id INTO unsigned_id
      FROM public.integer_vectors
     ORDER BY unsigned_embedding OPERATOR(pgcontext.<->)
              pgcontext.uint8vec('[42,126,39]')
     LIMIT 1;
    IF signed_id <> 42 OR unsigned_id <> 42 THEN
        RAISE EXCEPTION 'integer HNSW exact source recheck returned signed %, unsigned %',
            signed_id, unsigned_id;
    END IF;
END
$$;

DO $$
DECLARE
    test_case record;
    exact_id bigint;
    indexed_id bigint;
    plan json;
    query_sql text;
BEGIN
    FOR test_case IN
        SELECT * FROM (VALUES
            ('signed_embedding', 'int8vec', '<->', '[42,126,40]'),
            ('signed_embedding', 'int8vec', '<#>', '[42,126,40]'),
            ('signed_embedding', 'int8vec', '<=>', '[42,126,40]'),
            ('signed_embedding', 'int8vec', '<+>', '[42,126,40]'),
            ('unsigned_embedding', 'uint8vec', '<->', '[42,126,39]'),
            ('unsigned_embedding', 'uint8vec', '<#>', '[42,126,39]'),
            ('unsigned_embedding', 'uint8vec', '<=>', '[42,126,39]'),
            ('unsigned_embedding', 'uint8vec', '<+>', '[42,126,39]')
        ) AS cases(column_name, type_name, operator_name, query_value)
    LOOP
        query_sql := pg_catalog.format(
            'SELECT id FROM public.integer_vectors ORDER BY %I OPERATOR(pgcontext.%s) %L::pgcontext.%I, id LIMIT 1',
            test_case.column_name,
            test_case.operator_name,
            test_case.query_value,
            test_case.type_name
        );
        PERFORM pg_catalog.set_config('enable_indexscan', 'off', true);
        PERFORM pg_catalog.set_config('enable_bitmapscan', 'off', true);
        PERFORM pg_catalog.set_config('enable_seqscan', 'on', true);
        EXECUTE query_sql INTO exact_id;

        PERFORM pg_catalog.set_config('enable_indexscan', 'on', true);
        PERFORM pg_catalog.set_config('enable_bitmapscan', 'on', true);
        PERFORM pg_catalog.set_config('enable_seqscan', 'off', true);
        EXECUTE query_sql INTO indexed_id;
        EXECUTE 'EXPLAIN (FORMAT JSON) ' || query_sql INTO plan;
        IF indexed_id IS DISTINCT FROM exact_id THEN
            RAISE EXCEPTION 'integer HNSW % % returned %, exact oracle %',
                test_case.type_name, test_case.operator_name, indexed_id, exact_id;
        END IF;
        IF plan::text NOT LIKE '%Index Scan%' THEN
            RAISE EXCEPTION 'integer HNSW % % did not use an index: %',
                test_case.type_name, test_case.operator_name, plan;
        END IF;
    END LOOP;
END
$$;

DROP ROLE IF EXISTS pgcontext_integer_profile_reader;
DROP ROLE IF EXISTS pgcontext_integer_profile_owner;
CREATE ROLE pgcontext_integer_profile_owner;
CREATE ROLE pgcontext_integer_profile_reader;
UPDATE pgcontext._collections
   SET owner_role = 'pgcontext_integer_profile_owner'::pg_catalog.regrole::oid
 WHERE collection_name = 'integer_profiles';
GRANT pgcontext_integer_profile_owner TO pgcontext_integer_profile_reader;
GRANT USAGE ON SCHEMA pgcontext TO pgcontext_integer_profile_reader;
GRANT EXECUTE ON FUNCTION pgcontext.embedding_profiles()
    TO pgcontext_integer_profile_reader;
GRANT EXECUTE ON FUNCTION pgcontext.embedding_profile_explain(text, text)
    TO pgcontext_integer_profile_reader;
SET SESSION AUTHORIZATION pgcontext_integer_profile_reader;
DO $$
DECLARE
    visible_profiles bigint;
    direct_catalog_denied boolean := false;
    explained jsonb;
BEGIN
    SELECT count(*) INTO visible_profiles FROM pgcontext.embedding_profiles();
    SELECT pgcontext.embedding_profile_explain(
        'integer_profiles', 'provider_uint8_v1'
    ) INTO explained;
    BEGIN
        PERFORM count(*) FROM pgcontext._embedding_profiles;
    EXCEPTION WHEN insufficient_privilege THEN
        direct_catalog_denied := true;
    END;
    IF visible_profiles <> 3
       OR explained->>'binding_valid' <> 'true'
       OR NOT direct_catalog_denied THEN
        RAISE EXCEPTION 'least-privilege profile read failed: profiles %, explain %, direct denied %',
            visible_profiles, explained, direct_catalog_denied;
    END IF;
END
$$;
RESET SESSION AUTHORIZATION;
UPDATE pgcontext._collections
   SET owner_role = SESSION_USER::pg_catalog.regrole::oid
 WHERE collection_name = 'integer_profiles';
REVOKE EXECUTE ON FUNCTION pgcontext.embedding_profiles()
    FROM pgcontext_integer_profile_reader;
REVOKE EXECUTE ON FUNCTION pgcontext.embedding_profile_explain(text, text)
    FROM pgcontext_integer_profile_reader;
REVOKE USAGE ON SCHEMA pgcontext FROM pgcontext_integer_profile_reader;
DROP ROLE pgcontext_integer_profile_reader;
DROP ROLE pgcontext_integer_profile_owner;

UPDATE public.integer_vectors
   SET signed_embedding = pgcontext.int8vec('[-128,0,127]'),
       unsigned_embedding = pgcontext.uint8vec('[0,128,255]')
 WHERE id = 42;
DELETE FROM public.integer_vectors WHERE id = 1;
RESET enable_seqscan;
VACUUM public.integer_vectors;
REINDEX TABLE public.integer_vectors;

DO $$
DECLARE
    live_rows bigint;
BEGIN
    SELECT count(*) INTO live_rows FROM public.integer_vectors;
    IF live_rows <> 99 THEN
        RAISE EXCEPTION 'integer lifecycle expected 99 rows, saw %', live_rows;
    END IF;
END
$$;
SQL

if psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -c "COPY public.integer_vectors_wrong_typmod FROM '${binary_path}' WITH (FORMAT binary)"; then
    printf 'binary COPY unexpectedly bypassed provider-native typmods\n' >&2
    exit 1
fi

if psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -c "INSERT INTO public.integer_vectors_wrong_typmod VALUES ('[1,2,3]', '[1,2,3]', '1010101010')"; then
    printf 'text input unexpectedly bypassed provider-native typmods\n' >&2
    exit 1
fi

if psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -c "COPY public.integer_vectors_oversized_binary FROM '${oversized_binary_path}' WITH (FORMAT binary)"; then
    printf 'binary COPY unexpectedly accepted an allocation-policy-exceeding CBOR sequence\n' >&2
    exit 1
fi

pg_dump -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -Fc -f "${dump_path}"
create_database "${RESTORE_DB}"
pg_restore -h "${PGHOST}" -p "${PGPORT}" -d "${RESTORE_DB}" --no-owner "${dump_path}"

psql -h "${PGHOST}" -p "${PGPORT}" -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 <<'SQL'
DO $$
DECLARE
    live_rows bigint;
    integer_indexes bigint;
    profiles bigint;
BEGIN
    SELECT count(*) INTO live_rows FROM public.integer_vectors;
    SELECT count(*) INTO integer_indexes
      FROM pg_catalog.pg_indexes
     WHERE schemaname = 'public'
       AND tablename = 'integer_vectors'
       AND indexdef LIKE '%pgcontext_hnsw%';
    SELECT count(*) INTO profiles FROM pgcontext.embedding_profiles();
    IF live_rows <> 99 OR integer_indexes <> 9 OR profiles <> 3 THEN
        RAISE EXCEPTION 'restored integer lifecycle expected 99 rows/9 HNSW indexes/3 profiles, saw %/%/%',
            live_rows, integer_indexes, profiles;
    END IF;
END
$$;
SQL

printf 'integer_vector_lifecycle_verified\n'
