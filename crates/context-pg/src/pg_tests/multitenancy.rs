#[pg_test]
fn tenant_filter_recall_and_noisy_neighbor_diagnostics_use_public_sql() {
    create_multitenant_collection("m13_tenant_recall");
    Spi::run("SELECT pgcontext.register_filter_column('m13_tenant_recall', 'tenant_id', 'tenant_id')")
        .expect("tenant filter should be registered");
    upsert_multitenant_points("m13_tenant_recall", &["1", "2", "3", "4"]);

    let source_keys = text_array(
        "SELECT array_agg(source_key ORDER BY score, point_id)
           FROM pgcontext.search(
                'm13_tenant_recall',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                10
           )",
    );
    assert_eq!(source_keys, vec!["1", "2"]);

    let (recall, status) = recall_row(
        "WITH exact AS (
             SELECT array_agg(point_id ORDER BY point_id) AS point_ids
               FROM pgcontext.search(
                    'm13_tenant_recall',
                    '[0,0]'::vector,
                    '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                    10
               )
         ),
         candidates AS (
             SELECT point_ids[1:1] AS point_ids
               FROM exact
         )
         SELECT recall, status::text
           FROM pgcontext.recall_check(
                (SELECT point_ids FROM exact),
                (SELECT point_ids FROM candidates),
                0.5
           )",
    );
    assert_eq!(recall, 0.5);
    assert_eq!(status, "Passing");

    let suggested_sql = Spi::get_one::<String>(
        "SELECT suggested_sql
           FROM pgcontext.index_advisor('m13_tenant_recall')
          WHERE recommendation::text = 'CreateBtreeIndex'
          ORDER BY filter_key
          LIMIT 1",
    )
    .expect("tenant index-advisor query should succeed")
    .expect("tenant index-advisor should recommend an index");
    assert_eq!(
        suggested_sql,
        "CREATE INDEX m13_tenant_recall_tenant_id_btree_idx ON public.m13_tenant_recall USING btree (tenant_id)"
    );

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm13_tenant_recall',
            'tenant:acme',
            'search_filtered',
            2,
            4,
            12.0
         )",
    )
    .expect("tenant query stat should be recorded");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm13_tenant_recall',
            'tenant:other',
            'search_filtered',
            2,
            4,
            250.0
         )",
    )
    .expect("noisy-neighbor query stat should be recorded");

    let telemetry = tenant_latency_rows("m13_tenant_recall");
    assert_eq!(
        telemetry,
        vec![
            ("tenant:acme".to_owned(), 12.0, "Lt100Ms".to_owned()),
            ("tenant:other".to_owned(), 250.0, "Lt1S".to_owned()),
        ]
    );
}

#[pg_test]
fn tenant_partition_layout_keeps_filters_and_counts_tenant_scoped() {
    Spi::run(
        "CREATE TABLE public.m13_tenant_partitioned (
             id bigint NOT NULL,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             PRIMARY KEY (tenant_id, id)
         ) PARTITION BY LIST (tenant_id)",
    )
    .expect("tenant partitioned table should be created");
    Spi::run(
        "CREATE TABLE public.m13_tenant_partitioned_acme
            PARTITION OF public.m13_tenant_partitioned FOR VALUES IN ('acme')",
    )
    .expect("acme partition should be created");
    Spi::run(
        "CREATE TABLE public.m13_tenant_partitioned_other
            PARTITION OF public.m13_tenant_partitioned FOR VALUES IN ('other')",
    )
    .expect("other partition should be created");
    Spi::run(
        "INSERT INTO public.m13_tenant_partitioned (id, embedding, tenant_id)
         VALUES (10, '[3,0]'::vector, 'acme'),
                (20, '[1,0]'::vector, 'other'),
                (30, '[2,0]'::vector, 'acme'),
                (40, '[4,0]'::vector, 'other')",
    )
    .expect("tenant partition rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm13_tenant_partitioned',
            'public.m13_tenant_partitioned'
        )",
    )
    .expect("tenant partitioned collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm13_tenant_partitioned',
            'embedding',
            'embedding',
            2,
            'l2'
        )",
    )
    .expect("tenant partitioned vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm13_tenant_partitioned',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant partition filter should be registered");
    upsert_multitenant_points("m13_tenant_partitioned", &["10", "20", "30", "40"]);

    let filtered_count = Spi::get_one::<i64>(
        "SELECT pgcontext.count(
            'm13_tenant_partitioned',
            '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}'
        )",
    )
    .expect("tenant count should succeed")
    .expect("tenant count should not be null");
    assert_eq!(filtered_count, 2);

    let source_keys = text_array(
        "SELECT array_agg(source_key ORDER BY score, point_id)
           FROM pgcontext.search(
                'm13_tenant_partitioned',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                10
           )",
    );
    assert_eq!(source_keys, vec!["30", "10"]);
}

fn create_multitenant_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL
         )"
    ))
    .expect("multitenant source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, tenant_id)
         VALUES (1, '[1,0]'::vector, 'acme'),
                (2, '[2,0]'::vector, 'acme'),
                (3, '[1,0]'::vector, 'other'),
                (4, '[2,0]'::vector, 'other')"
    ))
    .expect("multitenant source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("multitenant collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("multitenant vector should be registered");
}

fn upsert_multitenant_points(collection_name: &str, source_keys: &[&str]) {
    let quoted_keys = source_keys
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(",");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{quoted_keys}])"
    ))
    .expect("multitenant points should be upserted");
}

fn text_array(sql: &str) -> Vec<String> {
    Spi::get_one::<Vec<String>>(sql)
        .expect("text array query should succeed")
        .unwrap_or_default()
}

fn recall_row(sql: &str) -> (f64, String) {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<f64>(1)?.expect("recall should not be null"),
            row.get::<String>(2)?.expect("status should not be null"),
        ))
    })
    .expect("recall query should succeed")
}

fn tenant_latency_rows(collection_name: &str) -> Vec<(String, f64, String)> {
    Spi::connect(|client| {
        let rows = client.select(
            &format!(
                "SELECT cohort, avg_latency_ms, latency_bucket::text
                   FROM pgcontext.query_cohort_stats()
                  WHERE collection_name = '{collection_name}'
                  ORDER BY cohort"
            ),
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("cohort should not be null"),
                row.get::<f64>(2)?
                    .expect("avg latency should not be null"),
                row.get::<String>(3)?
                    .expect("latency bucket should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("tenant telemetry query should succeed")
}
