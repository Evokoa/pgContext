#[pg_test]
fn query_builders_construct_nested_query_json() {
    let plan = json_value(
        "SELECT pgcontext.query_rerank(
            pgcontext.query_prefetch(ARRAY[
                pgcontext.query_weight(
                    pgcontext.query_nearest('[1,2]'::vector, 10),
                    0.75
                ),
                pgcontext.query_score_threshold(
                    pgcontext.query_recommend(ARRAY[1, 2]::bigint[], ARRAY[3]::bigint[], 5),
                    0.1,
                    0.9
                ),
                pgcontext.query_formula(
                    pgcontext.query_discover(ARRAY[4]::bigint[], 6),
                    '$score * 0.5'
                ),
                pgcontext.query_lookup(ARRAY[7, 8]::bigint[])
            ]),
            3
        )::jsonb",
    );

    assert_eq!(plan["kind"], "rerank");
    assert_eq!(plan["limit"], 3);
    assert_eq!(plan["branch"]["kind"], "prefetch");
    assert_eq!(plan["branch"]["branches"][0]["kind"], "weight");
    assert_eq!(plan["branch"]["branches"][0]["branch"]["kind"], "nearest");
    assert_eq!(plan["branch"]["branches"][1]["branch"]["kind"], "recommend");
    assert_eq!(plan["branch"]["branches"][2]["branch"]["kind"], "discover");
    assert_eq!(plan["branch"]["branches"][3]["kind"], "lookup");
}

#[pg_test]
fn execute_query_runs_nested_constructor_plan() {
    create_dense_hnsw_adapter_collection(
        "stage_g_execute_plan",
        "l2",
        "vector_hnsw_ops",
    );
    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_execute_plan',
               pgcontext.query_rerank(
                   pgcontext.query_prefetch(ARRAY[
                       pgcontext.query_nearest('[1,0]'::vector, 2),
                       pgcontext.query_nearest('[0,1]'::vector, 2)
                   ]),
                   2
               )
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[1].1, "10");
}

#[pg_test]
fn execute_query_preserves_lower_is_better_rerank_order() {
    create_dense_hnsw_adapter_collection("stage_g_lower_rerank", "l2", "vector_hnsw_ops");
    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_lower_rerank',
               pgcontext.query_rerank(
                   pgcontext.query_nearest('[1,0]'::vector, 3),
                   1
               )
           )",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "10");
}

#[pg_test]
fn execute_query_allocates_candidate_budget_to_every_prefetch_branch() {
    Spi::run(
        "CREATE TABLE public.stage_g_branch_budget (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL
         );
         INSERT INTO public.stage_g_branch_budget
         SELECT value,
                ARRAY[
                    (value::real / 200::real)::real,
                    (1::real - value::real / 200::real)::real
                ]::real[]::vector
           FROM generate_series(1, 200) AS value;
         SELECT pgcontext.create_collection(
             'stage_g_branch_budget', 'public.stage_g_branch_budget'
         );
         SELECT pgcontext.register_vector(
             'stage_g_branch_budget', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('stage_g_branch_budget', 500);
         CREATE INDEX stage_g_branch_budget_hnsw
             ON public.stage_g_branch_budget
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_g_branch_budget', 'embedding',
             'public.stage_g_branch_budget_hnsw'
         );
         SET LOCAL pgcontext.hnsw_candidate_budget = 32;",
    )
    .expect("realistic composite fixture should be created");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_branch_budget',
               pgcontext.query_rerank(
                   pgcontext.query_prefetch(ARRAY[
                       pgcontext.query_nearest('[0,1]'::vector, 10),
                       pgcontext.query_nearest('[1,0]'::vector, 10)
                   ]),
                   10
               )
           )",
    );
    assert_eq!(rows.len(), 10);
}

#[pg_test]
fn execute_query_routes_named_dense_vectors_and_filters() {
    Spi::run(
        "CREATE TABLE public.stage_g_named_dense (
             id bigint PRIMARY KEY,
             primary_embedding vector(2) NOT NULL,
             secondary_embedding vector(2) NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.stage_g_named_dense VALUES
             (1, '[1,0]', '[0,1]', 'acme'),
             (2, '[0,1]', '[1,0]', 'acme'),
             (3, '[0,1]', '[1,0]', 'other');
         SELECT pgcontext.create_collection(
             'stage_g_named_dense', 'public.stage_g_named_dense'
         );
         SELECT pgcontext.register_vector(
             'stage_g_named_dense', 'primary', 'primary_embedding', 2, 'l2'
         );
         SELECT pgcontext.register_vector(
             'stage_g_named_dense', 'secondary', 'secondary_embedding', 2, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             'stage_g_named_dense', 'tenant', 'tenant'
         );
         SELECT pgcontext.backfill_points('stage_g_named_dense', 100);
         CREATE INDEX stage_g_named_dense_secondary_hnsw
             ON public.stage_g_named_dense
             USING pgcontext_hnsw (secondary_embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_g_named_dense', 'secondary',
             'public.stage_g_named_dense_secondary_hnsw'
         );
         SELECT pgcontext.configure_vector(
             'stage_g_named_dense',
             'secondary',
             '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":8}'::jsonb,
             'ready'
         );",
    )
    .expect("named dense fixture should be created");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_named_dense',
               pgcontext.query_nearest(
                   'secondary',
                   '[1,0]'::vector,
                   '{\"must\":[{\"key\":\"tenant\",\"match\":\"acme\"}]}'::jsonb,
                   2
               )
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "2");
    assert!(rows.iter().all(|row| row.1 != "3"));

    let unfiltered = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_named_dense',
               pgcontext.query_nearest(
                   'secondary', '[1,0]'::vector, NULL::jsonb, 2
               )
           )",
    );
    assert_eq!(unfiltered.len(), 2);
    assert_eq!(unfiltered[0].1, "2");
}

#[pg_test]
fn execute_query_non_dense_leaves_do_not_require_a_dense_registration() {
    Spi::run(
        "CREATE TABLE public.stage_g_non_dense_only (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             body text NOT NULL,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.stage_g_non_dense_only VALUES
             (1, '{1:1}/2'::sparsevec, 'rust postgres', ARRAY['[1,0]'::vector]),
             (2, '{2:1}/2'::sparsevec, 'hybrid search', ARRAY['[0,1]'::vector]);
         SELECT pgcontext.create_collection(
             'stage_g_non_dense_only', 'public.stage_g_non_dense_only'
         );
         SELECT pgcontext.register_sparse_vector(
             'stage_g_non_dense_only', 'keywords', 'lexical', 2, 'cosine'
         );
         SELECT pgcontext.upsert_points(
             'stage_g_non_dense_only', ARRAY['1', '2']
         );
         SELECT * FROM pgcontext.register_late_interaction(
             'stage_g_non_dense_only',
             'public.stage_g_non_dense_only',
             'token_vectors'
         );",
    )
    .expect("non-dense-only fixture should be created");

    let sparse = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_non_dense_only',
               pgcontext.query_sparse_nearest(
                   'keywords', '{1:1}/2'::sparsevec, 2
               )
           )",
    );
    assert_eq!(sparse.len(), 2);
    assert_eq!(sparse[0].1, "1");

    let full_text = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_non_dense_only',
               pgcontext.query_full_text('postgres', 'body', 2)
           )",
    );
    assert_eq!(full_text.len(), 1);
    assert_eq!(full_text[0].1, "1");

    let point_id = Spi::get_one::<i64>(
        "SELECT point_id
           FROM pgcontext._visible_collection_points
          WHERE collection_id = (
                    SELECT collection_id
                      FROM pgcontext._collection_acl
                     WHERE collection_name = 'stage_g_non_dense_only'
                )
            AND source_key = '2'",
    )
    .expect("lookup point query should execute")
    .expect("lookup point should exist");
    let lookup = table_search_rows(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_non_dense_only',
               pgcontext.query_lookup(ARRAY[{point_id}]::bigint[])
           )"
    ));
    assert_eq!(lookup.len(), 1);
    assert_eq!(lookup[0].1, "2");

    let late = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_non_dense_only',
               pgcontext.query_late_interaction(
                   ARRAY['[1,0]'::vector], 2, 2
               )
           )",
    );
    assert_eq!(late.len(), 2);
    assert_eq!(late[0].1, "1");
}

#[pg_test]
fn execute_query_filtered_quantized_vector_uses_masked_full_precision_hnsw() {
    Spi::run(
        "CREATE TABLE public.stage_g_quantized_filter (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.stage_g_quantized_filter VALUES
             (1, '[1,0]', 'other'),
             (2, '[0.8,0.2]', 'acme'),
             (3, '[0,1]', 'acme');
         SELECT pgcontext.create_collection(
             'stage_g_quantized_filter', 'public.stage_g_quantized_filter'
         );
         SELECT pgcontext.register_vector(
             'stage_g_quantized_filter', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             'stage_g_quantized_filter', 'tenant', 'tenant'
         );
         SELECT pgcontext.backfill_points('stage_g_quantized_filter', 100);
         CREATE INDEX stage_g_quantized_filter_hnsw
             ON public.stage_g_quantized_filter
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_g_quantized_filter', 'embedding',
             'public.stage_g_quantized_filter_hnsw'
         );
         SELECT pgcontext.configure_vector(
             'stage_g_quantized_filter',
             'embedding',
             '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":8}'::jsonb,
             'ready'
         );",
    )
    .expect("filtered quantized fallback fixture should be created");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_quantized_filter',
               pgcontext.query_nearest(
                   NULL::text,
                   '[1,0]'::vector,
                   '{\"must\":[{\"key\":\"tenant\",\"match\":\"acme\"}]}'::jsonb,
                   2
               )
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "2");
    assert!(rows.iter().all(|row| row.1 != "1"));
}

#[pg_test]
fn execute_query_recommendation_matches_the_exact_oracle() {
    Spi::run(
        "CREATE TABLE public.stage_g_recommend_order (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL
         );
         INSERT INTO public.stage_g_recommend_order VALUES
             (1, '[1,0]'), (2, '[0.9,0.1]'), (3, '[0,1]'), (4, '[-1,0]');
         SELECT pgcontext.create_collection(
             'stage_g_recommend_order', 'public.stage_g_recommend_order'
         );
         SELECT pgcontext.register_vector(
             'stage_g_recommend_order', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('stage_g_recommend_order', 100);",
    )
    .expect("recommendation fixture should be created");
    let positive = Spi::get_one::<i64>(
        "SELECT point_id
           FROM pgcontext._visible_collection_points
          WHERE collection_id = (
                    SELECT collection_id
                      FROM pgcontext._collection_acl
                     WHERE collection_name = 'stage_g_recommend_order'
                )
            AND source_key = '1'",
    )
    .expect("positive point lookup should execute")
    .expect("positive point should exist");
    let exact = table_search_rows(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.recommend(
               'stage_g_recommend_order', ARRAY[{positive}]::bigint[], ARRAY[]::bigint[], 2
           )"
    ));
    let composite = table_search_rows(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_recommend_order',
               pgcontext.query_rerank(
                   pgcontext.query_recommend(
                       ARRAY[{positive}]::bigint[], ARRAY[]::bigint[], 3
                   ),
                   2
               )
           )"
    ));
    assert_eq!(composite, exact);
}

#[pg_test]
fn execute_query_routes_quantized_mapped_hnsw_and_exactly_rechecks() {
    create_search_collection("stage_g_quantized_composite");
    Spi::run(
        "SELECT pgcontext.configure_vector(
             'stage_g_quantized_composite',
             'embedding',
             '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":8}'::jsonb,
             'ready'
         )",
    )
    .expect("quantized vector policy should configure");
    upsert_search_points("stage_g_quantized_composite", &["10", "20", "30"]);
    let job_id = start_artifact_build_job(
        "stage_g_quantized_composite",
        "mmap",
        "composite-quantized",
        0,
    );
    Spi::run(&format!("SELECT pgcontext.run_build_job({job_id}, 1)"))
        .expect("quantized composite build should complete");
    let published = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {job_id},
                pgcontext.build_mmap_hnsw_artifact({job_id})
           )"
    ));
    assert_eq!(published.len(), 1);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_quantized_composite',
               pgcontext.query_nearest('[0,0]'::vector, 2)
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].1, "30");
    assert_eq!(rows[1].2, 2.0);
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_g_quantized_composite'",
    )
    .expect("quantized telemetry collection lookup should succeed")
    .expect("quantized telemetry collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].strategy, "quantized_mmap_hnsw");
    assert_eq!(events[0].lifecycle, "Indexed");
    assert!(events[0].visits >= events[0].candidates);
    assert!(events[0].candidates >= events[0].rechecks);

    Spi::run(
        "SELECT pgcontext.configure_vector(
             'stage_g_quantized_composite', 'embedding', '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":16}'::jsonb, 'ready'
         )",
    )
    .expect("quantized policy change should require a rebuild");
    let rebuild = std::panic::catch_unwind(|| {
        Spi::run(
            "SELECT * FROM pgcontext.execute_query(
                 'stage_g_quantized_composite',
                 pgcontext.query_nearest('[0,0]'::vector, 2)
             )",
        )
        .expect("stale quantized generation should fail");
    });
    assert!(rebuild.is_err());
    let events = crate::query_stats_async::test_events(collection_id);
    let rebuild = events.last().expect("rebuild event should be captured");
    assert_eq!(rebuild.completion, "error");
    assert_eq!(rebuild.lifecycle, "IndexNotReady");
}

#[pg_test]
fn execute_query_rejects_unknown_plan_fields() {
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.execute_query(
            'missing',
            '{\"kind\":\"nearest\",\"vector\":[1],\"limit\":1,\"sql\":\"select 1\"}'::jsonb
        )",
        "22023",
        "query node contains an unknown field",
        "executable query plan validation",
    );
}

#[pg_test]
fn execute_query_composes_all_named_postgres_sources() {
    Spi::run(
        "CREATE TABLE public.stage_g_named_sources (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             sparse_embedding sparsevec NOT NULL,
             body text NOT NULL,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.stage_g_named_sources VALUES
             (1, '[1,0]'::vector, '{1:1}/2'::sparsevec, 'rust postgres',
                 ARRAY['[1,0]'::vector, '[0.8,0.2]'::vector]),
             (2, '[0.5,0.5]'::vector, '{1:0.5,2:0.5}/2'::sparsevec, 'hybrid search',
                 ARRAY['[0.5,0.5]'::vector]),
             (3, '[0,1]'::vector, '{2:1}/2'::sparsevec, 'postgres search',
                 ARRAY['[0,1]'::vector, '[0.2,0.8]'::vector]);
         SELECT pgcontext.create_collection(
             'stage_g_named_sources', 'public.stage_g_named_sources'
         );
         SELECT pgcontext.register_vector(
             'stage_g_named_sources', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.register_sparse_vector(
             'stage_g_named_sources', 'keywords', 'sparse_embedding', 2, 'cosine'
         );
         SELECT pgcontext.upsert_points(
             'stage_g_named_sources', ARRAY['1', '2', '3']
         );
         CREATE INDEX stage_g_named_sources_hnsw
             ON public.stage_g_named_sources
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_g_named_sources', 'embedding',
             'public.stage_g_named_sources_hnsw'
         );
         SELECT * FROM pgcontext.register_late_interaction(
             'stage_g_named_sources', 'public.stage_g_named_sources', 'token_vectors'
         );",
    )
    .expect("named-source fixture should be created");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'stage_g_named_sources',
               pgcontext.query_rerank(
                   pgcontext.query_prefetch(ARRAY[
                       pgcontext.query_nearest('[1,0]'::vector, 3),
                       pgcontext.query_sparse_nearest(
                           'keywords', '{1:1}/2'::sparsevec, 3
                       ),
                       pgcontext.query_full_text('postgres', 'body', 3),
                       pgcontext.query_late_interaction(
                           ARRAY['[1,0]'::vector], 3, 3
                       )
                   ]),
                   3
               )
           )",
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].1, "1");
    assert_eq!(
        rows.iter()
            .map(|row| row.0)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        rows.len()
    );
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'stage_g_named_sources'",
    )
    .expect("composite telemetry collection lookup should succeed")
    .expect("composite telemetry collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].query_kind, "hybrid");
    assert_eq!(events[0].strategy, "composite_hnsw");
    assert_eq!(events[0].lifecycle, "Indexed");
    assert!(events[0].filter_candidates <= events[0].visits);
    assert!(events[0].stages >= 9);
}

#[pg_test]
#[should_panic(expected = "query limit must be positive: 0")]
fn query_nearest_rejects_zero_limits() {
    Spi::run("SELECT pgcontext.query_nearest('[1,2]'::vector, 0)")
        .expect("zero query limit should be rejected");
}

#[pg_test]
#[should_panic(expected = "recommend query requires at least one positive point id")]
fn query_recommend_rejects_empty_positive_points() {
    Spi::run(
        "SELECT pgcontext.query_recommend(
            ARRAY[]::bigint[],
            ARRAY[]::bigint[],
            5
        )",
    )
    .expect("empty recommend positives should be rejected");
}

#[pg_test]
#[should_panic(expected = "query point id must be positive: -1")]
fn query_lookup_rejects_negative_point_ids() {
    Spi::run("SELECT pgcontext.query_lookup(ARRAY[1, -1]::bigint[])")
        .expect("negative point id should be rejected");
}

#[pg_test]
#[should_panic(expected = "prefetch query requires at least one branch")]
fn query_prefetch_rejects_empty_branches() {
    Spi::run("SELECT pgcontext.query_prefetch(ARRAY[]::jsonb[])")
        .expect("empty prefetch branches should be rejected");
}

#[pg_test]
#[should_panic(expected = "query branch weight must be finite and non-negative")]
fn query_weight_rejects_negative_weights() {
    Spi::run(
        "SELECT pgcontext.query_weight(
            pgcontext.query_nearest('[1,2]'::vector, 5),
            -0.1
        )",
    )
    .expect("negative branch weight should be rejected");
}

#[pg_test]
#[should_panic(expected = "query score threshold min_score must not exceed max_score")]
fn query_score_threshold_rejects_inverted_ranges() {
    Spi::run(
        "SELECT pgcontext.query_score_threshold(
            pgcontext.query_nearest('[1,2]'::vector, 5),
            0.9,
            0.1
        )",
    )
    .expect("inverted score threshold should be rejected");
}

#[pg_test]
#[should_panic(expected = "query formula must be 1..=512 bytes")]
fn query_formula_rejects_empty_formulas() {
    Spi::run("SELECT pgcontext.query_formula(pgcontext.query_nearest('[1,2]'::vector, 5), '')")
        .expect("empty formula should be rejected");
}

#[pg_test]
fn query_formula_preserves_whitespace_and_512_byte_formulas() {
    let whitespace = json_value(
        "SELECT pgcontext.query_formula('{\"kind\":\"lookup\"}'::jsonb, '   ')::jsonb",
    );
    assert_eq!(whitespace["formula"], "   ");

    let formula = "x".repeat(512).replace('\'', "''");
    let plan = json_value(&format!(
        "SELECT pgcontext.query_formula('{{\"kind\":\"lookup\"}}'::jsonb, '{formula}')::jsonb"
    ));
    assert_eq!(plan["formula"].as_str().map(str::len), Some(512));
}

#[pg_test]
#[should_panic(expected = "query formula must be 1..=512 bytes")]
fn query_formula_rejects_513_byte_formulas() {
    let formula = "x".repeat(513).replace('\'', "''");
    Spi::run(&format!(
        "SELECT pgcontext.query_formula('{{\"kind\":\"lookup\"}}'::jsonb, '{formula}')"
    ))
    .expect("oversized formula should be rejected");
}

#[pg_test]
fn query_builder_semantic_errors_use_invalid_parameter_sqlstate() {
    let cases = [
        (
            "SELECT pgcontext.query_nearest('[1,2]'::vector, 0)",
            "query limit must be positive: 0",
        ),
        (
            "SELECT pgcontext.query_lookup(ARRAY[1, -1]::bigint[])",
            "query point id must be positive: -1",
        ),
        (
            "SELECT pgcontext.query_weight('{\"kind\":\"lookup\"}'::jsonb, -0.1)",
            "query branch weight must be finite and non-negative: -0.1",
        ),
        (
            "SELECT pgcontext.query_score_threshold('{\"kind\":\"lookup\"}'::jsonb, 0.9, 0.1)",
            "query score threshold min_score must not exceed max_score",
        ),
        (
            "SELECT pgcontext.query_formula('{\"kind\":\"lookup\"}'::jsonb, '')",
            "query formula must be 1..=512 bytes",
        ),
    ];
    for (sql, message) in cases {
        shared_assert_sql_failure(sql, "22023", message, "query builder semantic validation");
    }
}

fn json_value(sql: &str) -> serde_json::Value {
    Spi::get_one::<pgrx::JsonB>(sql)
        .expect("json query should succeed")
        .expect("json query should return a row")
        .0
}
