CREATE TYPE public.vector;
CREATE FUNCTION public.vector_in(cstring) RETURNS public.vector
IMMUTABLE PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'vector_in_wrapper';
CREATE FUNCTION public.vector_out(public.vector) RETURNS cstring
IMMUTABLE STRICT PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'vector_out_wrapper';
CREATE TYPE public.vector (
    INTERNALLENGTH = variable,
    INPUT = public.vector_in,
    OUTPUT = public.vector_out,
    STORAGE = extended
);

CREATE TYPE public.halfvec;
CREATE FUNCTION public.halfvec_in(cstring) RETURNS public.halfvec
IMMUTABLE PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'halfvec_in_wrapper';
CREATE FUNCTION public.halfvec_out(public.halfvec) RETURNS cstring
IMMUTABLE STRICT PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'halfvec_out_wrapper';
CREATE TYPE public.halfvec (
    INTERNALLENGTH = variable,
    INPUT = public.halfvec_in,
    OUTPUT = public.halfvec_out,
    STORAGE = extended
);

CREATE TYPE public.sparsevec;
CREATE FUNCTION public.sparsevec_in(cstring) RETURNS public.sparsevec
IMMUTABLE PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'sparsevec_in_wrapper';
CREATE FUNCTION public.sparsevec_out(public.sparsevec) RETURNS cstring
IMMUTABLE STRICT PARALLEL SAFE LANGUAGE c
AS '$libdir/pgcontext', 'sparsevec_out_wrapper';
CREATE TYPE public.sparsevec (
    INTERNALLENGTH = variable,
    INPUT = public.sparsevec_in,
    OUTPUT = public.sparsevec_out,
    STORAGE = extended
);
