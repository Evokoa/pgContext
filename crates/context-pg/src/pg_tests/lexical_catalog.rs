fn lexical_fixture(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             title text,
             body text,
             meta jsonb,
             document tsvector,
             saved_query tsquery
         );
         INSERT INTO public.{collection_name} (id, title, body, meta) VALUES
             (1, 'postgres internals', 'the postgres storage engine', '{{\"tags\": {{\"topic\": \"database\"}}}}'::jsonb),
             (2, 'rust systems', 'rust and postgres together', '{{\"tags\": {{\"topic\": \"language\"}}}}'::jsonb),
             (3, 'unrelated note', 'nothing to see here', '{{\"tags\": {{\"topic\": \"other\"}}}}'::jsonb);
         UPDATE public.{collection_name}
            SET document = pg_catalog.to_tsvector('pg_catalog.simple', body),
                saved_query = pg_catalog.plainto_tsquery('pg_catalog.simple', 'postgres');
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.backfill_points('{collection_name}', 100);"
    ))
    .expect("lexical fixture should be created");
}

#[pg_test]
fn lexical_catalog_keeps_base_tables_private_and_views_membership_filtered() {
    let private_tables = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(class.relname::text ORDER BY class.relname)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'r'
            AND class.relname IN (
                '_collection_lexical_sources',
                '_collection_lexical_fields',
                '_collection_fuzzy_sources'
            )",
    )
    .expect("lexical catalog query should succeed")
    .expect("lexical catalog tables should exist");
    assert_eq!(
        private_tables,
        vec![
            "_collection_fuzzy_sources".to_owned(),
            "_collection_lexical_fields".to_owned(),
            "_collection_lexical_sources".to_owned(),
        ]
    );

    let public_can_read_base = Spi::get_one::<bool>(
        "SELECT bool_or(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'r'
            AND class.relname IN (
                '_collection_lexical_sources',
                '_collection_lexical_fields',
                '_collection_fuzzy_sources'
            )",
    )
    .expect("lexical private-storage query should succeed")
    .expect("lexical private tables should exist");
    assert!(!public_can_read_base);

    let public_can_read_views = Spi::get_one::<bool>(
        "SELECT bool_and(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'v'
            AND class.relname IN (
                '_visible_collection_lexical_sources',
                '_visible_collection_lexical_fields',
                '_visible_collection_fuzzy_sources'
            )",
    )
    .expect("lexical view query should succeed")
    .expect("lexical visibility views should exist");
    assert!(public_can_read_views);

    let barriers = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'v'
            AND class.relname IN (
                '_visible_collection_lexical_sources',
                '_visible_collection_lexical_fields',
                '_visible_collection_fuzzy_sources'
            )
            AND class.reloptions @> ARRAY['security_barrier=true']",
    )
    .expect("security barrier query should succeed")
    .expect("security barrier count should not be null");
    assert_eq!(barriers, 3);
}

#[pg_test]
fn lexical_registration_records_fields_configuration_and_defaults() {
    lexical_fixture("lex_catalog_fields");
    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_fields',
             'article',
             ARRAY['title', 'body'],
             'pg_catalog.simple',
             ARRAY['A', 'D'],
             NULL,
             'ts_rank',
             2
         )",
    )
    .expect("lexical source should be registered");

    let listed = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT source_name, document_mode, text_configuration, ranker, normalization,
                        index_name, registration_revision, status
                   FROM pgcontext.lexical_sources('lex_catalog_fields')",
                Some(1),
                &[],
            )
            .expect("lexical source listing should succeed")
            .first();
        (
            row.get::<String>(1).ok().flatten(),
            row.get::<String>(2).ok().flatten(),
            row.get::<String>(3).ok().flatten(),
            row.get::<String>(4).ok().flatten(),
            row.get::<i32>(5).ok().flatten(),
            row.get::<String>(6).ok().flatten(),
            row.get::<i64>(7).ok().flatten(),
            row.get::<String>(8).ok().flatten(),
        )
    });
    assert_eq!(listed.0.as_deref(), Some("article"));
    assert_eq!(listed.1.as_deref(), Some("fields"));
    assert_eq!(listed.2.as_deref(), Some("pg_catalog.simple"));
    assert_eq!(listed.3.as_deref(), Some("ts_rank"));
    assert_eq!(listed.4, Some(2));
    assert_eq!(listed.5, None);
    assert_eq!(listed.6, Some(1));
    assert_eq!(listed.7.as_deref(), Some("ready"));

    let weights = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(weight ORDER BY field_ordinal)
           FROM pgcontext._visible_collection_lexical_fields",
    )
    .expect("lexical field query should succeed")
    .expect("lexical fields should exist");
    assert_eq!(weights, vec!["A".to_owned(), "D".to_owned()]);
}

#[pg_test]
fn lexical_registration_rejects_unknown_columns_and_configurations() {
    lexical_fixture("lex_catalog_invalid");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_invalid', 'article', ARRAY['missing_column']
         )",
        "42703",
        "lexical field column is missing: missing_column",
        "unknown lexical column",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_invalid', 'article', ARRAY['body'], 'pg_catalog.klingon'
         )",
        "42704",
        "text search configuration does not exist: pg_catalog.klingon",
        "unknown text search configuration",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_invalid', 'Article', ARRAY['body']
         )",
        "22023",
        "must begin with a lowercase letter or underscore",
        "invalid lexical source name",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_invalid', 'article', ARRAY['body'], 'pg_catalog.simple',
             NULL, NULL, 'bm25'
         )",
        "22023",
        "must be ts_rank or ts_rank_cd",
        "unsupported lexical ranker",
    );
}

#[pg_test]
fn lexical_json_paths_require_json_columns() {
    lexical_fixture("lex_catalog_json");
    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_json', 'topic', ARRAY['meta'], 'pg_catalog.simple',
             ARRAY['B'], ARRAY['tags.topic']
         )",
    )
    .expect("JSON path lexical source should be registered");
    let path = Spi::get_one::<Vec<String>>(
        "SELECT json_path FROM pgcontext._visible_collection_lexical_fields",
    )
    .expect("JSON path query should succeed")
    .expect("JSON path should be stored");
    assert_eq!(path, vec!["tags".to_owned(), "topic".to_owned()]);

    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_json', 'invalid', ARRAY['body'], 'pg_catalog.simple',
             NULL, ARRAY['tags.topic']
         )",
        "42804",
        "lexical JSON path requires a json or jsonb column: body",
        "JSON path on a text column",
    );
}

#[pg_test]
fn stored_vector_and_row_tsquery_registration_validate_column_types() {
    lexical_fixture("lex_catalog_stored");
    Spi::run(
        "SELECT pgcontext.register_lexical_document_source(
             'lex_catalog_stored', 'stored', 'document', 'pg_catalog.simple'
         )",
    )
    .expect("stored-vector lexical source should be registered");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_document_source(
             'lex_catalog_stored', 'invalid', 'body', 'pg_catalog.simple'
         )",
        "42703",
        "stored lexical document column is missing or is not tsvector: body",
        "stored vector column type",
    );

    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_stored', 'article', ARRAY['body'], 'pg_catalog.simple'
         );
         SELECT pgcontext.register_lexical_tsquery(
             'lex_catalog_stored', 'article', 'saved', 'saved_query'
         )",
    )
    .expect("row tsquery should be registered");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_tsquery(
             'lex_catalog_stored', 'article', 'invalid', 'body'
         )",
        "42703",
        "registered row tsquery column is missing or is not tsquery: body",
        "row tsquery column type",
    );
}

#[pg_test]
fn lexical_registration_requires_collection_ownership() {
    lexical_fixture("lex_catalog_acl");
    sql_test_create_role("lex_catalog_outsider");
    sql_test_grant_api_access("lex_catalog_outsider");
    sql_test_set_session_user("lex_catalog_outsider");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_acl', 'article', ARRAY['body']
         )",
        "42501",
        "permission denied for collection lex_catalog_acl",
        "non-owner lexical registration",
    );
    let visible = Spi::get_one::<i64>(
        "SELECT count(*) FROM pgcontext._visible_collection_lexical_sources",
    )
    .expect("membership-filtered view query should succeed")
    .expect("membership-filtered view count should not be null");
    assert_eq!(visible, 0);
    sql_test_reset_session_user();
}

#[pg_test]
fn dropping_a_lexical_source_removes_its_field_bindings() {
    lexical_fixture("lex_catalog_drop");
    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_drop', 'article', ARRAY['title', 'body'], 'pg_catalog.simple'
         )",
    )
    .expect("lexical source should be registered");
    let dropped =
        Spi::get_one::<bool>("SELECT pgcontext.drop_lexical_source('lex_catalog_drop', 'article')")
            .expect("drop should succeed")
            .expect("drop should return a value");
    assert!(dropped);
    let remaining = Spi::get_one::<i64>(
        "SELECT count(*) FROM pgcontext._visible_collection_lexical_fields",
    )
    .expect("field query should succeed")
    .expect("field count should not be null");
    assert_eq!(remaining, 0);
}

#[pg_test]
fn refreshing_the_lexical_catalog_rebinds_oids_after_a_table_rewrite() {
    lexical_fixture("lex_catalog_refresh");
    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_catalog_refresh', 'article', ARRAY['body'], 'pg_catalog.simple'
         )",
    )
    .expect("lexical source should be registered");
    let original = Spi::get_one::<pg_sys::Oid>(
        "SELECT source_table_oid FROM pgcontext._visible_collection_lexical_sources",
    )
    .expect("original oid query should succeed")
    .expect("original oid should exist");

    Spi::run(
        "ALTER TABLE public.lex_catalog_refresh SET (fillfactor = 90);
         VACUUM FULL public.lex_catalog_refresh;",
    )
    .expect("source table should be rewritten");
    Spi::run("SELECT pgcontext.refresh_lexical_catalog('lex_catalog_refresh')")
        .expect("catalog refresh should succeed");

    let refreshed = Spi::get_one::<bool>(
        "SELECT sources.source_table_oid = pg_catalog.to_regclass('public.lex_catalog_refresh')
           FROM pgcontext._visible_collection_lexical_sources AS sources",
    )
    .expect("refreshed oid query should succeed")
    .expect("refreshed oid should exist");
    assert!(refreshed);
    let _ = original;
}
