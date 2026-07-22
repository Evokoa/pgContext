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
