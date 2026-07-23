#[pg_test]
fn cosine_hnsw_skips_zero_source_vectors() {
    Spi::run(
        "CREATE TABLE pgvector_hnsw_zero_items (
             embedding vector NOT NULL
         );
         INSERT INTO pgvector_hnsw_zero_items VALUES
             ('[0,0]'::vector),
             ('[1,0]'::vector);
         CREATE INDEX pgvector_hnsw_zero_items_idx
             ON pgvector_hnsw_zero_items
          USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("cosine HNSW should build while skipping zero vectors");

    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*)
               FROM (
                    SELECT embedding
                      FROM pgvector_hnsw_zero_items
                     ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0]'::vector
                     LIMIT 10
               ) candidates"
        )
        .expect("cosine query should visit only the indexed nonzero vector"),
        Some(1)
    );
}
