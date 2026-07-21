#[pg_test]
fn recommend_from_positive_point_ids_excludes_examples_and_ranks_exactly() {
    create_recommend_collection("m13_recommend_points");
    upsert_recommend_points("m13_recommend_points", &["1", "2", "3", "4"]);

    let rows = recommend_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.recommend(
                'm13_recommend_points',
                ARRAY(
                    SELECT point_id
                      FROM pgcontext._collection_points AS points
                      JOIN pgcontext._collections AS collections USING (collection_id)
                     WHERE collections.collection_name = 'm13_recommend_points'
                       AND points.source_key = '1'
                ),
                ARRAY[]::bigint[],
                3
           )",
    );

    assert_source_projection(&rows, &[("2", 1.0), ("4", 4.1231055), ("3", 7.0)]);
}

#[pg_test]
fn recommend_from_positive_and_negative_points_uses_directional_query() {
    create_recommend_collection("m13_recommend_negative");
    upsert_recommend_points("m13_recommend_negative", &["1", "2", "3", "4"]);

    let rows = recommend_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.recommend(
                'm13_recommend_negative',
                ARRAY(
                    SELECT point_id
                      FROM pgcontext._collection_points AS points
                      JOIN pgcontext._collections AS collections USING (collection_id)
                     WHERE collections.collection_name = 'm13_recommend_negative'
                       AND points.source_key = '2'
                ),
                ARRAY(
                    SELECT point_id
                      FROM pgcontext._collection_points AS points
                      JOIN pgcontext._collections AS collections USING (collection_id)
                     WHERE collections.collection_name = 'm13_recommend_negative'
                       AND points.source_key = '4'
                ),
                2
           )",
    );

    assert_source_projection(&rows, &[("1", 4.1231055), ("3", 7.2111025)]);
}

#[pg_test]
fn recommend_from_raw_vectors_does_not_exclude_collection_points() {
    create_recommend_collection("m13_recommend_raw");
    upsert_recommend_points("m13_recommend_raw", &["1", "2", "3", "4"]);

    let rows = recommend_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.recommend(
                'm13_recommend_raw',
                ARRAY['[0,4]'::vector],
                ARRAY[]::vector[],
                2
           )",
    );

    assert_source_projection(&rows, &[("4", 0.0), ("1", 4.1231055)]);
}

#[pg_test]
#[should_panic(expected = "recommendation requires at least one positive example")]
fn recommend_rejects_empty_positive_point_ids() {
    create_recommend_collection("m13_recommend_empty");
    upsert_recommend_points("m13_recommend_empty", &["1"]);

    Spi::run(
        "SELECT pgcontext.recommend(
            'm13_recommend_empty',
            ARRAY[]::bigint[],
            ARRAY[]::bigint[],
            10
        )",
    )
    .expect("empty positive examples should be rejected");
}

#[pg_test]
#[should_panic(expected = "recommendation example point is not active or visible")]
fn recommend_rejects_deleted_point_examples() {
    create_recommend_collection("m13_recommend_deleted");
    upsert_recommend_points("m13_recommend_deleted", &["1", "2"]);
    Spi::run("SELECT pgcontext.delete_points('m13_recommend_deleted', ARRAY['2'])")
        .expect("recommendation example should be deleted");

    Spi::run(
        "SELECT pgcontext.recommend(
            'm13_recommend_deleted',
            ARRAY(
                SELECT point_id
                  FROM pgcontext._collection_points AS points
                  JOIN pgcontext._collections AS collections USING (collection_id)
                 WHERE collections.collection_name = 'm13_recommend_deleted'
                   AND points.source_key = '2'
            ),
            ARRAY[]::bigint[],
            10
        )",
    )
    .expect("deleted positive example should be rejected");
}

#[pg_test]
#[should_panic(expected = "recommendation vector dimensions do not match")]
fn recommend_rejects_mismatched_raw_vector_dimensions() {
    create_recommend_collection("m13_recommend_dims");
    upsert_recommend_points("m13_recommend_dims", &["1"]);

    Spi::run(
        "SELECT pgcontext.recommend(
            'm13_recommend_dims',
            ARRAY['[1,0]'::vector],
            ARRAY['[1,0,0]'::vector],
            10
        )",
    )
    .expect("mismatched raw vector dimensions should be rejected");
}

fn create_recommend_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("recommendation source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (1, '[1,0]'::vector),
                (2, '[2,0]'::vector),
                (3, '[8,0]'::vector),
                (4, '[0,4]'::vector)"
    ))
    .expect("recommendation source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("recommendation collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("recommendation vector should be registered");
}

fn upsert_recommend_points(collection_name: &str, source_keys: &[&str]) {
    let keys = source_keys
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(",");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{keys}])"
    ))
    .expect("recommendation points should be upserted");
}

fn recommend_rows(sql: &str) -> Vec<(i64, String, f32)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point id should not be null"),
                row.get::<String>(2)?
                    .expect("source key should not be null"),
                row.get::<f32>(3)?.expect("score should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("recommendation query failed")
}

fn assert_source_projection(actual: &[(i64, String, f32)], expected: &[(&str, f32)]) {
    assert_eq!(actual.len(), expected.len());
    for ((_, source_key, score), (expected_source_key, expected_score)) in
        actual.iter().zip(expected)
    {
        assert_eq!(source_key, expected_source_key);
        assert!(
            (*score - *expected_score).abs() < 0.000_01,
            "score mismatch for {source_key}: got {score}, expected {expected_score}"
        );
    }
}
