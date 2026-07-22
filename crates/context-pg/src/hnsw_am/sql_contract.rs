// Access-method SQL objects are kept together so opclass dependencies can be
// reviewed independently from the unsafe PostgreSQL callback implementation.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext.hnsw_handler(internal)
RETURNS index_am_handler
AS 'MODULE_PATHNAME', 'pgcontext_hnsw_handler'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = public.vector,
    RIGHTARG = public.vector,
    FUNCTION = pgcontext._l2_distance_fast8,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE ACCESS METHOD pgcontext_hnsw
    TYPE INDEX
    HANDLER pgcontext.hnsw_handler;

CREATE OPERATOR CLASS pgcontext.vector_hnsw_ops
    DEFAULT FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.hnsw_l2_distance(public.vector, public.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_ip_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.negative_inner_product(public.vector, public.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_cosine_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.cosine_distance(public.vector, public.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_l1_ops
    FOR TYPE public.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.l1_distance(public.vector, public.vector);

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = public.halfvec,
    RIGHTARG = public.halfvec,
    FUNCTION = pgcontext.halfvec_l2_distance,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ip_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_negative_inner_product(public.halfvec, public.halfvec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_cosine_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_cosine_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_l1_ops
    FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l1_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = public.sparsevec,
    RIGHTARG = public.sparsevec,
    FUNCTION = pgcontext.sparsevec_l2_distance,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ip_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_negative_inner_product(public.sparsevec, public.sparsevec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_cosine_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_cosine_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_l1_ops
    FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l1_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;

CREATE OPERATOR pgcontext.<~> (
    LEFTARG = public.bitvec,
    RIGHTARG = public.bitvec,
    FUNCTION = pgcontext.bitvec_hamming_distance,
    COMMUTATOR = OPERATOR(pgcontext.<~>)
);

CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;

CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_jaccard_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<%> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.bitvec_jaccard_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;

"#,
    name = "create_hnsw_access_method",
    requires = [
        pgcontext,
        Vector,
        HalfVec,
        SparseVec,
        BitVec,
        "create_vector_distance_operators",
        "create_vector_variant_distance_operators",
        "create_vector_fast_distance_functions",
        hnsw_l2_distance,
        negative_inner_product,
        cosine_distance,
        l1_distance,
        halfvec_l2_distance,
        halfvec_negative_inner_product,
        halfvec_cosine_distance,
        halfvec_l1_distance,
        sparsevec_l2_distance,
        sparsevec_negative_inner_product,
        sparsevec_cosine_distance,
        sparsevec_l1_distance,
        bitvec_hamming_distance,
        bitvec_jaccard_distance
    ]
);
