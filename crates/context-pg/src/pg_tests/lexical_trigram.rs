fn pg_trgm_is_available() -> bool {
    Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1 FROM pg_catalog.pg_available_extensions WHERE name = 'pg_trgm'
         )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

fn fuzzy_corpus(collection_name: &str, trgm_schema: &str) {
    Spi::run(&format!(
        "CREATE SCHEMA IF NOT EXISTS {trgm_schema};
         CREATE EXTENSION IF NOT EXISTS pg_trgm SCHEMA {trgm_schema};
         CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             body text NOT NULL
         );
         INSERT INTO public.{collection_name} (id, body) VALUES
             (1, 'postgres'),
             (2, 'postgresql database'),
             (3, 'completely different'),
             (4, 'postgrs');
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.backfill_points('{collection_name}', 100);
         SELECT pgcontext.register_fuzzy_source('{collection_name}', 'body_trgm', 'body');"
    ))
    .expect("fuzzy corpus should be created");
}

fn fuzzy_keys(collection_name: &str, mode: &str, threshold: &str, text: &str) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT point_id, source_key, score
                       FROM pgcontext.execute_query(
                           '{collection_name}',
                           pgcontext.query_fuzzy(
                               'body_trgm', '{text}', '{mode}', {threshold}, NULL, 10
                           )
                       )"
                ),
                None,
                &[],
            )
            .expect("fuzzy query should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(2).ok().flatten())
            .collect::<Vec<_>>()
    })
}

#[pg_test]
fn fuzzy_sources_resolve_pg_trgm_through_a_relocated_schema() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_relocated", "trgm_home");

    let schema = Spi::get_one::<String>(
        "SELECT trgm_schema FROM pgcontext.fuzzy_sources('fuzzy_relocated')",
    )
    .expect("fuzzy source listing should succeed")
    .expect("fuzzy source should exist");
    assert_eq!(schema, "trgm_home");

    let mut matched = fuzzy_keys("fuzzy_relocated", "similarity", "0.3", "postgrs");
    matched.sort();
    assert!(matched.contains(&"4".to_owned()));
    assert!(!matched.contains(&"3".to_owned()));
}

#[pg_test]
fn fuzzy_modes_apply_their_native_similarity_function() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_modes", "trgm_modes");

    for mode in ["similarity", "word_similarity", "strict_word_similarity"] {
        let matched = fuzzy_keys("fuzzy_modes", mode, "0.3", "postgres");
        assert!(
            matched.contains(&"1".to_owned()),
            "fuzzy mode {mode} should match the exact token"
        );
        assert!(
            !matched.contains(&"3".to_owned()),
            "fuzzy mode {mode} should not match unrelated text"
        );
    }
}

#[pg_test]
fn fuzzy_thresholds_bound_the_returned_matches() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_threshold", "trgm_threshold");

    let permissive = fuzzy_keys("fuzzy_threshold", "similarity", "0.2", "postgrs");
    let strict = fuzzy_keys("fuzzy_threshold", "similarity", "0.95", "postgrs");
    assert!(permissive.len() >= strict.len());
    assert!(strict.iter().all(|key| permissive.contains(key)));

    shared_assert_sql_failure(
        "SELECT pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.0)",
        "22023",
        "must be a finite value within 0.0 < threshold <= 1.0",
        "zero fuzzy threshold",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.query_fuzzy('body_trgm', 'postgrs', 'levenshtein', 0.3)",
        "22023",
        "must be similarity, word_similarity, or strict_word_similarity",
        "unsupported fuzzy mode",
    );
}

#[pg_test]
fn indexed_fuzzy_probes_restore_the_trigram_threshold_setting() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_guc", "trgm_guc");
    Spi::run("SELECT pgcontext.create_fuzzy_index('fuzzy_guc', 'body_trgm')")
        .expect("fuzzy index creation should succeed");
    Spi::run("SET pg_trgm.similarity_threshold = 0.42")
        .expect("baseline trigram threshold should be set");

    let matched = fuzzy_keys("fuzzy_guc", "similarity", "0.2", "postgrs");
    assert!(!matched.is_empty());

    let restored = Spi::get_one::<String>("SELECT pg_catalog.current_setting('pg_trgm.similarity_threshold')")
        .expect("threshold query should succeed")
        .expect("threshold should exist");
    assert!(
        restored.starts_with("0.42"),
        "trigram threshold leaked: {restored}"
    );
}

#[pg_test]
fn indexed_and_exact_fuzzy_paths_agree() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_parity", "trgm_parity");
    let exact = fuzzy_keys("fuzzy_parity", "similarity", "0.3", "postgrs");
    Spi::run("SELECT pgcontext.create_fuzzy_index('fuzzy_parity', 'body_trgm')")
        .expect("fuzzy index creation should succeed");
    let indexed = fuzzy_keys("fuzzy_parity", "similarity", "0.3", "postgrs");
    assert_eq!(exact, indexed);
}

#[pg_test]
fn fuzzy_registration_requires_collection_ownership() {
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_acl", "trgm_acl");
    sql_test_create_role("fuzzy_outsider");
    sql_test_grant_api_access("fuzzy_outsider");
    sql_test_set_session_user("fuzzy_outsider");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_fuzzy_source('fuzzy_acl', 'other', 'body')",
        "42501",
        "permission denied for collection fuzzy_acl",
        "non-owner fuzzy registration",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn word_similarity_probes_restore_their_own_mode_default() {
    // Regression: `pg_trgm` placeholder GUCs read as NULL until the module is
    // loaded, and every mode has a different documented default. Restoring a
    // single hardcoded value silently widened later `<%` comparisons.
    if !pg_trgm_is_available() {
        return;
    }
    fuzzy_corpus("fuzzy_mode_default", "trgm_mode_default");
    Spi::run("SELECT pgcontext.create_fuzzy_index('fuzzy_mode_default', 'body_trgm')")
        .expect("fuzzy index creation should succeed");

    let _ = fuzzy_keys("fuzzy_mode_default", "word_similarity", "0.9", "postgrs");

    let restored = Spi::get_one::<String>(
        "SELECT pg_catalog.current_setting('pg_trgm.word_similarity_threshold')",
    )
    .expect("threshold query should succeed")
    .expect("threshold should exist");
    let restored = restored
        .parse::<f64>()
        .expect("threshold should parse as a float");
    assert!(
        (restored - 0.6).abs() < 1e-9,
        "word_similarity threshold must be restored to its own default, found {restored}"
    );
}
