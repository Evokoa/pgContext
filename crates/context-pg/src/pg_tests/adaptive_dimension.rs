fn adaptive_corpus(collection_name: &str, prefixes: &str) {
    adaptive_corpus_rows(collection_name, prefixes, 400);
}

fn adaptive_corpus_rows(collection_name: &str, prefixes: &str, rows: usize) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding pgcontext.vector(64) NOT NULL
         );
         INSERT INTO public.{collection_name}
         SELECT d.id,
                (SELECT pg_catalog.array_agg((c.v / sqrt(c.sq))::real)::pgcontext.vector
                   FROM (SELECT v, pg_catalog.sum(v * v) OVER () AS sq
                           FROM (SELECT sin(d.id * 0.7 + k * 1.3)::real AS v
                                   FROM pg_catalog.generate_series(1, 64) AS k) AS raw) AS c)
           FROM pg_catalog.generate_series(1, {rows}) AS d(id);
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.register_vector(
             '{collection_name}', 'embedding', 'embedding', 64, 'cosine'
         );
         SELECT pgcontext.backfill_points('{collection_name}', 1000);
         CREATE INDEX {collection_name}_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
         SELECT pgcontext.register_embedding_profile(
             '{collection_name}', 'mrl', 'embedding', 'public.{collection_name}_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 64,
                 'normalization', 'unit_l2', 'metric', 'cosine',
                 'provider', 'acme', 'model', 'mrl-64', 'revision', '1',
                 'input_template', '{{t}}', 'output_template', '{{v}}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'matryoshka_prefixes', {prefixes}
             )
         );"
    ))
    .expect("adaptive-dimension corpus should be created");
}

fn ranked_source_keys(collection_name: &str, prefix_setting: i32) -> Vec<String> {
    Spi::run(&format!(
        "SET pgcontext.adaptive_prefix_dimensions = {prefix_setting}"
    ))
    .expect("adaptive prefix setting should apply");
    Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT point_id, source_key, score
                       FROM pgcontext.search(
                           '{collection_name}',
                           (SELECT embedding FROM public.{collection_name} WHERE id = 7),
                           10
                       )"
                ),
                None,
                &[],
            )
            .expect("adaptive-dimension search should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(2).ok().flatten())
            .collect::<Vec<_>>()
    })
}

/// Returns the serving strategies telemetry captured for a collection.
///
/// This reads the synchronously captured events rather than
/// `query_execution_stats()`, whose asynchronous worker has not necessarily
/// flushed by the time a `#[pg_test]` body inspects it.
fn observed_strategies(collection_name: &str) -> Vec<String> {
    let collection_id = Spi::get_one::<i64>(&format!(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = '{collection_name}'"
    ))
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    crate::query_stats_async::test_events(collection_id)
        .into_iter()
        .map(|event| event.strategy)
        .collect()
}

/// Returns the candidate counts telemetry captured for a collection.
fn observed_candidate_counts(collection_name: &str) -> Vec<u64> {
    let collection_id = Spi::get_one::<i64>(&format!(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = '{collection_name}'"
    ))
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    crate::query_stats_async::test_events(collection_id)
        .into_iter()
        .map(|event| event.candidates)
        .collect()
}

/// Returns adaptive expansion counts captured for a collection.
fn observed_expansion_counts(collection_name: &str) -> Vec<u64> {
    let collection_id = Spi::get_one::<i64>(&format!(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = '{collection_name}'"
    ))
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    crate::query_stats_async::test_events(collection_id)
        .into_iter()
        .map(|event| event.expansions)
        .collect()
}

#[pg_test]
fn adaptive_every_declared_prefix_returns_the_full_vector_exact_answer() {
    adaptive_corpus("adaptive_parity", "jsonb_build_array(8, 16, 32)");

    let full_vector = ranked_source_keys("adaptive_parity", -1);
    assert_eq!(full_vector.len(), 10);

    for prefix in [0, 8, 16, 32] {
        assert_eq!(
            ranked_source_keys("adaptive_parity", prefix),
            full_vector,
            "prefix setting {prefix} must return the full-vector exact answer"
        );
    }
}

#[pg_test]
fn adaptive_prefix_path_is_actually_selected() {
    adaptive_corpus("adaptive_selected", "jsonb_build_array(8)");
    let _ = ranked_source_keys("adaptive_selected", 8);

    // Without oversampling a prefix probe can only reorder a set it already
    // chose, so the recheck could never recover a true top-k point the prefix
    // ranked out. Assert the probe really admits more than the requested limit,
    // otherwise the parity assertions below would only be measuring whether the
    // corpus happens to be easy.
    let candidates = observed_candidate_counts("adaptive_selected");
    assert!(
        candidates.iter().any(|count| *count > 10),
        "a prefix probe must oversample beyond the requested limit, saw {candidates:?}"
    );

    let strategies = observed_strategies("adaptive_selected");
    assert!(
        strategies
            .iter()
            .any(|strategy| strategy == "dense_adaptive_prefix_exhaustive"),
        "a certified prefix must select the adaptive candidate path, saw {strategies:?}"
    );
    let expansions = observed_expansion_counts("adaptive_selected");
    assert!(
        expansions.iter().any(|count| *count > 0),
        "a 400-row corpus must exercise multi-stage widening, saw {expansions:?}"
    );
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'adaptive_selected'",
    )
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert!(
        events.iter().any(|event| {
            event.adaptive_prefix_dimensions == Some(8)
                && event.adaptive_termination.as_deref() == Some("exhaustive")
        }),
        "adaptive telemetry must retain the chosen prefix and termination: {events:?}"
    );
}

#[pg_test]
fn adaptive_small_corpus_completes_in_one_prefix_stage() {
    adaptive_corpus_rows("adaptive_one_stage", "jsonb_build_array(8, 16)", 20);
    let full_vector = ranked_source_keys("adaptive_one_stage", -1);
    assert_eq!(ranked_source_keys("adaptive_one_stage", 8), full_vector);

    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'adaptive_one_stage'",
    )
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    let events = crate::query_stats_async::test_events(collection_id);
    assert!(
        events.iter().any(|event| {
            event.strategy == "dense_adaptive_prefix_exhaustive"
                && event.expansions == 0
                && event.adaptive_prefix_dimensions == Some(8)
                && event.adaptive_termination.as_deref() == Some("exhaustive")
        }),
        "a 20-row corpus must complete in one prefix stage: {events:?}"
    );
}

#[pg_test]
fn adaptive_profile_cutover_and_failed_new_profile_roll_back_serving_policy() {
    adaptive_corpus("adaptive_cutover", "jsonb_build_array(8)");
    Spi::run(
        "SELECT pgcontext.register_embedding_profile(
             'adaptive_cutover', 'next', 'embedding',
             'public.adaptive_cutover_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 64,
                 'normalization', 'unit_l2', 'metric', 'cosine',
                 'provider', 'acme', 'model', 'mrl-64', 'revision', '2',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                 'zero_point', NULL,
                 'configuration_hash', 'fedcba9876543210',
                 'matryoshka_prefixes', jsonb_build_array(16)
             ),
             'shadow'
         )",
    )
    .expect("shadow replacement profile should register");

    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collection_acl
          WHERE collection_name = 'adaptive_cutover'",
    )
    .expect("collection lookup should succeed")
    .expect("collection should exist");
    let latest_prefix = || {
        crate::query_stats_async::test_events(collection_id)
            .last()
            .and_then(|event| event.adaptive_prefix_dimensions)
    };

    let exact = ranked_source_keys("adaptive_cutover", -1);
    assert_eq!(ranked_source_keys("adaptive_cutover", 0), exact);
    assert_eq!(latest_prefix(), Some(8), "shadow must not serve");

    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle(
             'adaptive_cutover', 'mrl', 'draining'
         );
         SELECT pgcontext.set_embedding_profile_lifecycle(
             'adaptive_cutover', 'next', 'active'
         );",
    )
    .expect("replacement profile should cut over after complete coverage");
    assert_eq!(ranked_source_keys("adaptive_cutover", 0), exact);
    assert_eq!(latest_prefix(), Some(16), "active replacement must serve");

    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle(
             'adaptive_cutover', 'next', 'failed'
         )",
    )
    .expect("failed replacement should leave the draining profile available");
    assert_eq!(ranked_source_keys("adaptive_cutover", 0), exact);
    assert_eq!(latest_prefix(), Some(8), "failed replacement must roll back");

    Spi::run(
        "SELECT pgcontext.set_embedding_profile_lifecycle(
             'adaptive_cutover', 'mrl', 'retired'
         )",
    )
    .expect("outgoing profile should retire");
    assert_eq!(ranked_source_keys("adaptive_cutover", 0), exact);
    assert_eq!(
        observed_strategies("adaptive_cutover").last().map(String::as_str),
        Some("dense_exact"),
        "retired and failed profiles must stop prefix serving"
    );
}

#[pg_test]
fn adaptive_prefix_preserves_rls_and_fails_after_source_select_revoke() {
    acl_create_role("adaptive_rls_table_owner");
    acl_create_role("adaptive_rls_collection_owner");
    acl_grant_api_access("adaptive_rls_table_owner");
    acl_grant_api_access("adaptive_rls_collection_owner");

    acl_set_session_user("adaptive_rls_table_owner");
    Spi::run(
        "CREATE TABLE public.adaptive_rls_docs (
             id bigint PRIMARY KEY,
             embedding pgcontext.vector(8) NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.adaptive_rls_docs
         SELECT id,
                ARRAY[id::real, 1, 0, 0, 0, 0, 0, 0]::real[]::pgcontext.vector,
                CASE WHEN id % 2 = 1 THEN 'acme' ELSE 'other' END
           FROM pg_catalog.generate_series(1, 100) AS id;
         CREATE INDEX adaptive_rls_docs_hnsw ON public.adaptive_rls_docs
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_l2_ops);
         ALTER TABLE public.adaptive_rls_docs ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.adaptive_rls_docs FORCE ROW LEVEL SECURITY;
         CREATE POLICY adaptive_rls_tenant ON public.adaptive_rls_docs
             USING (
                 current_setting('pgcontext_adaptive.tenant', true) = 'all'
                 OR tenant = current_setting('pgcontext_adaptive.tenant', true)
             );
         GRANT SELECT ON public.adaptive_rls_docs TO adaptive_rls_collection_owner;",
    )
    .expect("RLS source should be created");
    acl_reset_session_user();

    acl_set_session_user("adaptive_rls_collection_owner");
    Spi::run(
        "SET pgcontext_adaptive.tenant = 'all';
         SELECT pgcontext.create_collection(
             'adaptive_rls_docs', 'public.adaptive_rls_docs'
         );
         SELECT pgcontext.register_vector(
             'adaptive_rls_docs', 'embedding', 'embedding', 8, 'l2'
         );
         SELECT pgcontext.backfill_points('adaptive_rls_docs', 200);
         SELECT pgcontext.register_embedding_profile(
             'adaptive_rls_docs', 'mrl', 'embedding',
             'public.adaptive_rls_docs_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 8,
                 'normalization', 'none', 'metric', 'l2',
                 'provider', 'acme', 'model', 'mrl-8', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL,
                 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'matryoshka_prefixes', jsonb_build_array(4)
             )
         );
         SET pgcontext_adaptive.tenant = 'acme';",
    )
    .expect("restricted collection owner should configure the adaptive source");

    let full = ranked_source_keys("adaptive_rls_docs", -1);
    let prefix = ranked_source_keys("adaptive_rls_docs", 4);
    assert_eq!(prefix, full);
    assert!(
        prefix
            .iter()
            .all(|source_key| source_key.parse::<i64>().expect("numeric source key") % 2 == 1),
        "adaptive prefix escaped the invoker's RLS policy: {prefix:?}"
    );
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.adaptive_rls_docs FROM adaptive_rls_collection_owner")
        .expect("source SELECT should be revoked");
    acl_set_session_user("adaptive_rls_collection_owner");
    acl_expect_insufficient_privilege(
        "SELECT * FROM pgcontext.search(
             'adaptive_rls_docs', '[1,1,0,0,0,0,0,0]'::pgcontext.vector, 5
         )",
        "permission denied for source table: public.adaptive_rls_docs",
    );
    acl_reset_session_user();
}

#[pg_test]
fn adaptive_disabled_setting_reads_the_full_dimensions() {
    adaptive_corpus("adaptive_disabled", "jsonb_build_array(8)");
    let _ = ranked_source_keys("adaptive_disabled", -1);

    let strategies = observed_strategies("adaptive_disabled");
    assert!(
        strategies.iter().any(|strategy| strategy == "dense_exact"),
        "a disabled setting must read the full dimensions, saw {strategies:?}"
    );
    assert!(
        !strategies
            .iter()
            .any(|strategy| strategy.starts_with("dense_adaptive_prefix")),
        "a disabled setting must not select a prefix, saw {strategies:?}"
    );
}

#[pg_test]
fn adaptive_undeclared_pin_falls_back_to_the_full_dimensions() {
    adaptive_corpus("adaptive_pin_rejected", "jsonb_build_array(8, 16)");
    let full_vector = ranked_source_keys("adaptive_pin_rejected", -1);

    // 24 is not certified by the profile, so the pin is rejected rather than
    // silently reading an uncertified cut point.
    assert_eq!(ranked_source_keys("adaptive_pin_rejected", 24), full_vector);

    let strategies = observed_strategies("adaptive_pin_rejected");
    assert!(
        !strategies
            .iter()
            .any(|strategy| strategy.starts_with("dense_adaptive_prefix")),
        "an undeclared pin must not select a prefix, saw {strategies:?}"
    );
}

#[pg_test]
fn adaptive_collection_without_a_certified_policy_is_unchanged() {
    Spi::run(
        "CREATE TABLE public.adaptive_no_policy (
             id bigint PRIMARY KEY,
             embedding pgcontext.vector(8) NOT NULL
         );
         INSERT INTO public.adaptive_no_policy
         SELECT id, ARRAY[id, 1, 0, 0, 0, 0, 0, 0]::real[]::pgcontext.vector
           FROM pg_catalog.generate_series(1, 20) AS id;
         SELECT pgcontext.create_collection('adaptive_no_policy', 'public.adaptive_no_policy');
         SELECT pgcontext.register_vector(
             'adaptive_no_policy', 'embedding', 'embedding', 8, 'l2'
         );
         SELECT pgcontext.backfill_points('adaptive_no_policy', 100);",
    )
    .expect("policy-free corpus should be created");

    Spi::run("SET pgcontext.adaptive_prefix_dimensions = 4")
        .expect("adaptive prefix setting should apply");
    let results = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.search('adaptive_no_policy', '[1,1,0,0,0,0,0,0]'::pgcontext.vector, 5)",
    )
    .expect("policy-free search should succeed")
    .expect("policy-free search should return a count");
    assert_eq!(results, 5);

    let strategies = observed_strategies("adaptive_no_policy");
    assert!(
        !strategies
            .iter()
            .any(|strategy| strategy.starts_with("dense_adaptive_prefix")),
        "an uncertified collection must never read a prefix, saw {strategies:?}"
    );
}

#[pg_test]
fn adaptive_vector_prefix_projects_leading_coordinates_and_rejects_bad_cuts() {
    let prefix = Spi::get_one::<String>(
        "SELECT pgcontext.vector_prefix('[1,2,3,4]'::pgcontext.vector, 2)::text",
    )
    .expect("vector prefix should succeed")
    .expect("vector prefix should return a value");
    assert_eq!(prefix, "[1,2]");

    let identity = Spi::get_one::<String>(
        "SELECT pgcontext.vector_prefix('[1,2,3,4]'::pgcontext.vector, 4)::text",
    )
    .expect("vector prefix should succeed")
    .expect("vector prefix should return a value");
    assert_eq!(identity, "[1,2,3,4]");

    shared_assert_sql_failure(
        "SELECT pgcontext.vector_prefix('[1,2,3,4]'::pgcontext.vector, 0)",
        "22023",
        "vector prefix dimensions must be between 1 and 4: 0",
        "zero vector prefix",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.vector_prefix('[1,2,3,4]'::pgcontext.vector, 5)",
        "22023",
        "vector prefix dimensions must be between 1 and 4: 5",
        "oversized vector prefix",
    );
}

#[pg_test]
fn adaptive_ineligible_profiles_cannot_declare_a_prefix_policy() {
    Spi::run(
        "CREATE TABLE public.adaptive_ineligible (
             id bigint PRIMARY KEY,
             embedding pgcontext.vector(8) NOT NULL
         );
         INSERT INTO public.adaptive_ineligible
         SELECT id, ARRAY[id, 1, 0, 0, 0, 0, 0, 0]::real[]::pgcontext.vector
           FROM pg_catalog.generate_series(1, 5) AS id;
         SELECT pgcontext.create_collection('adaptive_ineligible', 'public.adaptive_ineligible');
         SELECT pgcontext.register_vector(
             'adaptive_ineligible', 'embedding', 'embedding', 8, 'l1'
         );
         CREATE INDEX adaptive_ineligible_hnsw ON public.adaptive_ineligible
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_l1_ops);",
    )
    .expect("ineligible corpus should be created");

    shared_assert_sql_failure(
        "SELECT pgcontext.register_embedding_profile(
             'adaptive_ineligible', 'mrl', 'embedding', 'public.adaptive_ineligible_hnsw',
             jsonb_build_object(
                 'representation', 'dense', 'dimensions', 8,
                 'normalization', 'none', 'metric', 'l1',
                 'provider', 'acme', 'model', 'm', 'revision', '1',
                 'input_template', '{t}', 'output_template', '{v}',
                 'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                 'configuration_hash', '0123456789abcdef',
                 'matryoshka_prefixes', jsonb_build_array(4)
             )
         )",
        "22023",
        "invalid vector: Matryoshka prefixes require the l2, inner_product, or cosine metric",
        "ineligible metric prefix policy",
    );
}
