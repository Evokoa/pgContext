// Access-method SQL objects are kept together so opclass dependencies can be
// reviewed independently from the unsafe PostgreSQL callback implementation.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext.hnsw_handler(internal)
RETURNS index_am_handler
AS 'MODULE_PATHNAME', 'pgcontext_hnsw_handler'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = pgcontext.vector,
    RIGHTARG = pgcontext.vector,
    FUNCTION = pgcontext._l2_distance_fast8,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE ACCESS METHOD pgcontext_hnsw
    TYPE INDEX
    HANDLER pgcontext.hnsw_handler;

CREATE OPERATOR CLASS pgcontext.vector_hnsw_ops
    DEFAULT FOR TYPE pgcontext.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.hnsw_l2_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_ip_ops
    FOR TYPE pgcontext.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.negative_inner_product(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_cosine_ops
    FOR TYPE pgcontext.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.cosine_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_hnsw_l1_ops
    FOR TYPE pgcontext.vector USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.l1_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = pgcontext.halfvec,
    RIGHTARG = pgcontext.halfvec,
    FUNCTION = pgcontext.halfvec_l2_distance,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ip_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_negative_inner_product(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_cosine_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_cosine_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_l1_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l1_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR pgcontext.<-> (
    LEFTARG = pgcontext.sparsevec,
    RIGHTARG = pgcontext.sparsevec,
    FUNCTION = pgcontext.sparsevec_l2_distance,
    COMMUTATOR = OPERATOR(pgcontext.<->)
);

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ip_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_negative_inner_product(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_cosine_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_cosine_distance(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_l1_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l1_distance(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR pgcontext.<~> (
    LEFTARG = pgcontext.bitvec,
    RIGHTARG = pgcontext.bitvec,
    FUNCTION = pgcontext.bitvec_hamming_distance,
    COMMUTATOR = OPERATOR(pgcontext.<~>)
);

CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_jaccard_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.bitvec_jaccard_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;

"#,
    name = "create_hnsw_access_method",
    requires = [
        "pgcontext_bootstrap",
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
