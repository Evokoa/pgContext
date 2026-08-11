use pgrx::JsonB;
use serde_json::json;

fn multi_model_corpus(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             tenant text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1,
             legacy_version bigint,
             modern_version bigint,
             legacy pgcontext.vector(4) NOT NULL,
             modern pgcontext.vector(8)
         );
         INSERT INTO public.{collection_name} (
             id, tenant, legacy_version, modern_version, legacy, modern
         )
         SELECT id,
                CASE WHEN id % 2 = 0 THEN 'even' ELSE 'odd' END,
                1,
                CASE WHEN id % 2 = 0 THEN 1 END,
                ARRAY[id, 1, 0, 0]::real[]::pgcontext.vector,
                CASE WHEN id % 2 = 0
                     THEN ARRAY[id, 1, 0, 0, 0, 0, 0, 0]::real[]::pgcontext.vector
                END
           FROM pg_catalog.generate_series(1, 20) AS id;
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.register_vector(
             '{collection_name}', 'legacy', 'legacy', 4, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             '{collection_name}', 'tenant', 'tenant'
         );
         SELECT pgcontext.backfill_points('{collection_name}', 100);
         CREATE INDEX {collection_name}_legacy_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
         CREATE INDEX {collection_name}_modern_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);"
    ))
    .expect("multi-model corpus should be created");
}

fn register_versioned_profile(
    collection_name: &str,
    profile_name: &str,
    column: &str,
    embedding_version_column: &str,
    dimensions: i32,
    lifecycle: &str,
) {
    Spi::run(&format!(
        "SELECT pgcontext.register_embedding_profile(
             '{collection_name}', '{profile_name}', '{column}',
             'public.{collection_name}_{column}_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', {dimensions},
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', '{profile_name}', 'revision', '1',
                 'input_template', '{{t}}', 'output_template', '{{v}}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'source_version_column', 'source_version',
                 'embedding_version_column', '{embedding_version_column}'
             ),
             '{lifecycle}'
         )"
    ))
    .expect("versioned embedding profile should be registered");
}

fn register_multi_model_profile(
    collection_name: &str,
    profile_name: &str,
    column: &str,
    dimensions: i32,
    lifecycle: &str,
) {
    Spi::run(&format!(
        "SELECT pgcontext.register_embedding_profile(
             '{collection_name}', '{profile_name}', '{column}',
             'public.{collection_name}_{column}_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', {dimensions},
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', '{profile_name}', 'revision', '1',
                 'input_template', '{{t}}', 'output_template', '{{v}}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef'
             ),
             '{lifecycle}'
         )"
    ))
    .expect("embedding profile should be registered");
}

fn profile_lifecycle(collection_name: &str, profile_name: &str) -> String {
    Spi::get_one::<String>(&format!(
        "SELECT lifecycle FROM pgcontext.embedding_profile_coverage('{collection_name}')
          WHERE profile_name = '{profile_name}'"
    ))
    .expect("coverage query should succeed")
    .expect("profile should exist")
}

/// Asserts a data-modifying statement fails with an exact SQLSTATE and message.
///
/// `shared_assert_sql_failure` runs its argument as a subquery, and PostgreSQL
/// allows neither a bare UPDATE nor a data-modifying CTE there, so DML needs its
/// own harness that executes the statement directly inside the exception block.
fn assert_dml_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let escaped = sql.replace('\'', "''");
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $harness$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{escaped}';
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %, message: %',
                        actual_sqlstate, SQLERRM;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
            END;
        END $harness$;
        "#
    ))
    .expect("invalid DML should raise the expected error");
}

fn assert_sql_failure_exact_without(
    sql: &str,
    sqlstate: &str,
    message: &str,
    forbidden: &str,
    context: &str,
) {
    let escaped = sql.replace('\'', "''");
    let message = message.replace('\'', "''");
    let forbidden = forbidden.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $harness$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{escaped}';
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %, message: %',
                        actual_sqlstate, SQLERRM;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
                IF position('{forbidden}' IN SQLERRM) <> 0 THEN
                    RAISE EXCEPTION 'raw query leaked through {context} error: %', SQLERRM;
                END IF;
            END;
        END $harness$;
        "#
    ))
    .expect("invalid SQL should raise the exact bounded error without raw input");
}

fn assert_multi_model_sqlstate(sql: &str, sqlstate: &str, context: &str) {
    let escaped = sql.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $harness$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{escaped}';
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %, message: %',
                        actual_sqlstate, SQLERRM;
                END IF;
            END;
        END $harness$;
        "#
    ))
    .expect("invalid SQL should raise the expected SQLSTATE");
}

#[pg_test]
fn multi_model_profiles_register_into_shadow_or_active_only() {
    multi_model_corpus("mm_register");
    register_multi_model_profile("mm_register", "legacy_v1", "legacy", 4, "active");
    register_multi_model_profile("mm_register", "modern_v2", "modern", 8, "shadow");

    assert_eq!(profile_lifecycle("mm_register", "legacy_v1"), "active");
    assert_eq!(profile_lifecycle("mm_register", "modern_v2"), "shadow");

    for rejected in ["draining", "retired", "failed"] {
        shared_assert_sql_failure(
            &format!(
                "SELECT pgcontext.register_embedding_profile(
                     'mm_register', 'invalid_{rejected}', 'legacy',
                     'public.mm_register_legacy_hnsw',
                     jsonb_build_object(
                         'representation', 'dense', 'dimensions', 4,
                         'normalization', 'none', 'metric', 'l2',
                         'provider', 'acme', 'model', 'm', 'revision', '{rejected}',
                         'input_template', '{{t}}', 'output_template', '{{v}}',
                         'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                         'zero_point', NULL,
                         'configuration_hash', '0123456789abcdef'
                     ),
                     '{rejected}'
                 )"
            ),
            "22023",
            &format!("embedding profiles register into shadow or active, not {rejected}"),
            "registration into a non-initial lifecycle",
        );
    }
    shared_assert_sql_failure(
        "SELECT pgcontext.register_embedding_profile(
             'mm_register', 'invalid_state', 'legacy', 'public.mm_register_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'm', 'revision', 'paused',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef'
             ),
             'paused'
         )",
        "22023",
        "invalid vector: unsupported embedding profile lifecycle: paused",
        "unknown lifecycle",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.register_embedding_profile(
             'mm_register', E'bad\\nname', 'legacy', 'public.mm_register_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'm', 'revision', 'control-name',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef'
             ),
             'active'
         )",
        "22023",
        "invalid profile name: must not contain control characters: \"bad\\nname\"",
        "control character in profile name",
    );
}

#[pg_test]
fn multi_model_lifecycle_walks_a_cutover_and_rejects_illegal_moves() {
    multi_model_corpus("mm_cutover");
    register_multi_model_profile("mm_cutover", "legacy_v1", "legacy", 4, "active");

    for (next, expected) in [
        ("draining", "draining"),
        ("active", "active"),
        ("draining", "draining"),
        ("retired", "retired"),
    ] {
        let observed = Spi::get_one::<String>(&format!(
            "SELECT pgcontext.set_embedding_profile_lifecycle(
                 'mm_cutover', 'legacy_v1', '{next}'
             )"
        ))
        .expect("transition should succeed")
        .expect("transition should return a state");
        assert_eq!(observed, expected);
    }
    assert_eq!(profile_lifecycle("mm_cutover", "legacy_v1"), "retired");

    // Retired is terminal.
    for next in ["active", "shadow", "draining", "failed"] {
        shared_assert_sql_failure(
            &format!(
                "SELECT pgcontext.set_embedding_profile_lifecycle(
                     'mm_cutover', 'legacy_v1', '{next}'
                 )"
            ),
            "22023",
            &format!("invalid vector: embedding profile cannot move from retired to {next}"),
            "transition out of a terminal state",
        );
    }
}

#[pg_test]
fn multi_model_shadow_promotes_and_a_failed_profile_must_re_backfill() {
    multi_model_corpus("mm_shadow");
    register_multi_model_profile("mm_shadow", "modern_v2", "legacy", 4, "shadow");

    shared_assert_sql_failure(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'draining')",
        "22023",
        "invalid vector: embedding profile cannot move from shadow to draining",
        "shadow to draining",
    );

    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'active')",
    )
    .expect("shadow should promote to active");
    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'failed')",
    )
    .expect("active should be withdrawable");

    shared_assert_sql_failure(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'active')",
        "22023",
        "invalid vector: embedding profile cannot move from failed to active",
        "failed straight back to active",
    );
    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'shadow')",
    )
    .expect("a failed profile should re-backfill");
    assert_eq!(profile_lifecycle("mm_shadow", "modern_v2"), "shadow");
}

#[pg_test]
fn multi_model_coverage_reports_serving_state_and_partial_backfill() {
    multi_model_corpus("mm_coverage");
    register_multi_model_profile("mm_coverage", "legacy_v1", "legacy", 4, "active");
    register_multi_model_profile("mm_coverage", "modern_v2", "modern", 8, "shadow");

    let rows = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT profile_name, lifecycle, serves_queries, covered_points, active_points
                   FROM pgcontext.embedding_profile_coverage('mm_coverage')
                  ORDER BY profile_name",
                None,
                &[],
            )
            .expect("coverage query should succeed");
        rows.into_iter()
            .map(|row| {
                (
                    row.get::<String>(1).ok().flatten().unwrap_or_default(),
                    row.get::<String>(2).ok().flatten().unwrap_or_default(),
                    row.get::<bool>(3).ok().flatten().unwrap_or_default(),
                    row.get::<i64>(4).ok().flatten().unwrap_or_default(),
                    row.get::<i64>(5).ok().flatten().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
    });

    assert_eq!(rows.len(), 2);
    // The legacy column is fully populated; the modern column covers only the
    // even ids, which is exactly the partial-backfill gap coverage exists to
    // surface before a cutover.
    assert_eq!(
        rows[0],
        ("legacy_v1".to_owned(), "active".to_owned(), true, 20, 20)
    );
    assert_eq!(
        rows[1],
        ("modern_v2".to_owned(), "shadow".to_owned(), false, 10, 20)
    );
}

#[pg_test]
fn multi_model_version_bindings_distinguish_current_stale_and_missing_embeddings() {
    multi_model_corpus("mm_versions");
    register_versioned_profile(
        "mm_versions",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    register_versioned_profile(
        "mm_versions",
        "modern_v2",
        "modern",
        "modern_version",
        8,
        "active",
    );

    Spi::run(
        "UPDATE public.mm_versions
            SET source_version = 2
          WHERE id IN (2, 3)",
    )
    .expect("source edits should advance the authoritative version");

    let rows = Spi::connect(|client| {
        client
            .select(
                "SELECT profile_name, covered_points, stale_points, active_points
                   FROM pgcontext.embedding_profile_coverage('mm_versions')
                  ORDER BY profile_name",
                None,
                &[],
            )
            .expect("versioned coverage should succeed")
            .into_iter()
            .map(|row| {
                (
                    row.get::<String>(1).ok().flatten().unwrap_or_default(),
                    row.get::<i64>(2).ok().flatten().unwrap_or_default(),
                    row.get::<i64>(3).ok().flatten().unwrap_or_default(),
                    row.get::<i64>(4).ok().flatten().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
    });

    assert_eq!(
        rows,
        vec![
            ("legacy_v1".to_owned(), 18, 2, 20),
            ("modern_v2".to_owned(), 9, 1, 20),
        ]
    );
}

#[pg_test]
fn multi_model_version_bindings_are_paired_bigint_columns() {
    multi_model_corpus("mm_version_contract");

    shared_assert_sql_failure(
        "SELECT pgcontext.register_embedding_profile(
             'mm_version_contract', 'unpaired', 'legacy',
             'public.mm_version_contract_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'm', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'source_version_column', 'source_version'
             )
         )",
        "22023",
        "source_version_column and embedding_version_column must be supplied together",
        "unpaired version binding",
    );

    Spi::run("ALTER TABLE public.mm_version_contract ADD COLUMN bad_version text")
        .expect("bad version fixture should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_embedding_profile(
             'mm_version_contract', 'wrong_type', 'legacy',
             'public.mm_version_contract_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'm', 'revision', '2',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', 'fedcba9876543210',
                 'source_version_column', 'source_version',
                 'embedding_version_column', 'bad_version'
             )
         )",
        "42804",
        "embedding_version_column must be a pg_catalog.int8 column: bad_version",
        "non-bigint version binding",
    );
}

#[pg_test]
fn multi_model_query_fuses_current_rows_and_reports_profile_contributions() {
    multi_model_corpus("mm_query");
    register_versioned_profile(
        "mm_query",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    register_versioned_profile(
        "mm_query",
        "modern_v2",
        "modern",
        "modern_version",
        8,
        "active",
    );

    let report = Spi::get_one::<JsonB>(
        r#"SELECT pgcontext.query_multi_model(
               'mm_query',
               jsonb_build_array(
                   jsonb_build_object(
                       'profile', 'legacy_v1',
                       'configuration_hash', '0123456789abcdef',
                       'query', '[1,1,0,0]',
                       'limit', 10,
                       'weight', 1.0
                   ),
                   jsonb_build_object(
                       'profile', 'modern_v2',
                       'configuration_hash', '0123456789abcdef',
                       'query', '[2,1,0,0,0,0,0,0]',
                       'limit', 10,
                       'weight', 2.0
                   )
               ),
               NULL,
               10,
               60,
               100,
               true
           )"#,
    )
    .expect("multi-model query should succeed")
    .expect("multi-model query should return a report");

    assert_eq!(report.0["completion"], "complete");
    assert_eq!(report.0["missing_profiles"], json!([]));
    let results = report.0["results"].as_array().expect("result array");
    assert!(!results.is_empty());
    assert!(results.len() <= 10);
    assert!(results.iter().all(|row| {
        row["contributions"]
            .as_array()
            .is_some_and(|contributions| !contributions.is_empty())
    }));
    assert!(
        results
            .iter()
            .flat_map(|row| row["contributions"].as_array().into_iter().flatten())
            .any(|entry| entry["profile"] == "modern_v2")
    );

    let filtered = Spi::get_one::<JsonB>(
        r#"SELECT pgcontext.query_multi_model(
               'mm_query',
               jsonb_build_array(
                   jsonb_build_object(
                       'profile', 'legacy_v1',
                       'configuration_hash', '0123456789abcdef',
                       'query', '[1,1,0,0]',
                       'limit', 10,
                       'weight', 1.0
                   ),
                   jsonb_build_object(
                       'profile', 'modern_v2',
                       'configuration_hash', '0123456789abcdef',
                       'query', '[2,1,0,0,0,0,0,0]',
                       'limit', 10,
                       'weight', 2.0
                   )
               ),
               '{"must":[{"key":"tenant","match":"even"}]}'::jsonb,
               10,
               60,
               100,
               true
           )"#,
    )
    .expect("filtered multi-model query should succeed")
    .expect("filtered multi-model query should return a report");
    assert!(
        filtered.0["results"]
            .as_array()
            .expect("filtered result array")
            .iter()
            .all(|row| row["source_key"]
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                .is_some_and(|source_key| source_key % 2 == 0))
    );
}

#[pg_test]
fn multi_model_query_restores_caller_planner_settings() {
    multi_model_corpus("mm_planner_restore");
    register_versioned_profile(
        "mm_planner_restore",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    Spi::run(
        "SET enable_indexscan = off;
         SET enable_bitmapscan = on;
         SET enable_seqscan = on",
    )
    .expect("caller planner settings should apply");
    let before = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(',',
             pg_catalog.current_setting('enable_indexscan'),
             pg_catalog.current_setting('enable_bitmapscan'),
             pg_catalog.current_setting('enable_seqscan'))",
    )
    .expect("planner settings should be readable")
    .expect("planner setting summary should be non-null");

    let report = Spi::get_one::<JsonB>(
        r#"SELECT pgcontext.query_multi_model(
               'mm_planner_restore',
               jsonb_build_array(jsonb_build_object(
                   'profile', 'legacy_v1',
                   'configuration_hash', '0123456789abcdef',
                   'query', '[1,1,0,0]',
                   'limit', 5,
                   'weight', 1.0
               )),
               NULL, 5, 60, 6, true
           )"#,
    )
    .expect("query should temporarily force its validated HNSW plan")
    .expect("query should return a report");
    assert_eq!(report.0["completion"], "complete");

    let after = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(',',
             pg_catalog.current_setting('enable_indexscan'),
             pg_catalog.current_setting('enable_bitmapscan'),
             pg_catalog.current_setting('enable_seqscan'))",
    )
    .expect("planner settings should remain readable")
    .expect("planner setting summary should be non-null");
    assert_eq!(after, before);
    Spi::run("RESET enable_indexscan; RESET enable_bitmapscan; RESET enable_seqscan")
        .expect("caller planner settings should reset");
}

#[pg_test]
fn multi_model_query_requires_explicit_degraded_service() {
    multi_model_corpus("mm_degraded");
    register_versioned_profile(
        "mm_degraded",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    register_versioned_profile(
        "mm_degraded",
        "modern_v2",
        "modern",
        "modern_version",
        8,
        "shadow",
    );
    let branches = r#"jsonb_build_array(
        jsonb_build_object(
            'profile', 'legacy_v1', 'configuration_hash', '0123456789abcdef',
            'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
        ),
        jsonb_build_object(
            'profile', 'modern_v2', 'configuration_hash', '0123456789abcdef',
            'query', '[2,1,0,0,0,0,0,0]', 'limit', 5, 'weight', 1.0
        )
    )"#;
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.query_multi_model(
                 'mm_degraded', {branches}, NULL, 5, 60, 20, true
             )"
        ),
        "55000",
        "multi-model query requires unavailable profile modern_v2: lifecycle_not_serving",
        "required shadow profile",
    );

    let report = Spi::get_one::<JsonB>(&format!(
        "SELECT pgcontext.query_multi_model(
             'mm_degraded', {branches}, NULL, 5, 60, 20, false
         )"
    ))
    .expect("explicit degraded query should succeed")
    .expect("degraded query should return a report");
    assert_eq!(report.0["completion"], "degraded");
    assert_eq!(report.0["missing_profiles"][0]["profile"], "modern_v2");
    assert_eq!(
        report.0["missing_profiles"][0]["reason"],
        "lifecycle_not_serving"
    );

    assert_sql_failure_exact_without(
        "SELECT pgcontext.query_multi_model(
             'mm_degraded',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'legacy_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'modern_v2',
                     'configuration_hash', '0123456789abcdef',
                     'query', 'not-a-vector', 'limit', 5, 'weight', 1.0
                 )
             ),
             NULL, 5, 60, 20, false
         )",
        "22023",
        "query does not match profile modern_v2",
        "not-a-vector",
        "malformed query in a degraded shadow branch",
    );
}

#[pg_test]
fn degraded_selection_cannot_hide_duplicate_registered_source_columns() {
    multi_model_corpus("mm_duplicate_source");
    register_versioned_profile(
        "mm_duplicate_source",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    Spi::run(
        "SELECT pgcontext.register_embedding_profile(
             'mm_duplicate_source', 'legacy_v2', 'legacy',
             'public.mm_duplicate_source_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'legacy_v2', 'revision', '2',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                 'zero_point', NULL,
                 'configuration_hash', 'fedcba9876543210',
                 'source_version_column', 'source_version',
                 'embedding_version_column', 'legacy_version'
             ),
             'shadow'
         )",
    )
    .expect("second immutable revision should register");
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'mm_duplicate_source',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'legacy_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'legacy_v2',
                     'configuration_hash', 'fedcba9876543210',
                     'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
                 )
             ),
             NULL, 5, 60, 20, false
         )",
        "22023",
        "multi-model branches must use distinct source columns",
        "duplicate source column hidden by degraded lifecycle",
    );
}

#[pg_test]
fn multi_model_request_validation_precedes_catalog_and_candidate_work() {
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'does_not_exist',
             (
                 SELECT jsonb_agg(jsonb_build_object('surprise', branch_number))
                   FROM pg_catalog.generate_series(1, 128) AS branch_number
             ),
             NULL, 5, 60, 10000, true
         )",
        "22023",
        "branches must contain between 1 and 127 branches",
        "branch count before per-branch parsing",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'does_not_exist',
             (
                 SELECT jsonb_agg(jsonb_build_object(
                     'profile', pg_catalog.format('profile_%s', branch_number),
                     'configuration_hash', '0123456789abcdef',
                     'query', pg_catalog.repeat('x', 524288),
                     'limit', 1,
                     'weight', 1.0,
                     'surprise', true
                 ))
                   FROM pg_catalog.generate_series(1, 33) AS branch_number
             ),
             NULL, 1, 60, 66, true
         )",
        "54000",
        "multi_profile_query_bytes budget exceeded: 17301504 > 16777216",
        "aggregate query bytes before per-branch parsing",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'does_not_exist',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'legacy_v1',
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1,0]', 'limit', 5, 'weight', 1.0,
                 'surprise', true
             )),
             NULL, 5, 60, 6, true
         )",
        "22023",
        "unsupported multi-model branch field",
        "unknown branch key",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'does_not_exist',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'legacy_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,0]', 'limit', 5, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'modern_v2',
                     'configuration_hash', 'fedcba9876543210',
                     'query', '[1,0]', 'limit', 5, 'weight', 1.0
                 )
             ),
             NULL, 5, 60, 10, true
         )",
        "54000",
        "multi_profile_candidates budget exceeded: 12 > 10",
        "candidate budget preflight",
    );
}

#[pg_test]
fn multi_model_query_rejects_hash_and_dimension_drift_before_candidate_execution() {
    multi_model_corpus("mm_query_drift");
    register_versioned_profile(
        "mm_query_drift",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );

    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'mm_query_drift',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'legacy_v1',
                 'configuration_hash', 'fedcba9876543210',
                 'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
             )),
             NULL, 5, 60, 6, true
         )",
        "55000",
        "multi-model query requires unavailable profile legacy_v1: configuration_changed",
        "configuration hash drift",
    );
    assert_multi_model_sqlstate(
        "SELECT pgcontext.query_multi_model(
             'mm_query_drift',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'legacy_v1',
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1,1]', 'limit', 5, 'weight', 1.0
             )),
             NULL, 5, 60, 6, true
         )",
        "22023",
        "profile query dimension mismatch",
    );
}

#[pg_test]
fn multi_model_authoritative_recheck_excludes_source_edits_and_deleted_points() {
    multi_model_corpus("mm_current");
    register_versioned_profile(
        "mm_current",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    register_versioned_profile(
        "mm_current",
        "modern_v2",
        "modern",
        "modern_version",
        8,
        "active",
    );

    Spi::run(
        "UPDATE public.mm_current SET source_version = 2 WHERE id = 1;
         SELECT pgcontext.delete_points('mm_current', ARRAY['2'])",
    )
    .expect("source edit and point deletion should succeed");
    let report = Spi::get_one::<JsonB>(
        "SELECT pgcontext.query_multi_model(
             'mm_current',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'legacy_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,1,0,0]', 'limit', 10, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'modern_v2',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[2,1,0,0,0,0,0,0]', 'limit', 10, 'weight', 1.0
                 )
             ),
             NULL, 10, 60, 22, true
         )",
    )
    .expect("current-row query should execute")
    .expect("current-row query should return a report");
    let source_keys = report.0["results"]
        .as_array()
        .expect("result array")
        .iter()
        .filter_map(|row| row["source_key"].as_str())
        .collect::<Vec<_>>();
    assert!(!source_keys.contains(&"1"));
    assert!(!source_keys.contains(&"2"));
}

#[pg_test]
fn multi_model_index_drift_is_fail_closed_or_explicitly_degraded() {
    multi_model_corpus("mm_index_drift");
    register_versioned_profile(
        "mm_index_drift",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    register_versioned_profile(
        "mm_index_drift",
        "modern_v2",
        "modern",
        "modern_version",
        8,
        "active",
    );
    Spi::run("DROP INDEX public.mm_index_drift_modern_hnsw")
        .expect("modern index should be removed");
    let branches = "jsonb_build_array(
        jsonb_build_object(
            'profile', 'legacy_v1', 'configuration_hash', '0123456789abcdef',
            'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
        ),
        jsonb_build_object(
            'profile', 'modern_v2', 'configuration_hash', '0123456789abcdef',
            'query', '[2,1,0,0,0,0,0,0]', 'limit', 5, 'weight', 1.0
        )
    )";
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.query_multi_model(
                 'mm_index_drift', {branches}, NULL, 5, 60, 12, true
             )"
        ),
        "55000",
        "multi-model query requires unavailable profile modern_v2: not_ready",
        "required missing index",
    );
    let report = Spi::get_one::<JsonB>(&format!(
        "SELECT pgcontext.query_multi_model(
             'mm_index_drift', {branches}, NULL, 5, 60, 12, false
         )"
    ))
    .expect("degraded index query should execute")
    .expect("degraded index query should return a report");
    assert_eq!(report.0["completion"], "degraded");
    assert_eq!(report.0["missing_profiles"][0]["profile"], "modern_v2");
    assert_eq!(report.0["missing_profiles"][0]["reason"], "not_ready");
}

#[pg_test]
fn multi_model_query_fuses_dense_and_provider_native_integer_profiles() {
    Spi::run(
        "CREATE TABLE public.mm_provider_native (
             id bigint PRIMARY KEY,
             source_version bigint NOT NULL,
             dense_version bigint NOT NULL,
             integer_version bigint NOT NULL,
             dense pgcontext.vector(2) NOT NULL,
             integer_embedding pgcontext.int8vec(2) NOT NULL
         );
         INSERT INTO public.mm_provider_native VALUES
             (1, 1, 1, 1, '[1,0]', pgcontext.int8vec('[1,0]')),
             (2, 1, 1, 1, '[2,0]', pgcontext.int8vec('[2,0]')),
             (3, 1, 1, 1, '[3,0]', pgcontext.int8vec('[3,0]'));
         SELECT pgcontext.create_collection('mm_provider_native', 'public.mm_provider_native');
         SELECT pgcontext.register_vector('mm_provider_native', 'dense', 'dense', 2, 'l2');
         SELECT pgcontext.backfill_points('mm_provider_native', 10);
         CREATE INDEX mm_provider_native_dense_hnsw ON public.mm_provider_native
             USING pgcontext_hnsw (dense pgcontext.vector_hnsw_ops);
         CREATE INDEX mm_provider_native_integer_hnsw ON public.mm_provider_native
             USING pgcontext_hnsw (integer_embedding pgcontext.int8vec_hnsw_ops);",
    )
    .expect("provider-native corpus should be created");
    register_versioned_profile(
        "mm_provider_native",
        "dense_v1",
        "dense",
        "dense_version",
        2,
        "active",
    );
    Spi::run(
        "SELECT pgcontext.register_embedding_profile(
             'mm_provider_native', 'integer_v1', 'integer_embedding',
             'public.mm_provider_native_integer_hnsw',
             jsonb_build_object(
                 'representation', 'int8', 'dimensions', 2,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'fixture', 'model', 'integer', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', 'fedcba9876543210',
                 'source_version_column', 'source_version',
                 'embedding_version_column', 'integer_version'
             )
         )",
    )
    .expect("provider-native profile should register");

    crate::multi_model::force_next_exact_profile_for_test("integer_v1");

    let report = Spi::get_one::<JsonB>(
        "SELECT pgcontext.query_multi_model(
             'mm_provider_native',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'dense_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,0]', 'limit', 3, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'integer_v1',
                     'configuration_hash', 'fedcba9876543210',
                     'query', '[1,0]', 'limit', 3, 'weight', 1.0
                 )
             ),
             NULL, 3, 60, 8, true
         )",
    )
    .expect("provider-native multi-model query should execute")
    .expect("provider-native query should return a report");
    assert_eq!(report.0["completion"], "complete");
    assert_eq!(report.0["results"][0]["point_id"], 1);
    assert_eq!(
        report.0["results"][0]["contributions"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let integer_branch = report.0["branches"]
        .as_array()
        .and_then(|branches| {
            branches
                .iter()
                .find(|branch| branch["profile"] == "integer_v1")
        })
        .expect("integer branch should be reported");
    assert_eq!(
        integer_branch["strategy"],
        "exact_fallback_with_authoritative_recheck"
    );
    let integer_contribution = report.0["results"][0]["contributions"]
        .as_array()
        .and_then(|contributions| {
            contributions
                .iter()
                .find(|contribution| contribution["profile"] == "integer_v1")
        })
        .expect("integer exact contribution should be reported");
    assert_eq!(integer_contribution["source_kind"], "exact");
    assert_eq!(
        integer_contribution["source_authority"],
        "provider_native"
    );
}

#[pg_test]
fn multi_model_candidate_source_rejects_oversized_keys_before_materialization() {
    Spi::run(
        "CREATE TABLE public.mm_oversized_key (
             id text PRIMARY KEY,
             source_version bigint NOT NULL,
             embedding_version bigint NOT NULL,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO public.mm_oversized_key VALUES
             ('valid', 1, 1, '[10,0]'),
             (pg_catalog.repeat('OVERSIZED_SENTINEL', 100), 1, 1, '[1,0]');
         SELECT pgcontext.create_collection(
             'mm_oversized_key', 'public.mm_oversized_key'
         );
         SELECT pgcontext.register_vector(
             'mm_oversized_key', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.upsert_points('mm_oversized_key', ARRAY['valid']);
         CREATE INDEX mm_oversized_key_embedding_hnsw ON public.mm_oversized_key
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);",
    )
    .expect("oversized-key source fixture should be created");
    register_versioned_profile(
        "mm_oversized_key",
        "oversized_v1",
        "embedding",
        "embedding_version",
        2,
        "active",
    );

    assert_sql_failure_exact_without(
        "SELECT pgcontext.query_multi_model(
             'mm_oversized_key',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'oversized_v1',
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1,0]', 'limit', 1, 'weight', 1.0
             )),
             NULL, 1, 60, 2, true
         )",
        "54000",
        "multi_profile_source_key_bytes budget exceeded: 1800 > 1024",
        "OVERSIZED_SENTINEL",
        "oversized multi-profile source identity",
    );
}

#[pg_test]
fn multi_model_query_serves_partitioned_sources_through_bounded_profile_branches() {
    Spi::run(
        "CREATE TABLE public.mm_partitioned (
             id bigint PRIMARY KEY,
             source_version bigint NOT NULL,
             legacy_version bigint NOT NULL,
             modern_version bigint NOT NULL,
             legacy pgcontext.vector(2) NOT NULL,
             modern pgcontext.vector(3) NOT NULL
         ) PARTITION BY RANGE (id);
         CREATE TABLE public.mm_partitioned_low PARTITION OF public.mm_partitioned
             FOR VALUES FROM (1) TO (11);
         CREATE TABLE public.mm_partitioned_high PARTITION OF public.mm_partitioned
             FOR VALUES FROM (11) TO (21);
         INSERT INTO public.mm_partitioned
         SELECT id, 1, 1, 1,
                ARRAY[id::real, 0]::real[]::pgcontext.vector,
                ARRAY[(21 - id)::real, 0, 0]::real[]::pgcontext.vector
           FROM pg_catalog.generate_series(1, 20) AS id;
         SELECT pgcontext.create_collection('mm_partitioned', 'public.mm_partitioned');
         SELECT pgcontext.register_vector('mm_partitioned', 'legacy', 'legacy', 2, 'l2');
         SELECT pgcontext.backfill_points('mm_partitioned', 100);
         CREATE INDEX mm_partitioned_legacy_hnsw ON public.mm_partitioned
             USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
         CREATE INDEX mm_partitioned_modern_hnsw ON public.mm_partitioned
             USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);",
    )
    .expect("partitioned multi-profile source should be created");
    register_versioned_profile(
        "mm_partitioned",
        "legacy_v1",
        "legacy",
        "legacy_version",
        2,
        "active",
    );
    register_versioned_profile(
        "mm_partitioned",
        "modern_v2",
        "modern",
        "modern_version",
        3,
        "active",
    );

    let query = "SELECT pgcontext.query_multi_model(
             'mm_partitioned',
             jsonb_build_array(
                 jsonb_build_object(
                     'profile', 'legacy_v1',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,0]', 'limit', 10, 'weight', 1.0
                 ),
                 jsonb_build_object(
                     'profile', 'modern_v2',
                     'configuration_hash', '0123456789abcdef',
                     'query', '[1,0,0]', 'limit', 10, 'weight', 1.0
                 )
             ),
             NULL, 10, 60, 22, true
         )";
    let report = Spi::get_one::<JsonB>(query)
    .expect("partitioned multi-profile query should execute")
    .expect("partitioned multi-profile query should return a report");
    let source_keys = report.0["results"]
        .as_array()
        .expect("partitioned result array")
        .iter()
        .filter_map(|row| row["source_key"].as_str())
        .filter_map(|source_key| source_key.parse::<i64>().ok())
        .collect::<Vec<_>>();
    assert!(source_keys.iter().any(|source_key| *source_key <= 10));
    assert!(source_keys.iter().any(|source_key| *source_key >= 11));
    let branches = report.0["branches"]
        .as_array()
        .expect("partitioned branch report array");
    assert!(branches.iter().all(|branch| {
        branch["strategy"] == "hnsw_with_authoritative_recheck"
            && branch["hnsw_visits"].as_u64().unwrap_or(0) > 0
    }));
    let accounted_comparisons = branches
        .iter()
        .map(|branch| {
            branch["hnsw_visits"].as_u64().unwrap_or(0)
                + branch["candidate_count"].as_u64().unwrap_or(0)
                + branch["recheck_count"].as_u64().unwrap_or(0)
        })
        .sum::<u64>();
    let reported_comparisons = report.0["budget_usage"]["comparisons"]
        .as_u64()
        .expect("global comparisons should be reported");
    assert!(
        reported_comparisons >= accounted_comparisons,
        "canonical accounting must include traversal, native scoring, recheck, and fusion"
    );
    let last_child_visits = Spi::get_one::<i64>(
        "SELECT node_reads FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("last child scan work should be readable")
    .expect("last child scan work should be non-null");
    let last_child_visits = u64::try_from(last_child_visits)
        .expect("HNSW child visit count should be nonnegative");
    let last_branch_visits = branches
        .last()
        .and_then(|branch| branch["hnsw_visits"].as_u64())
        .expect("last branch should report HNSW visits");
    assert!(
        last_branch_visits > last_child_visits,
        "a partition branch must account more than only its final child scan"
    );

    let comparison_limit = usize::try_from(reported_comparisons - 1)
        .expect("bounded partition comparison limit should fit usize");
    crate::multi_model::set_next_comparison_limit_for_test(comparison_limit);
    assert_multi_model_sqlstate(
        query,
        "54000",
        "partition child scans share the canonical global comparison budget",
    );
}

#[pg_test]
fn multi_model_profile_provenance_distinguishes_exact_and_hnsw_sources() {
    let point_id = context_core::PointId::new(1);
    let configuration =
        context_core::ConfigurationRevision::new(2).expect("configuration identity");
    let profile = context_core::ProfileId::new(3).expect("profile identity");
    let source_version = context_core::SourceVersion::new(4).expect("source version");

    let exact = crate::retrieval::profile_candidate_provenance(
        point_id,
        configuration,
        profile,
        source_version,
        crate::retrieval::CandidateAdapter::Exact,
        context_core::SourceAuthority::PostgreSqlRow,
    )
    .expect("exact profile provenance");
    assert_eq!(exact.source(), context_query::CandidateSourceKind::Exact);
    assert_eq!(
        exact.authority(),
        context_core::SourceAuthority::PostgreSqlRow
    );

    let hnsw = crate::retrieval::profile_candidate_provenance(
        point_id,
        configuration,
        profile,
        source_version,
        crate::retrieval::CandidateAdapter::Hnsw,
        context_core::SourceAuthority::ProviderNative,
    )
    .expect("HNSW profile provenance");
    assert_eq!(hnsw.source(), context_query::CandidateSourceKind::Hnsw);
    assert_eq!(
        hnsw.authority(),
        context_core::SourceAuthority::DerivedArtifact
    );
    assert_ne!(exact.occurrence_id(), hnsw.occurrence_id());

    let provider_native_exact = crate::retrieval::profile_candidate_provenance(
        point_id,
        configuration,
        profile,
        source_version,
        crate::retrieval::CandidateAdapter::Exact,
        context_core::SourceAuthority::ProviderNative,
    )
    .expect("provider-native exact profile provenance");
    assert_eq!(
        provider_native_exact.authority(),
        context_core::SourceAuthority::ProviderNative
    );
}

#[pg_test]
fn multi_model_attached_index_budget_is_inclusive_and_fail_closed() {
    let maximum = context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES;
    crate::multi_model::validate_attached_index_count(maximum, maximum)
        .expect("the exact attached-index boundary should fit");
    let error = crate::multi_model::validate_attached_index_count(maximum + 1, maximum)
        .expect_err("one attached index beyond the boundary must fail");
    assert!(matches!(
        error,
        context_query::QueryError::WorkBudgetExceeded {
            budget: "multi_profile_attached_indexes",
            actual,
            maximum: reported_maximum,
        } if actual == maximum + 1 && reported_maximum == maximum
    ));
}

#[pg_test]
fn multi_model_memory_and_plan_boundaries_are_inclusive_and_fail_closed() {
    let query_boundary = (context_query::DEFAULT_QUERY_MEMORY_BYTES - 1) / 4;
    assert!(crate::multi_model::validate_request_query_memory(query_boundary).is_ok());
    assert!(matches!(
        crate::multi_model::validate_request_query_memory(query_boundary + 1),
        Err(context_query::QueryError::WorkBudgetExceeded {
            budget: "multi_profile_preparation_memory",
            ..
        })
    ));

    let per_candidate = crate::multi_model::candidate_transient_memory_bytes(1)
        .expect("one candidate projection");
    let candidate_boundary = context_query::DEFAULT_QUERY_MEMORY_BYTES / per_candidate;
    assert!(
        crate::multi_model::candidate_transient_memory_bytes(candidate_boundary)
            .is_ok_and(|bytes| bytes <= context_query::DEFAULT_QUERY_MEMORY_BYTES)
    );
    assert!(
        crate::multi_model::candidate_transient_memory_bytes(candidate_boundary + 1)
            .is_ok_and(|bytes| bytes > context_query::DEFAULT_QUERY_MEMORY_BYTES)
    );

    let plan_boundary = context_query::DEFAULT_QUERY_MEMORY_BYTES / 4_096;
    assert!(
        crate::multi_model::hnsw_plan_json_memory_bytes(plan_boundary - 1)
            .is_ok_and(|bytes| bytes < context_query::DEFAULT_QUERY_MEMORY_BYTES)
    );
    assert_eq!(
        crate::multi_model::hnsw_plan_json_memory_bytes(plan_boundary)
            .expect("exact plan-JSON memory boundary"),
        context_query::DEFAULT_QUERY_MEMORY_BYTES
    );

    assert!(crate::multi_model::validate_hnsw_plan_depth_for_test(256));
    assert!(!crate::multi_model::validate_hnsw_plan_depth_for_test(257));
    assert!(
        crate::multi_model::project_single_index_profile_capacity_for_test(
            context_query::MAX_MULTI_PROFILE_BRANCHES,
        )
        .is_ok_and(|bytes| bytes < context_query::DEFAULT_QUERY_MEMORY_BYTES)
    );
    assert!(
        crate::multi_model::filter_field_admission_memory(
            context_core::policy::MAX_FILTER_NODES,
        )
        .is_ok_and(|bytes| bytes < context_query::DEFAULT_QUERY_MEMORY_BYTES)
    );
}

#[pg_test]
fn multi_model_collection_timeout_cancels_before_profile_candidate_work() {
    multi_model_corpus("mm_timeout");
    register_versioned_profile(
        "mm_timeout",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    Spi::run(
        "SELECT * FROM pgcontext.configure_collection_limits(
             'mm_timeout', true,
             NULL, NULL, NULL, NULL, NULL, NULL, 1, NULL
         )",
    )
    .expect("one-millisecond multi-profile timeout should configure");
    crate::retrieval::delay_next_query_preparation_for_test(20_000);
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'mm_timeout',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'legacy_v1',
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
             )),
             NULL, 5, 60, 6, true
         )",
        "57014",
        "canceling statement due to statement timeout",
        "multi-profile preparation timeout",
    );
}

#[pg_test]
fn multi_model_collection_timeout_remains_armed_through_report_finalization() {
    multi_model_corpus("mm_final_timeout");
    register_versioned_profile(
        "mm_final_timeout",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    Spi::run(
        "SELECT * FROM pgcontext.configure_collection_limits(
             'mm_final_timeout', true,
             NULL, NULL, NULL, NULL, NULL, NULL, 1, NULL
         )",
    )
    .expect("one-millisecond finalization timeout should configure");
    crate::retrieval::delay_next_query_finalization_for_test(20_000);
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'mm_final_timeout',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'legacy_v1',
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
             )),
             NULL, 5, 60, 6, true
         )",
        "57014",
        "canceling statement due to statement timeout",
        "multi-profile finalization timeout",
    );
}

#[pg_test]
fn multi_model_filter_preflight_rejects_oversized_scalars_before_catalog_work() {
    let fixed_scalar_bytes = "must".len()
        + "key".len()
        + "tenant".len()
        + "match".len()
        + "value".len();
    let boundary_value = "x".repeat(context_query::MAX_FILTER_SCALAR_BYTES - fixed_scalar_bytes);
    let boundary = json!({
        "must": [{"key": "tenant", "match": {"value": boundary_value}}]
    });
    context_query::validate_filter_json_value(&boundary)
        .expect("the exact filter scalar-byte boundary should pass");
    let over = json!({
        "must": [{"key": "tenant", "match": {
            "value": "x".repeat(context_query::MAX_FILTER_SCALAR_BYTES - fixed_scalar_bytes + 1)
        }}]
    });
    assert!(matches!(
        context_query::validate_filter_json_value(&over),
        Err(context_query::QueryError::InvalidInput {
            field: "filter",
            ..
        })
    ));

    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'collection_that_must_not_be_resolved',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'p', 'configuration_hash', '0123456789abcdef',
                 'query', '[1]', 'limit', 1, 'weight', 1.0
             )),
             jsonb_build_object(
                 'must', jsonb_build_array(jsonb_build_object(
                     'key', 'tenant', 'match', jsonb_build_object(
                         'value', pg_catalog.repeat('FILTER_SENTINEL', 5000)
                     )
                 ))
             ),
             1, 60, 1, true
         )",
        "22023",
        "invalid filter: scalar bytes exceed policy maximum",
        "multi-profile filter preflight before catalog resolution",
    );
}

#[pg_test]
fn multi_model_branch_preflight_bounds_profile_and_unknown_field_errors() {
    assert_sql_failure_exact_without(
        "SELECT pgcontext.query_multi_model(
             'collection_that_must_not_be_resolved',
             jsonb_build_array(jsonb_build_object(
                 'profile', pg_catalog.repeat('PROFILE_SENTINEL', 10000),
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1]', 'limit', 1, 'weight', 1.0
             )), NULL, 1, 60, 1, true
         )",
        "22023",
        "invalid profile name: exceeds 128 bytes",
        "PROFILE_SENTINEL",
        "oversized profile preflight",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.query_multi_model(
             'collection_that_must_not_be_resolved',
             jsonb_build_array(jsonb_build_object(
                 'profile', pg_catalog.repeat(' ', 10000),
                 'configuration_hash', '0123456789abcdef',
                 'query', '[1]', 'limit', 1, 'weight', 1.0
             )), NULL, 1, 60, 1, true
         )",
        "22023",
        "invalid profile name: exceeds 128 bytes",
        "oversized blank profile preflight",
    );
    assert_sql_failure_exact_without(
        "SELECT pgcontext.query_multi_model(
             'collection_that_must_not_be_resolved',
             jsonb_build_array(jsonb_build_object(
                 'profile', 'p', 'configuration_hash', '0123456789abcdef',
                 'query', '[1]', 'limit', 1, 'weight', 1.0,
                 pg_catalog.repeat('UNKNOWN_SENTINEL', 10000), true
             )), NULL, 1, 60, 1, true
         )",
        "22023",
        "unsupported multi-model branch field",
        "UNKNOWN_SENTINEL",
        "oversized unknown branch field preflight",
    );
}

#[pg_test]
fn multi_model_filter_catalog_loads_only_bounded_referenced_keys() {
    multi_model_corpus("mm_filter_catalog");
    Spi::run(
        "ALTER TABLE public.mm_filter_catalog
             ADD COLUMN metadata jsonb NOT NULL DEFAULT '{\"tenant_1\":\"acme\"}'::jsonb;
         SELECT pgcontext.register_jsonb_path(
             'mm_filter_catalog', 'path_boundary', 'metadata',
             ARRAY[pg_catalog.repeat('x', 8192)]
         );
         SELECT pgcontext.register_jsonb_path(
             'mm_filter_catalog', 'bulk_' || key_id::text, 'metadata',
             ARRAY['tenant_' || key_id::text]
         )
           FROM pg_catalog.generate_series(1, 300) AS key_id;",
    )
    .expect("large unrelated filter catalog should register");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_jsonb_path(
             'mm_filter_catalog', 'path_over', 'metadata',
             ARRAY[pg_catalog.repeat('x', 8193)]
         )",
        "22023",
        "JSONB path bytes exceeded: 8193 > 8192",
        "JSONB path registration byte boundary",
    );
    register_versioned_profile(
        "mm_filter_catalog",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    let branches = "jsonb_build_array(jsonb_build_object(
        'profile', 'legacy_v1', 'configuration_hash', '0123456789abcdef',
        'query', '[1,1,0,0]', 'limit', 5, 'weight', 1.0
    ))";
    Spi::get_one::<JsonB>(&format!(
        "SELECT pgcontext.query_multi_model(
             'mm_filter_catalog', {branches}, NULL, 5, 60, 6, true
         )"
    ))
    .expect("no-filter query should skip the oversized filter catalog")
    .expect("no-filter report");
    assert_eq!(crate::multi_model::last_filter_field_count_for_test(), 0);

    Spi::get_one::<JsonB>(&format!(
        "SELECT pgcontext.query_multi_model(
             'mm_filter_catalog', {branches},
             '{{\"must\":[{{\"key\":\"bulk_1\",\"match\":\"acme\"}}]}}'::jsonb,
             5, 60, 6, true
         )"
    ))
    .expect("filtered query should load only its referenced catalog key")
    .expect("filtered report");
    assert_eq!(crate::multi_model::last_filter_field_count_for_test(), 1);

    Spi::run(
        "UPDATE pgcontext._collection_payload_columns
            SET jsonb_path = ARRAY[pg_catalog.repeat('PATH_SENTINEL', 631)]
          WHERE filter_key = 'bulk_1'",
    )
    .expect("test should inject an oversized stored path drift");
    assert_sql_failure_exact_without(
        &format!(
            "SELECT pgcontext.query_multi_model(
                 'mm_filter_catalog', {branches},
                 '{{\"must\":[{{\"key\":\"bulk_1\",\"match\":\"acme\"}}]}}'::jsonb,
                 5, 60, 6, true
             )"
        ),
        "54000",
        "multi_profile_filter_path_bytes budget exceeded: 8203 > 8192",
        "PATH_SENTINEL",
        "oversized stored filter path preflight",
    );
    Spi::run(
        "UPDATE pgcontext._collection_payload_columns
            SET jsonb_path = ARRAY(
                SELECT 'x' FROM pg_catalog.generate_series(1, 17)
            )
          WHERE filter_key = 'bulk_1'",
    )
    .expect("test should inject an over-depth stored path drift");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.query_multi_model(
                 'mm_filter_catalog', {branches},
                 '{{\"must\":[{{\"key\":\"bulk_1\",\"match\":\"acme\"}}]}}'::jsonb,
                 5, 60, 6, true
             )"
        ),
        "54000",
        "multi_profile_filter_path_depth budget exceeded: 17 > 16",
        "over-depth stored filter path preflight",
    );
}

#[pg_test]
fn multi_model_coverage_fails_closed_when_the_registered_source_drifts() {
    multi_model_corpus("mm_coverage_drift");
    register_multi_model_profile("mm_coverage_drift", "legacy_v1", "legacy", 4, "active");
    Spi::run("ALTER TABLE public.mm_coverage_drift RENAME TO mm_coverage_drift_moved")
        .expect("source relation should be renamed");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_coverage_drift')",
        "42P01",
        "relation \"public.mm_coverage_drift\" does not exist",
        "coverage read after source relation drift",
    );

    Spi::run(
        "CREATE TABLE public.mm_coverage_drift (
             id bigint PRIMARY KEY,
             tenant text NOT NULL,
             source_version bigint NOT NULL,
             legacy_version bigint,
             modern_version bigint,
             legacy pgcontext.vector(4) NOT NULL,
             modern pgcontext.vector(8)
         )",
    )
    .expect("replacement relation should be created at the registered name");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_coverage_drift')",
        "XX001",
        "embedding profile source relation identity changed: legacy_v1",
        "coverage read after source OID replacement",
    );
}

#[pg_test]
fn multi_model_coverage_rejects_same_attnum_type_drift() {
    multi_model_corpus("mm_coverage_type_drift");
    register_versioned_profile(
        "mm_coverage_type_drift",
        "legacy_v1",
        "legacy",
        "legacy_version",
        4,
        "active",
    );
    Spi::run(
        "DROP INDEX public.mm_coverage_type_drift_legacy_hnsw;
         ALTER TABLE public.mm_coverage_type_drift
             ALTER COLUMN legacy TYPE text USING legacy::text",
    )
    .expect("vector type drift should retain the registered attnum");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_coverage_type_drift')",
        "XX001",
        "embedding profile source column identity changed: legacy_v1",
        "coverage vector type drift",
    );

    Spi::run(
        "ALTER TABLE public.mm_coverage_type_drift
             ALTER COLUMN legacy TYPE pgcontext.vector(4) USING legacy::pgcontext.vector;
         ALTER TABLE public.mm_coverage_type_drift
             ALTER COLUMN legacy_version TYPE text USING legacy_version::text",
    )
    .expect("version type drift should retain the registered attnum");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_coverage_type_drift')",
        "XX001",
        "embedding profile source column identity changed: legacy_v1",
        "coverage version type drift",
    );
}

#[pg_test]
fn multi_model_coverage_requires_current_source_select() {
    sql_test_create_role("mm_coverage_source_owner");
    sql_test_create_role("mm_coverage_collection_owner");
    sql_test_grant_api_access("mm_coverage_source_owner");
    sql_test_grant_api_access("mm_coverage_collection_owner");

    sql_test_set_session_user("mm_coverage_source_owner");
    Spi::run(
        "CREATE TABLE public.mm_coverage_acl_source (
             id bigint PRIMARY KEY,
             source_version bigint NOT NULL,
             legacy_version bigint NOT NULL,
             legacy pgcontext.vector(2) NOT NULL
         );
         INSERT INTO public.mm_coverage_acl_source VALUES (1, 1, 1, '[1,0]');
         CREATE INDEX mm_coverage_acl_source_legacy_hnsw
             ON public.mm_coverage_acl_source
             USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops)",
    )
    .expect("source owner should create the ACL fixture");
    sql_test_reset_session_user();

    Spi::run(
        "GRANT SELECT ON public.mm_coverage_acl_source TO mm_coverage_collection_owner",
    )
    .expect("collection owner should receive temporary source SELECT");
    sql_test_set_session_user("mm_coverage_collection_owner");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'mm_coverage_acl', 'public.mm_coverage_acl_source'
         );
         SELECT pgcontext.register_vector(
             'mm_coverage_acl', 'legacy', 'legacy', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('mm_coverage_acl', 10);
         SELECT pgcontext.register_embedding_profile(
             'mm_coverage_acl', 'legacy_v1', 'legacy',
             'public.mm_coverage_acl_source_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 2,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'fixture', 'model', 'legacy', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'source_version_column', 'source_version',
                 'embedding_version_column', 'legacy_version'
             ),
             'active'
         )",
    )
    .expect("collection owner should register while source SELECT is granted");
    sql_test_reset_session_user();

    Spi::run(
        "REVOKE SELECT ON public.mm_coverage_acl_source FROM mm_coverage_collection_owner",
    )
    .expect("source SELECT should be revoked");
    sql_test_set_session_user("mm_coverage_collection_owner");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_coverage_acl')",
        "42501",
        "permission denied for source table of embedding profile legacy_v1",
        "coverage after source SELECT revocation",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn multi_model_lifecycle_requires_collection_ownership() {
    multi_model_corpus("mm_acl");
    register_multi_model_profile("mm_acl", "legacy_v1", "legacy", 4, "active");
    sql_test_create_role("mm_outsider");
    sql_test_grant_api_access("mm_outsider");
    sql_test_set_session_user("mm_outsider");
    shared_assert_sql_failure(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_acl', 'legacy_v1', 'draining')",
        "42501",
        "permission denied for collection mm_acl",
        "non-owner lifecycle transition",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.embedding_profile_coverage('mm_acl')",
        "42501",
        "permission denied for collection mm_acl",
        "non-owner coverage read",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn multi_model_lifecycle_rejects_an_unregistered_profile() {
    multi_model_corpus("mm_missing");
    shared_assert_sql_failure(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_missing', 'absent', 'active')",
        "42704",
        "embedding profile is not registered: absent",
        "unregistered profile transition",
    );
}

#[pg_test]
fn multi_model_profile_contract_columns_stay_immutable() {
    // The lifecycle column is now updatable, so the guarantee that every other
    // profile column is frozen needs its own regression.
    multi_model_corpus("mm_immutable");
    register_multi_model_profile("mm_immutable", "legacy_v1", "legacy", 4, "active");

    // Cover the identity columns the narrowing put at risk, not just a few
    // descriptive ones: moving a row between collections or renaming it would
    // silently reinterpret already-stored vectors.
    for column in [
        "dimensions = 8",
        "metric = 'cosine'",
        "model = 'other'",
        "profile_name = 'stolen'",
        "collection_id = collection_id + 1",
        "embedding_profile_id = embedding_profile_id + 1000",
        "source_column_name = 'modern'",
        "configuration_hash = 'fedcba9876543210'",
        "created_at = pg_catalog.now() + interval '1 day'",
        "matryoshka_prefixes = ARRAY[2]",
    ] {
        assert_dml_failure(
            &format!(
                "UPDATE pgcontext._embedding_profiles SET {column}
                  WHERE profile_name = 'legacy_v1'"
            ),
            "55000",
            "embedding profiles are immutable; register a new profile revision",
            "profile contract mutation",
        );
    }

    Spi::run(
        "UPDATE pgcontext._embedding_profiles SET lifecycle = 'draining'
          WHERE profile_name = 'legacy_v1'",
    )
    .expect("the lifecycle column must remain updatable");
    assert_eq!(profile_lifecycle("mm_immutable", "legacy_v1"), "draining");

    // The storage layer enforces the transition table too, so a direct catalog
    // write cannot skip backfill or resurrect a terminal state even though the
    // lifecycle column is writable.
    assert_dml_failure(
        "UPDATE pgcontext._embedding_profiles SET lifecycle = 'shadow'
          WHERE profile_name = 'legacy_v1'",
        "22023",
        "embedding profile cannot move from draining to shadow",
        "illegal direct lifecycle write",
    );
    assert_dml_failure(
        "UPDATE pgcontext._embedding_profiles SET lifecycle = 'draining'
          WHERE profile_name = 'legacy_v1'",
        "22023",
        "embedding profile cannot move from draining to draining",
        "no-op direct lifecycle write",
    );
}

#[pg_test]
fn multi_model_retired_profiles_stop_driving_prefix_selection() {
    // Lifecycle only means something if a non-serving profile actually stops
    // being read. Prefix selection is the one serving path that reads profiles.
    multi_model_corpus("mm_retired_prefix");
    Spi::run(
        "SELECT pgcontext.register_embedding_profile(
             'mm_retired_prefix', 'legacy_v1', 'legacy',
             'public.mm_retired_prefix_legacy_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 4,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'legacy_v1', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'matryoshka_prefixes', jsonb_build_array(2)
             ),
             'active'
         )",
    )
    .expect("prefix profile should be registered");

    Spi::run("SET pgcontext.adaptive_prefix_dimensions = 2")
        .expect("adaptive prefix setting should apply");
    let serving = Spi::get_one::<i64>(
        "SELECT count(*) FROM pgcontext.search(
             'mm_retired_prefix', '[1,1,0,0]'::pgcontext.vector, 5
         )",
    )
    .expect("search should succeed")
    .expect("search should return a count");
    assert_eq!(serving, 5);
    assert!(
        observed_strategies("mm_retired_prefix")
            .iter()
            .any(|strategy| strategy == "dense_adaptive_prefix_exhaustive"),
        "an active profile must drive prefix selection"
    );

    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle(
             'mm_retired_prefix', 'legacy_v1', 'draining'
         );
         SELECT pgcontext.set_embedding_profile_lifecycle(
             'mm_retired_prefix', 'legacy_v1', 'retired'
         )",
    )
    .expect("profile should retire");

    let before = observed_strategies("mm_retired_prefix").len();
    let after_retirement = Spi::get_one::<i64>(
        "SELECT count(*) FROM pgcontext.search(
             'mm_retired_prefix', '[1,1,0,0]'::pgcontext.vector, 5
         )",
    )
    .expect("search should succeed")
    .expect("search should return a count");
    assert_eq!(after_retirement, 5);
    let strategies = observed_strategies("mm_retired_prefix");
    assert!(
        strategies
            .iter()
            .skip(before)
            .all(|strategy| !strategy.starts_with("dense_adaptive_prefix")),
        "a retired profile must stop driving prefix selection, saw {strategies:?}"
    );
}
