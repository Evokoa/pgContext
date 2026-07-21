#[pg_test]
fn query_cohort_stats_aggregate_recorded_samples() {
    create_query_stats_collection("m10_query_stats_docs");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_docs',
            'tenant:acme',
            'search_filtered',
            3,
            12,
            4.5
        )",
    )
    .expect("first query stat should be recorded");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_docs',
            'tenant:acme',
            'search_filtered',
            1,
            8,
            1.5
        )",
    )
    .expect("second query stat should be recorded");

    let rows = query_cohort_rows(
        "SELECT collection_name,
                cohort,
                query_kind,
                query_count,
                total_results,
                total_candidates,
                avg_latency_ms,
                status::text
           FROM pgcontext.query_cohort_stats()
          WHERE collection_name = 'm10_query_stats_docs'",
    );

    assert_eq!(
        rows,
        vec![(
            "m10_query_stats_docs".to_owned(),
            "tenant:acme".to_owned(),
            "search_filtered".to_owned(),
            2,
            4,
            Some(20),
            3.0,
            "Observed".to_owned(),
        )]
    );
}

#[pg_test]
fn query_cohort_stats_aggregate_detailed_counters() {
    create_query_stats_collection("m10_query_stats_detailed");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_detailed',
            'tenant:acme',
            'candidate_recheck',
            3,
            12,
            9,
            3,
            0.95,
            1.0,
            42.0,
            'Indexed'
        )",
    )
    .expect("first detailed query stat should be recorded");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_detailed',
            'tenant:acme',
            'candidate_recheck',
            2,
            8,
            5,
            3,
            0.85,
            0.75,
            75.0,
            'Indexed'
        )",
    )
    .expect("second detailed query stat should be recorded");

    let row = detailed_query_cohort_row(
        "SELECT collection_name,
                cohort,
                query_kind,
                query_count,
                total_results,
                total_candidates,
                total_rows_rechecked,
                total_rows_pruned,
                avg_recall_threshold,
                avg_recall_achieved,
                latency_bucket::text,
                lifecycle_state::text,
                avg_latency_ms,
                status::text
           FROM pgcontext.query_cohort_stats()
          WHERE collection_name = 'm10_query_stats_detailed'",
    );

    assert_eq!(row.0, "m10_query_stats_detailed");
    assert_eq!(row.1, "tenant:acme");
    assert_eq!(row.2, "candidate_recheck");
    assert_eq!(row.3, 2);
    assert_eq!(row.4, 5);
    assert_eq!(row.5, Some(20));
    assert_eq!(row.6, 14);
    assert_eq!(row.7, 6);
    assert!((row.8.expect("threshold avg") - 0.9).abs() < 0.000_000_001);
    assert!((row.9.expect("achieved avg") - 0.875).abs() < 0.000_000_001);
    assert_eq!(row.10, "Lt100Ms");
    assert_eq!(row.11, "Indexed");
    assert_eq!(row.12, 58.5);
    assert_eq!(row.13, "Observed");
}

#[pg_test]
fn telemetry_surfaces_do_not_store_vectors_payloads_filters_or_query_text() {
    create_query_stats_collection_with_payloads("m10_query_stats_privacy");

    Spi::run(
        "SELECT *
           FROM pgcontext.search(
                'm10_query_stats_privacy',
                '[0.123,0.456]'::vector,
                '{\"must\":[{\"key\":\"tenant\",\"match\":\"secret-tenant-token\"}]}',
                10
           )",
    )
    .expect("filtered search with sensitive literals should execute");
    Spi::run(
        "SELECT *
           FROM pgcontext.query(
                'm10_query_stats_privacy',
                '[0.123,0.456]'::vector,
                'secret-query-token',
                'body',
                10
           )",
    )
    .expect("hybrid query with sensitive text should execute");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_privacy',
            'tenant:bucket',
            'search_filtered',
            1,
            2,
            1.0
        )",
    )
    .expect("privacy query stat should be recorded");

    let telemetry_text = query_stats_one_text(
        "SELECT COALESCE(string_agg(row_to_json(telemetry)::text, ' '), '')
           FROM pgcontext.telemetry() AS telemetry
          WHERE collection_name = 'm10_query_stats_privacy'",
    );
    let cohort_text = query_stats_one_text(
        "SELECT COALESCE(string_agg(row_to_json(stats)::text, ' '), '')
           FROM pgcontext.query_cohort_stats() AS stats
          WHERE collection_name = 'm10_query_stats_privacy'",
    );
    let persisted_text = format!("{telemetry_text} {cohort_text}");

    for forbidden in [
        "secret-body-token",
        "secret-tenant-token",
        "secret-query-token",
        "0.123",
        "0.456",
        "{\"must\"",
    ] {
        assert!(
            !persisted_text.contains(forbidden),
            "telemetry surfaces persisted sensitive literal {forbidden}: {persisted_text}"
        );
    }
}

#[pg_test]
#[should_panic(expected = "collection does not exist: m10_query_stats_missing")]
fn record_query_stat_rejects_missing_collections() {
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_missing',
            'all',
            'search',
            1,
            NULL,
            1.0
        )",
    )
    .expect("missing collection should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported query kind: unsupported")]
fn record_query_stat_rejects_invalid_query_kind() {
    create_query_stats_collection("m10_query_stats_bad_kind");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_kind',
            'all',
            'unsupported',
            1,
            NULL,
            1.0
        )",
    )
    .expect("invalid query kind should fail");
}

#[pg_test]
#[should_panic(
    expected = "query cohort may contain only ASCII letters, digits, '_', '-', '.', ':', or '/'"
)]
fn record_query_stat_rejects_control_characters_in_cohort() {
    create_query_stats_collection("m10_query_stats_bad_cohort");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_cohort',
            E'tenant:acme\\nstatus:forged',
            'search',
            1,
            NULL,
            1.0
        )",
    )
    .expect("control-character cohort should fail");
}

#[pg_test]
#[should_panic(expected = "result_count must not be negative: -1")]
fn record_query_stat_rejects_negative_counts() {
    create_query_stats_collection("m10_query_stats_bad_count");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_count',
            'all',
            'search',
            -1,
            NULL,
            1.0
        )",
    )
    .expect("negative result count should fail");
}

#[pg_test]
#[should_panic(expected = "rows_rechecked must not be negative: -1")]
fn record_query_stat_rejects_negative_rows_rechecked() {
    create_query_stats_collection("m10_query_stats_bad_recheck");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_recheck',
            'all',
            'candidate_recheck',
            1,
            2,
            -1,
            0,
            NULL,
            NULL,
            1.0,
            'Exact'
        )",
    )
    .expect("negative rows_rechecked should fail");
}

#[pg_test]
#[should_panic(expected = "rows_pruned must not be negative: -1")]
fn record_query_stat_rejects_negative_rows_pruned() {
    create_query_stats_collection("m10_query_stats_bad_pruned");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_pruned',
            'all',
            'candidate_recheck',
            1,
            2,
            1,
            -1,
            NULL,
            NULL,
            1.0,
            'Exact'
        )",
    )
    .expect("negative rows_pruned should fail");
}

#[pg_test]
#[should_panic(expected = "recall_threshold must be finite and between 0 and 1 inclusive: 1.5")]
fn record_query_stat_rejects_invalid_recall_threshold() {
    create_query_stats_collection("m10_query_stats_bad_recall");

    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_bad_recall',
            'all',
            'candidate_recheck',
            1,
            2,
            1,
            0,
            1.5,
            1.0,
            1.0,
            'Exact'
        )",
    )
    .expect("invalid recall_threshold should fail");
}

fn create_query_stats_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("query stats source table should be created");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("query stats collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("query stats vector should be registered");
}

fn create_query_stats_collection_with_payloads(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL,
             tenant text NOT NULL
         )"
    ))
    .expect("query stats privacy source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, body, tenant)
         VALUES (1, '[0.123,0.456]'::vector, 'secret-body-token', 'secret-tenant-token')"
    ))
    .expect("query stats privacy row should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("query stats privacy collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("query stats privacy vector should be registered");
    Spi::run(&format!(
        "SELECT pgcontext.register_filter_column('{collection_name}', 'tenant', 'tenant')"
    ))
    .expect("query stats privacy filter should be registered");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY['1'])"
    ))
    .expect("query stats privacy point should be upserted");
}

fn query_stats_one_text(sql: &str) -> String {
    Spi::get_one::<String>(sql)
        .expect("privacy text query should succeed")
        .expect("privacy text should not be null")
}

type QueryCohortTestRow = (String, String, String, i64, i64, Option<i64>, f64, String);

#[allow(
    clippy::type_complexity,
    reason = "test assertions mirror the SQL result shape"
)]
type DetailedQueryCohortTestRow = (
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    i64,
    i64,
    Option<f64>,
    Option<f64>,
    String,
    String,
    f64,
    String,
);

fn detailed_query_cohort_row(sql: &str) -> DetailedQueryCohortTestRow {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?
                .expect("collection_name should not be null"),
            row.get::<String>(2)?.expect("cohort should not be null"),
            row.get::<String>(3)?
                .expect("query_kind should not be null"),
            row.get::<i64>(4)?.expect("query_count should not be null"),
            row.get::<i64>(5)?.expect("total_results should not be null"),
            row.get::<i64>(6)?,
            row.get::<i64>(7)?
                .expect("total_rows_rechecked should not be null"),
            row.get::<i64>(8)?
                .expect("total_rows_pruned should not be null"),
            row.get::<f64>(9)?,
            row.get::<f64>(10)?,
            row.get::<String>(11)?
                .expect("latency_bucket should not be null"),
            row.get::<String>(12)?
                .expect("lifecycle_state should not be null"),
            row.get::<f64>(13)?.expect("avg_latency_ms should not be null"),
            row.get::<String>(14)?.expect("status should not be null"),
        ))
    })
    .expect("detailed query cohort row should be returned")
}

fn query_cohort_rows(sql: &str) -> Vec<QueryCohortTestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?.expect("cohort should not be null"),
                row.get::<String>(3)?
                    .expect("query_kind should not be null"),
                row.get::<i64>(4)?.expect("query_count should not be null"),
                row.get::<i64>(5)?.expect("total_results should not be null"),
                row.get::<i64>(6)?,
                row.get::<f64>(7)?.expect("avg_latency_ms should not be null"),
                row.get::<String>(8)?.expect("status should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("query cohort rows should be returned")
}
