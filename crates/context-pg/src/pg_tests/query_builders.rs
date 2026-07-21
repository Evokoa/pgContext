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
