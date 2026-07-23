-- Certified pgvector compatibility bridge.
--
-- This artifact deliberately lives outside the main pgcontext extension. Its
-- extension dependencies keep pgvector-owned objects out of pgcontext's
-- dependency graph, while its extension membership makes every cast, wrapper,
-- and opclass removable as one audited unit.

DO $pgcontext_pgvector_preflight$
DECLARE
    pgcontext_version text;
    pgvector_version text;
    pgvector_schema text;
    compatible_type_count integer;
BEGIN
    IF pg_catalog.current_setting('server_version_num')::integer < 170000
       OR pg_catalog.current_setting('server_version_num')::integer >= 180000 THEN
        RAISE EXCEPTION 'pgcontext_pgvector 0.2.0 supports PostgreSQL 17 only'
            USING ERRCODE = '0A000';
    END IF;

    SELECT extension.extversion
      INTO pgcontext_version
      FROM pg_catalog.pg_extension AS extension
     WHERE extension.extname = 'pgcontext';
    IF pgcontext_version IS DISTINCT FROM '0.2.0' THEN
        RAISE EXCEPTION 'pgcontext_pgvector 0.2.0 requires pgcontext 0.2.0 (found %)',
                        pgcontext_version
            USING ERRCODE = '0A000';
    END IF;

    SELECT extension.extversion, namespace.nspname
      INTO pgvector_version, pgvector_schema
      FROM pg_catalog.pg_extension AS extension
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = extension.extnamespace
     WHERE extension.extname = 'vector';
    IF pgvector_schema IS DISTINCT FROM 'public' THEN
        RAISE EXCEPTION 'pgcontext_pgvector requires pgvector in public (found %)',
                        pgvector_schema
            USING ERRCODE = '0A000';
    END IF;
    IF pgvector_version IS NULL OR pgvector_version !~ '^0[.]8[.][0-9]+$' THEN
        RAISE EXCEPTION 'pgcontext_pgvector 0.2.0 certifies pgvector 0.8.x only (found %)',
                        pgvector_version
            USING ERRCODE = '0A000';
    END IF;

    SELECT pg_catalog.count(*)::integer
      INTO compatible_type_count
      FROM pg_catalog.pg_type AS type
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = type.typnamespace
     WHERE namespace.nspname = 'public'
       AND type.typname IN ('vector', 'halfvec', 'sparsevec')
       AND type.typlen = -1
       AND NOT type.typbyval
       AND type.typalign = 'i'
       AND type.typstorage = 'e'
       AND type.typtype = 'b'
       AND EXISTS (
             SELECT 1
               FROM pg_catalog.pg_depend AS dependency
               JOIN pg_catalog.pg_extension AS extension
                 ON extension.oid = dependency.refobjid
              WHERE dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                AND dependency.objid = type.oid
                AND dependency.deptype = 'e'
                AND extension.extname = 'vector'
           );
    IF compatible_type_count <> 3 THEN
        RAISE EXCEPTION 'pgvector vector/halfvec/sparsevec catalog contract is not certified'
            USING ERRCODE = '0A000';
    END IF;
END
$pgcontext_pgvector_preflight$ LANGUAGE plpgsql;

CREATE CAST (public.vector AS pgcontext.vector)
    WITHOUT FUNCTION AS ASSIGNMENT;

CREATE CAST (public.halfvec AS pgcontext.halfvec)
    WITHOUT FUNCTION AS ASSIGNMENT;

CREATE FUNCTION pgcontext._pgvector_sparsevec_to_pgcontext(public.sparsevec)
RETURNS pgcontext.sparsevec
AS '$libdir/pgcontext', 'pgcontext_pgvector_sparsevec_to_pgcontext'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE FUNCTION pgcontext._pgcontext_sparsevec_to_pgvector(pgcontext.sparsevec)
RETURNS public.sparsevec
AS '$libdir/pgcontext', 'pgcontext_pgcontext_sparsevec_to_pgvector'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE CAST (public.sparsevec AS pgcontext.sparsevec)
    WITH FUNCTION pgcontext._pgvector_sparsevec_to_pgcontext(public.sparsevec)
    AS ASSIGNMENT;

CREATE CAST (pgcontext.sparsevec AS public.sparsevec)
    WITH FUNCTION pgcontext._pgcontext_sparsevec_to_pgvector(pgcontext.sparsevec)
    AS ASSIGNMENT;

CREATE FUNCTION pgcontext._pgvector_vector_l2_support(public.vector, public.vector)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.hnsw_l2_distance($1::pgcontext.vector, $2::pgcontext.vector);

CREATE FUNCTION pgcontext._pgvector_vector_ip_support(public.vector, public.vector)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.negative_inner_product($1::pgcontext.vector, $2::pgcontext.vector)::double precision;

CREATE FUNCTION pgcontext._pgvector_vector_cosine_support(public.vector, public.vector)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.cosine_distance($1::pgcontext.vector, $2::pgcontext.vector)::double precision;

CREATE FUNCTION pgcontext._pgvector_vector_l1_support(public.vector, public.vector)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.l1_distance($1::pgcontext.vector, $2::pgcontext.vector)::double precision;

CREATE FUNCTION pgcontext._pgvector_halfvec_l2_support(public.halfvec, public.halfvec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.halfvec_l2_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision;

CREATE FUNCTION pgcontext._pgvector_halfvec_ip_support(public.halfvec, public.halfvec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.halfvec_negative_inner_product($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision;

CREATE FUNCTION pgcontext._pgvector_halfvec_cosine_support(public.halfvec, public.halfvec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.halfvec_cosine_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision;

CREATE FUNCTION pgcontext._pgvector_halfvec_l1_support(public.halfvec, public.halfvec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN pgcontext.halfvec_l1_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision;

CREATE FUNCTION pgcontext._pgvector_sparsevec_l2_support(public.sparsevec, public.sparsevec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN public.l2_distance($1, $2);

CREATE FUNCTION pgcontext._pgvector_sparsevec_ip_support(public.sparsevec, public.sparsevec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN -public.inner_product($1, $2);

CREATE FUNCTION pgcontext._pgvector_sparsevec_cosine_support(public.sparsevec, public.sparsevec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN public.cosine_distance($1, $2);

CREATE FUNCTION pgcontext._pgvector_sparsevec_l1_support(public.sparsevec, public.sparsevec)
RETURNS double precision
LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
RETURN public.l1_distance($1, $2);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_pgvector_l2_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 public.<-> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_vector_l2_support(public.vector, public.vector),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.vector_hnsw_pgvector_ip_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 public.<#> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_vector_ip_support(public.vector, public.vector),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.vector_hnsw_pgvector_cosine_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 public.<=> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_vector_cosine_support(public.vector, public.vector),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.vector_hnsw_pgvector_l1_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 public.<+> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_vector_l1_support(public.vector, public.vector),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_pgvector_l2_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 public.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_halfvec_l2_support(public.halfvec, public.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_pgvector_ip_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 public.<#> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_halfvec_ip_support(public.halfvec, public.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_pgvector_cosine_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 public.<=> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_halfvec_cosine_support(public.halfvec, public.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_pgvector_l1_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 public.<+> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_halfvec_l1_support(public.halfvec, public.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_pgvector_l2_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 public.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_sparsevec_l2_support(public.sparsevec, public.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_pgvector_ip_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 public.<#> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_sparsevec_ip_support(public.sparsevec, public.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_pgvector_cosine_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 public.<=> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_sparsevec_cosine_support(public.sparsevec, public.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_pgvector_l1_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 public.<+> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext._pgvector_sparsevec_l1_support(public.sparsevec, public.sparsevec),
    STORAGE pgcontext.vector;
