fn multi_model_corpus(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             legacy pgcontext.vector(4) NOT NULL,
             modern pgcontext.vector(8)
         );
         INSERT INTO public.{collection_name} (id, legacy, modern)
         SELECT id,
                ARRAY[id, 1, 0, 0]::real[]::pgcontext.vector,
                CASE WHEN id % 2 = 0
                     THEN ARRAY[id, 1, 0, 0, 0, 0, 0, 0]::real[]::pgcontext.vector
                END
           FROM pg_catalog.generate_series(1, 20) AS id;
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.register_vector(
             '{collection_name}', 'legacy', 'legacy', 4, 'l2'
         );
         SELECT pgcontext.backfill_points('{collection_name}', 100);
         CREATE INDEX {collection_name}_legacy_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
         CREATE INDEX {collection_name}_modern_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);"
    ))
    .expect("multi-model corpus should be created");
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

    Spi::run("SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'active')")
        .expect("shadow should promote to active");
    Spi::run("SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'failed')")
        .expect("active should be withdrawable");

    shared_assert_sql_failure(
        "SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'active')",
        "22023",
        "invalid vector: embedding profile cannot move from failed to active",
        "failed straight back to active",
    );
    Spi::run("SELECT pgcontext.set_embedding_profile_lifecycle('mm_shadow', 'modern_v2', 'shadow')")
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
            .any(|strategy| strategy == "dense_adaptive_prefix"),
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
            .all(|strategy| strategy != "dense_adaptive_prefix"),
        "a retired profile must stop driving prefix selection, saw {strategies:?}"
    );
}
