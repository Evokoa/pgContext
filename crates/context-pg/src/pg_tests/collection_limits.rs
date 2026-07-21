#[pg_test]
fn collection_limits_reports_default_non_strict_policy() {
    create_limit_collection("m12_limits_default", "embedding vector");

    let rows = collection_limit_rows(
        "SELECT strict_mode,
                max_dimensions,
                max_vectors,
                max_points,
                max_filter_nodes,
                max_search_limit,
                max_candidate_budget,
                query_timeout_ms,
                max_index_memory_bytes
           FROM pgcontext.collection_limits('m12_limits_default')",
    );

    assert_eq!(
        rows,
        vec![(
            false, None, None, None, None, None, None, None, None,
        )]
    );
}

#[pg_test]
fn configure_collection_limits_round_trips_strict_policy() {
    create_limit_collection("m12_limits_configured", "embedding vector");

    let rows = collection_limit_rows(
        "SELECT strict_mode,
                max_dimensions,
                max_vectors,
                max_points,
                max_filter_nodes,
                max_search_limit,
                max_candidate_budget,
                query_timeout_ms,
                max_index_memory_bytes
           FROM pgcontext.configure_collection_limits(
                'm12_limits_configured',
                true,
                3,
                1,
                2,
                8,
                5,
                7,
                250,
                4096
           )",
    );

    assert_eq!(
        rows,
        vec![(
            true,
            Some(3),
            Some(1),
            Some(2),
            Some(8),
            Some(5),
            Some(7),
            Some(250),
            Some(4096),
        )]
    );
}

#[pg_test]
#[should_panic(expected = "collection m12_limits_dimensions max_dimensions 2 exceeded: 3")]
fn register_vector_rejects_dimensions_above_strict_collection_limit() {
    create_limit_collection("m12_limits_dimensions", "embedding vector");
    configure_limit_collection("m12_limits_dimensions", "true, 2, 1, NULL, NULL, NULL, NULL, NULL, NULL");

    Spi::run(
        "SELECT pgcontext.register_vector(
            'm12_limits_dimensions',
            'embedding',
            'embedding',
            3,
            'l2'
        )",
    )
    .expect("oversized vector registration should fail");
}

#[pg_test]
#[should_panic(expected = "collection m12_limits_vectors max_vectors 1 exceeded: 2")]
fn register_vector_rejects_vector_count_above_strict_collection_limit() {
    create_limit_collection("m12_limits_vectors", "embedding vector, title_embedding vector");
    configure_limit_collection("m12_limits_vectors", "true, 3, 1, NULL, NULL, NULL, NULL, NULL, NULL");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm12_limits_vectors',
            'embedding',
            'embedding',
            3,
            'l2'
        )",
    )
    .expect("first vector registration should succeed");

    Spi::run(
        "SELECT pgcontext.register_vector(
            'm12_limits_vectors',
            'title_embedding',
            'title_embedding',
            3,
            'l2'
        )",
    )
    .expect("second vector registration should fail");
}

#[pg_test]
fn upsert_points_rejects_point_count_above_strict_collection_limit_without_partial_insert() {
    create_limit_collection("m12_limits_points", "embedding vector");
    configure_limit_collection("m12_limits_points", "true, NULL, NULL, 2, NULL, NULL, NULL, NULL, NULL");

    let result = std::panic::catch_unwind(|| {
        Spi::run("SELECT pgcontext.upsert_points('m12_limits_points', ARRAY['a', 'b', 'c'])")
            .expect("over-budget point upsert should fail");
    });
    assert!(result.is_err(), "over-budget point upsert should panic");

    let count = Spi::get_one::<i64>(
        "SELECT count(*) FROM pgcontext._collection_points points
          JOIN pgcontext._collections collections USING (collection_id)
         WHERE collections.collection_name = 'm12_limits_points'",
    )
    .expect("point count query should succeed")
    .expect("point count should not be null");
    assert_eq!(count, 0);
}

#[pg_test]
#[should_panic(expected = "invalid collection m12_limits_invalid max_search_limit: 0")]
fn configure_collection_limits_rejects_invalid_limit_values() {
    create_limit_collection("m12_limits_invalid", "embedding vector");

    Spi::run(
        "SELECT pgcontext.configure_collection_limits(
            'm12_limits_invalid',
            true,
            NULL,
            NULL,
            NULL,
            NULL,
            0,
            NULL,
            NULL,
            NULL
        )",
    )
    .expect("invalid limit value should fail");
}

#[pg_test]
#[should_panic(expected = "collection m12_limits_search max_search_limit 1 exceeded: 2")]
fn search_rejects_limit_above_strict_collection_limit() {
    create_limit_collection("m12_limits_search", "embedding vector");
    Spi::run(
        "INSERT INTO public.m12_limits_search (embedding)
         VALUES ('[1,2,3]'::vector)",
    )
    .expect("search fixture row should be inserted");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm12_limits_search',
            'embedding',
            'embedding',
            3,
            'l2'
        )",
    )
    .expect("search vector should register");
    Spi::run("SELECT pgcontext.upsert_points('m12_limits_search', ARRAY['1'])")
        .expect("search point should register");
    configure_limit_collection("m12_limits_search", "true, NULL, NULL, NULL, NULL, 1, NULL, NULL, NULL");

    Spi::run("SELECT pgcontext.search('m12_limits_search', '[1,2,3]'::vector, 2)")
        .expect("over-limit search should fail");
}

#[pg_test]
#[should_panic(expected = "collection m12_limits_candidates max_candidate_budget 1 exceeded: 2")]
fn candidate_recheck_rejects_candidate_batch_above_strict_collection_limit() {
    create_limit_collection("m12_limits_candidates", "embedding vector");
    Spi::run(
        "INSERT INTO public.m12_limits_candidates (embedding)
         VALUES ('[1,2,3]'::vector), ('[4,5,6]'::vector)",
    )
    .expect("candidate fixture rows should be inserted");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm12_limits_candidates',
            'embedding',
            'embedding',
            3,
            'l2'
        )",
    )
    .expect("candidate vector should register");
    Spi::run("SELECT pgcontext.upsert_points('m12_limits_candidates', ARRAY['1', '2'])")
        .expect("candidate points should register");
    configure_limit_collection(
        "m12_limits_candidates",
        "true, NULL, NULL, NULL, NULL, NULL, 1, NULL, NULL",
    );

    Spi::run(
        "SELECT pgcontext.search(
            'm12_limits_candidates',
            '[1,2,3]'::vector,
            ARRAY[1, 2]::bigint[],
            1
        )",
    )
    .expect("over-budget candidate recheck should fail");
}

fn create_limit_collection(collection_name: &str, vector_columns_sql: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             {vector_columns_sql}
         )"
    ))
    .expect("limit fixture source table should be created");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("limit fixture collection should be created");
}

fn configure_limit_collection(collection_name: &str, args: &str) {
    Spi::run(&format!(
        "SELECT pgcontext.configure_collection_limits('{collection_name}', {args})"
    ))
    .expect("collection limits should configure");
}

type CollectionLimitRow = (
    bool,
    Option<i32>,
    Option<i32>,
    Option<i64>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i64>,
);

fn collection_limit_rows(sql: &str) -> Vec<CollectionLimitRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<bool>(1)?.expect("strict_mode should not be null"),
                row.get::<i32>(2)?,
                row.get::<i32>(3)?,
                row.get::<i64>(4)?,
                row.get::<i32>(5)?,
                row.get::<i32>(6)?,
                row.get::<i32>(7)?,
                row.get::<i32>(8)?,
                row.get::<i64>(9)?,
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("collection limits query failed")
}
