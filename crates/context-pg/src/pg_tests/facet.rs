#[pg_test]
fn facet_counts_filtered_column_values_over_active_points() {
    create_facet_collection("m5_facet_docs");
    upsert_search_points("m5_facet_docs", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.delete_points('m5_facet_docs', ARRAY['40'])")
        .expect("point should be deleted");
    Spi::run(
        "SELECT pgcontext.register_filter_column('m5_facet_docs', 'tenant_id', 'tenant_id')",
    )
    .expect("tenant filter should be registered");
    Spi::run("SELECT pgcontext.register_filter_column('m5_facet_docs', 'status', 'status')")
        .expect("status filter should be registered");

    let rows = facet_rows(
        "SELECT value, count
           FROM pgcontext.facet(
                'm5_facet_docs',
                'status',
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                10
           )",
    );

    assert_eq!(rows, vec![("open".to_owned(), 2), ("closed".to_owned(), 1)]);
}

#[pg_test]
fn facet_counts_jsonb_path_values_and_skips_missing_values() {
    create_facet_collection("m5_facet_jsonb");
    upsert_search_points("m5_facet_jsonb", &["10", "20", "30", "50"]);
    Spi::run(
        "SELECT pgcontext.register_jsonb_path(
            'm5_facet_jsonb',
            'topic',
            'metadata',
            ARRAY['topic']
        )",
    )
    .expect("JSONB path should be registered");

    let rows = facet_rows(
        "SELECT value, count
           FROM pgcontext.facet('m5_facet_jsonb', 'topic', NULL, 10)",
    );

    assert_eq!(
        rows,
        vec![("rust".to_owned(), 2), ("postgres".to_owned(), 1)]
    );
}

#[pg_test]
fn facet_applies_limit_after_count_and_value_tie_breaks() {
    create_facet_collection("m5_facet_ties");
    upsert_search_points("m5_facet_ties", &["10", "20"]);
    Spi::run("SELECT pgcontext.register_filter_column('m5_facet_ties', 'status', 'status')")
        .expect("status filter should be registered");

    let rows = facet_rows(
        "SELECT value, count
           FROM pgcontext.facet('m5_facet_ties', 'status', NULL, 1)",
    );

    assert_eq!(rows, vec![("closed".to_owned(), 1)]);
}

#[pg_test]
#[should_panic(expected = "unknown filter field: priority")]
fn facet_rejects_unregistered_filter_fields() {
    create_facet_collection("m5_facet_unknown");
    upsert_search_points("m5_facet_unknown", &["10"]);
    Spi::run("SELECT pgcontext.register_filter_column('m5_facet_unknown', 'status', 'status')")
        .expect("status filter should be registered");

    Spi::run(
        "SELECT pgcontext.facet(
            'm5_facet_unknown',
            'status',
            '{\"must\":[{\"key\":\"priority\",\"match\":\"high\"}]}',
            10
        )",
    )
    .expect("unregistered filter field should be rejected");
}

fn create_facet_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             status text,
             metadata jsonb NOT NULL
         )"
    ))
    .expect("facet source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, tenant_id, status, metadata)
         VALUES (10, '[1,0]'::vector, 'acme', 'open', '{{\"topic\":\"rust\"}}'::jsonb),
                (20, '[2,0]'::vector, 'acme', 'closed', '{{\"topic\":\"postgres\"}}'::jsonb),
                (30, '[3,0]'::vector, 'acme', 'open', '{{\"topic\":\"rust\"}}'::jsonb),
                (40, '[4,0]'::vector, 'acme', 'ignored', '{{\"topic\":\"ignored\"}}'::jsonb),
                (50, '[5,0]'::vector, 'other', 'open', '{{\"other\":\"missing\"}}'::jsonb)"
    ))
    .expect("facet source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("facet collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("facet vector should be registered");
}

fn facet_rows(sql: &str) -> Vec<(String, i64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("facet value should not be null"),
                row.get::<i64>(2)?.expect("facet count should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("facet query failed")
}
