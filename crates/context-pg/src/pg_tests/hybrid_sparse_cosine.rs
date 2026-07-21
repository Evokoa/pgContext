#[pg_test]
fn dense_sparse_query_supports_registered_sparse_cosine_metric() {
    create_dense_sparse_collection_with_metric("m14_dense_sparse_cosine", "cosine");
    upsert_hybrid_points("m14_dense_sparse_cosine", &["10", "20", "30"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm14_dense_sparse_cosine',
                '[0,0]'::vector,
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, _score)| source_key)
            .collect::<Vec<_>>(),
        vec!["10".to_owned(), "20".to_owned(), "30".to_owned()]
    );
}

#[pg_test]
fn dense_sparse_query_rejects_registered_sparse_cosine_zero_vector() {
    create_dense_sparse_collection_with_metric("m14_dense_sparse_cosine_zero", "cosine");
    upsert_hybrid_points("m14_dense_sparse_cosine_zero", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.query(
            'm14_dense_sparse_cosine_zero',
            '[0,0]'::vector,
            'lexical',
            pgcontext.sparsevec('{}/4'),
            3
        )",
        "22P02",
        "invalid vector: sparse cosine distance is undefined for zero vectors",
        "dense+sparse cosine zero query vector",
    );
}
