#[pg_test]
fn count_returns_active_points_for_table_backed_collection() {
    create_count_collection("m5_count_docs");
    upsert_search_points("m5_count_docs", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.delete_points('m5_count_docs', ARRAY['40'])")
        .expect("point should be deleted");

    assert_eq!(
        count_value("SELECT pgcontext.count('m5_count_docs')"),
        3
    );
}

#[pg_test]
fn count_uses_shared_filter_plan_for_ordinary_columns() {
    create_count_collection("m5_count_filter");
    upsert_search_points("m5_count_filter", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.register_filter_column('m5_count_filter', 'tenant_id', 'tenant_id')")
        .expect("tenant filter should be registered");
    Spi::run("SELECT pgcontext.register_filter_column('m5_count_filter', 'status', 'status')")
        .expect("status filter should be registered");

    assert_eq!(
        count_value(
            "SELECT pgcontext.count(
                'm5_count_filter',
                '{\"must\":[
                    {\"key\":\"tenant_id\",\"match\":\"acme\"},
                    {\"key\":\"status\",\"match\":\"open\"}
                ]}'
            )"
        ),
        2
    );
}

#[pg_test]
fn count_uses_shared_filter_plan_for_jsonb_paths() {
    create_count_collection("m5_count_jsonb");
    upsert_search_points("m5_count_jsonb", &["10", "20", "30", "50"]);
    Spi::run(
        "SELECT pgcontext.register_jsonb_path(
            'm5_count_jsonb',
            'topic',
            'metadata',
            ARRAY['topic']
        )",
    )
    .expect("JSONB path should be registered");

    assert_eq!(
        count_value(
            "SELECT pgcontext.count(
                'm5_count_jsonb',
                '{\"must\":[{\"key\":\"topic\",\"match\":\"rust\"}]}'
            )"
        ),
        2
    );
}

#[pg_test]
fn count_binds_numeric_and_boolean_filter_parameters() {
    create_count_collection("m5_count_typed");
    upsert_search_points("m5_count_typed", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.register_filter_column('m5_count_typed', 'priority', 'priority')")
        .expect("priority filter should be registered");
    Spi::run("SELECT pgcontext.register_filter_column('m5_count_typed', 'archived', 'archived')")
        .expect("archived filter should be registered");

    assert_eq!(
        count_value(
            "SELECT pgcontext.count(
                'm5_count_typed',
                '{\"must\":[
                    {\"key\":\"priority\",\"range\":{\"gte\":2,\"lt\":4}},
                    {\"key\":\"archived\",\"match\":false}
                ]}'
            )"
        ),
        2
    );
    assert_eq!(
        count_value(
            "SELECT pgcontext.count(
                'm5_count_typed',
                '{\"must\":[{\"key\":\"priority\",\"match\":{\"any\":[1,3]}}]}'
            )"
        ),
        2
    );
}

#[pg_test]
#[should_panic(expected = "ordinary column array filters must contain one scalar type")]
fn count_rejects_mixed_type_ordinary_column_arrays() {
    create_count_collection("m5_count_mixed");
    upsert_search_points("m5_count_mixed", &["10"]);
    Spi::run("SELECT pgcontext.register_filter_column('m5_count_mixed', 'priority', 'priority')")
        .expect("priority filter should be registered");

    Spi::run(
        "SELECT pgcontext.count(
            'm5_count_mixed',
            '{\"must\":[{\"key\":\"priority\",\"match\":{\"any\":[1,\"two\"]}}]}'
        )",
    )
    .expect("mixed ordinary column array should be rejected");
}

#[pg_test]
#[should_panic(expected = "unknown filter field: priority")]
fn count_rejects_unregistered_filter_fields() {
    create_count_collection("m5_count_unknown");
    upsert_search_points("m5_count_unknown", &["10"]);

    Spi::run(
        "SELECT pgcontext.count(
            'm5_count_unknown',
            '{\"must\":[{\"key\":\"priority\",\"match\":\"high\"}]}'
        )",
    )
    .expect("unregistered filter field should be rejected");
}

fn create_count_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             status text,
             priority integer NOT NULL,
             archived boolean NOT NULL,
             metadata jsonb NOT NULL
         )"
    ))
    .expect("count source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name}
             (id, embedding, tenant_id, status, priority, archived, metadata)
         VALUES (10, '[1,0]'::vector, 'acme', 'open', 1, false, '{{\"topic\":\"rust\"}}'::jsonb),
                (20, '[2,0]'::vector, 'acme', 'closed', 2, false, '{{\"topic\":\"postgres\"}}'::jsonb),
                (30, '[3,0]'::vector, 'acme', 'open', 3, false, '{{\"topic\":\"rust\"}}'::jsonb),
                (40, '[4,0]'::vector, 'acme', 'ignored', 4, true, '{{\"topic\":\"ignored\"}}'::jsonb),
                (50, '[5,0]'::vector, 'other', 'open', 5, false, '{{\"other\":\"missing\"}}'::jsonb)"
    ))
    .expect("count source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("count collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("count vector should be registered");
}

fn count_value(sql: &str) -> i64 {
    Spi::get_one::<i64>(sql)
        .expect("count query should succeed")
        .expect("count should not be null")
}
