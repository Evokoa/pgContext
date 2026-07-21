#[pg_test]
fn discover_returns_farthest_active_points_from_context_centroid() {
    create_discovery_collection("m13_discover_docs");
    upsert_discovery_points("m13_discover_docs", &["1", "2", "3", "4"]);

    let rows = discovery_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.discover(
                'm13_discover_docs',
                ARRAY(
                    SELECT point_id
                      FROM pgcontext._collection_points AS points
                      JOIN pgcontext._collections AS collections USING (collection_id)
                     WHERE collections.collection_name = 'm13_discover_docs'
                       AND points.source_key = '1'
                ),
                3
           )",
    );

    assert_discovery_projection(&rows, &[("3", 5.0), ("4", 4.0), ("2", 1.0)]);
}

#[pg_test]
fn explore_is_alias_for_discovery_search() {
    create_discovery_collection("m13_explore_docs");
    upsert_discovery_points("m13_explore_docs", &["1", "2", "3", "4"]);

    let rows = discovery_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.explore(
                'm13_explore_docs',
                ARRAY(
                    SELECT point_id
                      FROM pgcontext._collection_points AS points
                      JOIN pgcontext._collections AS collections USING (collection_id)
                     WHERE collections.collection_name = 'm13_explore_docs'
                       AND points.source_key IN ('1', '2')
                ),
                2
           )",
    );

    assert_discovery_projection(&rows, &[("3", 4.5), ("4", 4.031129)]);
}

#[pg_test]
#[should_panic(expected = "discovery search requires at least one context point id")]
fn discover_rejects_empty_context_points() {
    create_discovery_collection("m13_discover_empty");
    upsert_discovery_points("m13_discover_empty", &["1"]);

    Spi::run("SELECT pgcontext.discover('m13_discover_empty', ARRAY[]::bigint[], 10)")
        .expect("empty context points should be rejected");
}

#[pg_test]
#[should_panic(expected = "recommendation example point is not active or visible")]
fn discover_rejects_deleted_context_points() {
    create_discovery_collection("m13_discover_deleted");
    upsert_discovery_points("m13_discover_deleted", &["1", "2"]);
    Spi::run("SELECT pgcontext.delete_points('m13_discover_deleted', ARRAY['2'])")
        .expect("context point should be deleted");

    Spi::run(
        "SELECT pgcontext.discover(
            'm13_discover_deleted',
            ARRAY(
                SELECT point_id
                  FROM pgcontext._collection_points AS points
                  JOIN pgcontext._collections AS collections USING (collection_id)
                 WHERE collections.collection_name = 'm13_discover_deleted'
                   AND points.source_key = '2'
            ),
            10
        )",
    )
    .expect("deleted context point should be rejected");
}

fn create_discovery_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("discovery source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (1, '[0,0]'::vector),
                (2, '[1,0]'::vector),
                (3, '[5,0]'::vector),
                (4, '[0,4]'::vector)"
    ))
    .expect("discovery source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("discovery collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("discovery vector should be registered");
}

fn upsert_discovery_points(collection_name: &str, source_keys: &[&str]) {
    let keys = source_keys
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(",");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{keys}])"
    ))
    .expect("discovery points should be upserted");
}

fn discovery_rows(sql: &str) -> Vec<(i64, String, f32)> {
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
    .expect("discovery query failed")
}

fn assert_discovery_projection(actual: &[(i64, String, f32)], expected: &[(&str, f32)]) {
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
