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
fn automatic_query_stats_capture_strategy_work_and_source_updates() {
    create_query_stats_collection("stage_i_automatic_stats");
    Spi::run(
        "INSERT INTO public.stage_i_automatic_stats VALUES
             (1, '[1,0]'::vector),
             (2, '[0,1]'::vector),
             (3, '[0.8,0.2]'::vector);
         SELECT pgcontext.backfill_points('stage_i_automatic_stats', 100);
         CREATE INDEX stage_i_automatic_stats_hnsw
             ON public.stage_i_automatic_stats
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_i_automatic_stats', 'embedding',
             'public.stage_i_automatic_stats_hnsw'
         );",
    )
    .expect("automatic query stats fixture should be created");

    let first = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_i_automatic_stats',
               pgcontext.query_nearest('[1,0]'::vector, 2)
           )",
    );
    assert_eq!(first.len(), 2);

    Spi::run("UPDATE public.stage_i_automatic_stats SET embedding = '[0,1]' WHERE id = 3")
        .expect("source update should commit before the second observed query");
    let second = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_i_automatic_stats',
               pgcontext.query_nearest('[0,1]'::vector, 2)
           )",
    );
    assert_eq!(second.len(), 2);

    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_i_automatic_stats'",
    )
    .expect("automatic collection lookup should succeed")
    .expect("automatic collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.collection_id == collection_id));
    assert!(events.iter().all(|event| event.query_kind == "search"));
    assert!(events.iter().all(|event| event.strategy == "dense_hnsw"));
    assert!(events.iter().all(|event| event.visits >= event.candidates));
    assert!(events.iter().all(|event| event.candidates >= event.rechecks));
    assert!(events.iter().all(|event| event.stages >= 2));
    assert!(events.iter().all(|event| event.expansions >= 1));
    assert!(events.iter().all(|event| event.completion == "complete"));
    assert!(events.iter().all(|event| event.lifecycle == "Indexed"));
    assert!(events.iter().all(|event| event.latency_micros > 0));
    assert!(events.iter().all(|event| event.result_count == 2));
}

#[pg_test]
fn automatic_query_stats_capture_named_sparse_exact_and_ann_searches() {
    Spi::run(
        "CREATE TABLE public.stage_i_sparse_stats (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.stage_i_sparse_stats VALUES
             (1, '{1:1}/4'::sparsevec),
             (2, '{1:2}/4'::sparsevec),
             (3, '{1:3}/4'::sparsevec);
         SELECT pgcontext.create_collection(
             'stage_i_sparse_stats', 'public.stage_i_sparse_stats'
         );
         SELECT pgcontext.register_sparse_vector(
             'stage_i_sparse_stats', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.backfill_points('stage_i_sparse_stats', 100);",
    )
    .expect("sparse automatic query stats fixture should be created");

    let exact = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
               'stage_i_sparse_stats', 'lexical', '{1:2}/4'::sparsevec, 2
           )",
    );
    assert_eq!(exact.len(), 2);

    Spi::run(
        "CREATE INDEX stage_i_sparse_stats_hnsw
             ON public.stage_i_sparse_stats USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'stage_i_sparse_stats', 'lexical',
             'public.stage_i_sparse_stats_hnsw'
         );",
    )
    .expect("sparse automatic query stats HNSW fixture should be attached");
    let ann = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
               'stage_i_sparse_stats', 'lexical', '{1:2}/4'::sparsevec, 2
           )",
    );
    assert_eq!(ann, exact);

    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_i_sparse_stats'",
    )
    .expect("sparse automatic collection lookup should succeed")
    .expect("sparse automatic collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].strategy, "named_sparse_exact");
    assert_eq!(events[0].lifecycle, "Exact");
    assert_eq!(events[1].strategy, "named_sparse_hnsw");
    assert_eq!(events[1].lifecycle, "Indexed");
    assert!(events.iter().all(|event| event.query_kind == "search"));
    assert!(events.iter().all(|event| event.completion == "complete"));
    assert!(events.iter().all(|event| event.result_count == 2));
    assert!(events.iter().all(|event| event.visits >= event.candidates));
    assert!(events.iter().all(|event| event.candidates >= event.rechecks));
}

#[pg_test]
fn automatic_observability_classifies_terminal_outcomes() {
    use context_query::{Completion, ExecutionState, ReadinessReason};

    assert_eq!(
        crate::query_stats::completion_label(Completion::Cancelled),
        "cancelled"
    );

    let rebuild = ExecutionState::RebuildRequired {
        reason: ReadinessReason::ConfigurationChanged,
    };
    assert_eq!(
        crate::query_stats::lifecycle_state_label(&rebuild, "dense_hnsw", false),
        "IndexNotReady"
    );

    let not_ready = ExecutionState::NotReady {
        reason: ReadinessReason::GenerationMissing,
    };
    assert_eq!(
        crate::query_stats::lifecycle_state_label(&not_ready, "quantized_mmap_hnsw", false),
        "ArtifactMissing"
    );

    let corrupt = ExecutionState::NotReady {
        reason: ReadinessReason::ValidationFailed,
    };
    assert_eq!(
        crate::query_stats::lifecycle_state_label(&corrupt, "quantized_mmap_hnsw", false),
        "IndexCorrupt"
    );

}

#[pg_test]
fn automatic_observability_dsm_first_attach_failure_is_fail_open() {
    assert!(crate::query_stats_async::test_failed_first_attach_recovers());
}

#[pg_test]
fn automatic_observability_reuses_database_slot_across_extension_generations() {
    assert!(crate::query_stats_async::test_database_slot_generations_reuse_one_slot());
}

#[pg_test]
fn automatic_observability_producer_lock_contention_is_fail_open() {
    create_query_stats_collection("stage_i_lock_contention");
    let started = std::time::Instant::now();
    let result_count = crate::query_stats_async::test_with_producer_lock_contention(|| {
        Spi::get_one::<i64>(
            "SELECT count(*)
               FROM pgcontext.execute_query(
                    'stage_i_lock_contention',
                    pgcontext.query_lookup(ARRAY[1]::bigint[])
               )",
        )
        .expect("retrieval should remain usable under telemetry lock contention")
        .expect("count should not be null")
    });
    assert_eq!(result_count, 0);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[pg_test]
fn automatic_observability_rejects_reused_worker_pid_identity() {
    assert!(crate::query_stats_async::test_pid_reuse_is_rejected());
}

#[pg_test]
fn automatic_observability_captures_executor_error_and_missing_artifact() {
    create_query_stats_collection("stage_i_terminal_events");
    Spi::run(
        "INSERT INTO public.stage_i_terminal_events VALUES
             (1, '[1,0]'::vector),
             (2, '[0,1]'::vector);
         SELECT pgcontext.backfill_points('stage_i_terminal_events', 10);
         CREATE INDEX stage_i_terminal_events_hnsw
             ON public.stage_i_terminal_events
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_i_terminal_events', 'embedding',
             'public.stage_i_terminal_events_hnsw'
         );",
    )
    .expect("terminal event fixture should be created");
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_i_terminal_events'",
    )
    .expect("terminal event collection lookup should succeed")
    .expect("terminal event collection should exist");

    let typed_error = std::panic::catch_unwind(|| {
        Spi::run(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_terminal_events',
                 pgcontext.query_formula(
                     pgcontext.query_nearest('[1,0]'::vector, 1),
                     'system($score)'
                 )
             )",
        )
        .expect("invalid executable formula should fail");
    });
    assert!(typed_error.is_err());
    let events = crate::query_stats_async::test_events(collection_id);
    let event = events.last().expect("typed executor error should be captured");
    assert_eq!(event.completion, "error");
    assert_eq!(event.strategy, "executor_error");
    assert_eq!(event.visits, 0);
    assert_eq!(event.candidates, 0);
    assert_eq!(event.stages, 0);

    Spi::run(
        "SELECT pgcontext.configure_vector(
             'stage_i_terminal_events', 'embedding', '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":8}'::jsonb, 'ready'
         );",
    )
    .expect("missing-artifact vector policy should be configured");
    let missing = std::panic::catch_unwind(|| {
        Spi::run(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_terminal_events',
                 pgcontext.query_nearest('[1,0]'::vector, 1)
             )",
        )
        .expect("missing quantized artifact should fail");
    });
    assert!(missing.is_err());
    let events = crate::query_stats_async::test_events(collection_id);
    let event = events.last().expect("missing artifact event should be captured");
    assert_eq!(event.completion, "error");
    assert_eq!(event.lifecycle, "ArtifactMissing");
}

#[pg_test]
fn automatic_observations_are_owned_by_the_invocation_that_started_them() {
    create_query_stats_collection("stage_i_nested_observation");
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_i_nested_observation'",
    )
    .expect("nested observation collection lookup should succeed")
    .expect("nested observation collection should exist");

    let outer = crate::query_stats_async::begin(collection_id, "hybrid", false)
        .expect("test telemetry should be enabled");
    let missing = std::panic::catch_unwind(|| {
        Spi::run(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_missing_nested_collection',
                 pgcontext.query_lookup(ARRAY[1]::bigint[])
             )",
        )
        .expect("missing nested collection should fail before observation begin");
    });
    assert!(missing.is_err());
    crate::query_stats_async::finish(outer, complete_test_summary("exact_lookup"));

    let outer = crate::query_stats_async::begin(collection_id, "hybrid", false)
        .expect("test telemetry should remain enabled");
    let typed = std::panic::catch_unwind(|| {
        Spi::run(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_nested_observation',
                 pgcontext.query_formula(
                     pgcontext.query_lookup(ARRAY[1]::bigint[]),
                     'system($score)'
                 )
             )",
        )
        .expect("nested typed executor error should fail after its observation finishes");
    });
    assert!(typed.is_err());
    crate::query_stats_async::finish(outer, complete_test_summary("exact_lookup"));

    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].completion, "complete");
    assert_eq!(events[1].completion, "error");
    assert_eq!(events[2].completion, "complete");
}

#[pg_test]
fn automatic_observability_reports_actual_named_source_visits() {
    Spi::run(
        "CREATE TABLE public.stage_i_named_visits (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             body text NOT NULL
         );
         INSERT INTO public.stage_i_named_visits VALUES
             (1, '[1,0]', 'postgres telemetry'),
             (2, '[0.8,0.2]', 'postgres telemetry'),
             (3, '[0,1]', 'postgres telemetry'),
             (4, '[-1,0]', 'postgres telemetry');
         SELECT pgcontext.create_collection(
             'stage_i_named_visits', 'public.stage_i_named_visits'
         );
         SELECT pgcontext.register_vector(
             'stage_i_named_visits', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('stage_i_named_visits', 100);",
    )
    .expect("named source telemetry fixture should be created");
    let (collection_id, first_point_id) = Spi::connect(|client| {
        let row = client.select(
            "SELECT acl.collection_id, points.point_id
               FROM pgcontext._collection_acl AS acl
               JOIN pgcontext._visible_collection_points AS points
                 ON points.collection_id = acl.collection_id
              WHERE acl.collection_name = 'stage_i_named_visits'
                AND points.source_key = '1'",
            None,
            &[],
        )?.first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.expect("collection id should exist"),
            row.get::<i64>(2)?.expect("point id should exist"),
        ))
    })
    .expect("named source ids should resolve");

    assert_eq!(
        table_search_rows(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_named_visits',
                 pgcontext.query_full_text('postgres telemetry', 'body', 1)
             )"
        )
        .len(),
        1
    );
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.last().map(|event| event.strategy.as_str()), Some("postgres_full_text"));
    assert_eq!(events.last().map(|event| event.visits), Some(4));

    assert_eq!(
        table_search_rows(&format!(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_named_visits',
                 pgcontext.query_recommend(ARRAY[{first_point_id}]::bigint[], ARRAY[]::bigint[], 1)
             )"
        ))
        .len(),
        1
    );
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.last().map(|event| event.strategy.as_str()), Some("exact_recommend"));
    assert_eq!(events.last().map(|event| event.visits), Some(3));

    assert_eq!(
        table_search_rows(&format!(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_named_visits',
                 pgcontext.query_discover(ARRAY[{first_point_id}]::bigint[], 1)
             )"
        ))
        .len(),
        1
    );
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.last().map(|event| event.strategy.as_str()), Some("exact_discover"));
    assert_eq!(events.last().map(|event| event.visits), Some(3));

    let point_ids = Spi::get_one::<Vec<i64>>(
        "SELECT array_agg(point_id ORDER BY point_id)
           FROM pgcontext._visible_collection_points
          WHERE collection_id = (
                    SELECT collection_id FROM pgcontext._collection_acl
                     WHERE collection_name = 'stage_i_named_visits'
                )",
    )
    .expect("lookup point ids should query")
    .expect("lookup point ids should exist");
    let ids = point_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    assert_eq!(
        table_search_rows(&format!(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_named_visits',
                 pgcontext.query_rerank(
                     pgcontext.query_lookup(ARRAY[{ids}]::bigint[]), 1
                 )
             )"
        ))
        .len(),
        1
    );
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.last().map(|event| event.visits), Some(4));
}

#[pg_test]
fn automatic_observability_persists_executor_budget_exhaustion() {
    Spi::run(
        "CREATE TABLE public.stage_i_executor_budget (
             id bigint PRIMARY KEY,
             body text NOT NULL
         );
         INSERT INTO public.stage_i_executor_budget
         SELECT id, 'budget telemetry'
           FROM generate_series(1, 200) AS id;
         SELECT pgcontext.create_collection(
             'stage_i_executor_budget', 'public.stage_i_executor_budget'
         );
         SELECT pgcontext.backfill_points('stage_i_executor_budget', 500);",
    )
    .expect("executor budget telemetry fixture should be created");
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_i_executor_budget'",
    )
    .expect("executor budget collection lookup should succeed")
    .expect("executor budget collection should exist");
    let formula = format!("$score{}", "+1".repeat(100));
    let exhausted = std::panic::catch_unwind(|| {
        Spi::run(&format!(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_i_executor_budget',
                 pgcontext.query_formula(
                     pgcontext.query_full_text('budget telemetry', 'body', 200),
                     '{formula}'
                 )
             )"
        ))
        .expect("executor formula work budget should be exceeded");
    });
    assert!(exhausted.is_err());
    let events = crate::query_stats_async::test_events(collection_id);
    let event = events.last().expect("executor budget event should be captured");
    assert_eq!(event.completion, "budget_exhausted");
    assert_eq!(event.strategy, "postgres_full_text");
    assert_eq!(event.visits, 200);
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
    let automatic_text = query_stats_one_text(
        "SELECT COALESCE(string_agg(row_to_json(stats)::text, ' '), '')
           FROM pgcontext.query_execution_stats() AS stats
          WHERE collection_name = 'm10_query_stats_privacy'",
    );
    let raw_automatic_text = query_stats_one_text(
        "SELECT COALESCE(string_agg(row_to_json(stats)::text, ' '), '')
           FROM pgcontext._visible_query_stats AS stats
          WHERE collection_id = (
                    SELECT collection_id
                      FROM pgcontext._collection_acl
                     WHERE collection_name = 'm10_query_stats_privacy'
                )",
    );
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'm10_query_stats_privacy'",
    )
    .expect("privacy collection lookup should succeed")
    .expect("privacy collection should exist");
    let queued_text = format!("{:?}", crate::query_stats_async::test_events(collection_id));
    let persisted_text = format!(
        "{telemetry_text} {cohort_text} {automatic_text} {raw_automatic_text} {queued_text}"
    );

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
#[should_panic(expected = "query cohort 'automatic' is reserved for executor telemetry")]
fn record_query_stat_rejects_the_automatic_cohort() {
    create_query_stats_collection("m10_query_stats_reserved_cohort");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
            'm10_query_stats_reserved_cohort',
            'automatic',
            'search',
            1,
            1,
            1.0
        )",
    )
    .expect("reserved automatic cohort should fail");
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

fn complete_test_summary(strategy: &'static str) -> crate::query_stats_async::AutomaticQuerySummary {
    crate::query_stats_async::AutomaticQuerySummary {
        result_count: 1,
        visits: 1,
        filter_candidates: 0,
        candidates: 1,
        rechecks: 1,
        stages: 1,
        expansions: 0,
        completion: "complete",
        lifecycle: "Exact",
        strategy,
    }
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
