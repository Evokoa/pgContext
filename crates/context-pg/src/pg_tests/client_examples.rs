#[pg_test]
fn named_dense_sparse_vector_example_uses_current_sql_surfaces() {
    let sql = include_str!("../../../../examples/sql/05_named_dense_sparse_vectors.sql");
    assert!(
        sql.contains("pgcontext.search_sparse("),
        "named dense/sparse example should use current sparse search SQL"
    );
    assert!(
        sql.contains("FROM pgcontext.query("),
        "named dense/sparse example should use current dense+sparse query SQL"
    );
    assert!(
        !sql.contains("\"status\":\"planned\""),
        "named dense/sparse example must not use planned sparse placeholders"
    );
    assert!(
        !sql.contains("sparse query routing remains planned"),
        "named dense/sparse example must not document stale sparse routing"
    );

    Spi::run(sql).expect("named dense/sparse vector example should run");
}
