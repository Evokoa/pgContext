fn indexed_lexical_corpus(collection_name: &str, rows: i32) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             body text NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.{collection_name} (id, body, tenant)
         SELECT id,
                CASE WHEN id % 3 = 0 THEN 'postgres storage engine ' || id
                     ELSE 'unrelated filler text ' || id
                END,
                CASE WHEN id = {rows} THEN 'selected' ELSE 'other' END
           FROM pg_catalog.generate_series(1, {rows}) AS id;
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.backfill_points('{collection_name}', 10000);
         SELECT pgcontext.register_filter_column('{collection_name}', 'tenant', 'tenant');
         SELECT pgcontext.register_lexical_source(
             '{collection_name}', 'article', ARRAY['body'], 'pg_catalog.simple'
         );"
    ))
    .expect("indexed lexical corpus should be created");
}

#[pg_test]
fn indexed_lexical_filter_is_applied_before_the_bounded_probe() {
    indexed_lexical_corpus("lex_index_filter_probe", 60);
    Spi::run(
        "SELECT pgcontext.create_lexical_index('lex_index_filter_probe', 'article');
         SET pgcontext.lexical_candidate_budget = 1;",
    )
    .expect("indexed filter fixture should be configured");
    let keys = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_index_filter_probe',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'postgres storage'),
                   jsonb_build_object(
                       'must', jsonb_build_array(
                           jsonb_build_object(
                               'key', 'tenant', 'match', jsonb_build_object('value', 'selected')
                           )
                       )
                   ),
                   1
               )
           )",
    );
    Spi::run("RESET pgcontext.lexical_candidate_budget")
        .expect("lexical candidate budget should reset");
    assert_eq!(keys, vec!["60".to_owned()]);
}

fn indexed_lexical_keys(collection_name: &str, limit: i32) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT point_id, source_key, score
                       FROM pgcontext.execute_query(
                           '{collection_name}',
                           pgcontext.query_lexical(
                               'article',
                               jsonb_build_object('form', 'plain', 'text', 'postgres storage'),
                               NULL, {limit}
                           )
                       )"
                ),
                None,
                &[],
            )
            .expect("indexed lexical query should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(2).ok().flatten())
            .collect::<Vec<_>>()
    })
}

#[pg_test]
fn creating_a_lexical_index_attaches_a_valid_gin_index() {
    indexed_lexical_corpus("lex_index_gin", 60);
    let index_name =
        Spi::get_one::<String>("SELECT pgcontext.create_lexical_index('lex_index_gin', 'article')")
            .expect("index creation should succeed")
            .expect("index name should be returned");
    assert!(index_name.starts_with("pgcontext_lexical_"));

    let attached = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT index_name, index_am_name, index_is_lossy, registration_revision
                   FROM pgcontext._visible_collection_lexical_sources
                  WHERE source_name = 'article'",
                Some(1),
                &[],
            )
            .expect("attached index query should succeed")
            .first();
        (
            row.get::<String>(1).ok().flatten(),
            row.get::<String>(2).ok().flatten(),
            row.get::<bool>(3).ok().flatten(),
            row.get::<i64>(4).ok().flatten(),
        )
    });
    assert_eq!(attached.0, Some(index_name.clone()));
    assert_eq!(attached.1.as_deref(), Some("gin"));
    assert_eq!(attached.2, Some(false));
    assert_eq!(attached.3, Some(2));

    let index_is_valid = Spi::get_one::<bool>(&format!(
        "SELECT index.indisvalid AND index.indislive AND index.indpred IS NULL
           FROM pg_catalog.pg_index AS index
           JOIN pg_catalog.pg_class AS index_class ON index_class.oid = index.indexrelid
          WHERE index_class.relname = '{index_name}'"
    ))
    .expect("index validity query should succeed")
    .expect("index validity should be observable");
    assert!(index_is_valid);
}

#[pg_test]
fn the_canonical_index_expression_is_planner_matchable() {
    indexed_lexical_corpus("lex_index_plan", 400);
    let index_name = Spi::get_one::<String>(
        "SELECT pgcontext.create_lexical_index('lex_index_plan', 'article')",
    )
    .expect("index creation should succeed")
    .expect("index name should be returned");
    Spi::run("ANALYZE public.lex_index_plan").expect("statistics should be collected");
    Spi::run("SET LOCAL enable_seqscan = off").expect("sequential scans should be discouraged");

    let plan = Spi::connect(|client| {
        let rows = client
            .select(
                "EXPLAIN (COSTS OFF)
                 SELECT source.id
                   FROM public.lex_index_plan AS source
                  WHERE pg_catalog.setweight(
                            pg_catalog.to_tsvector(
                                '\"pg_catalog\".\"simple\"'::pg_catalog.regconfig,
                                coalesce(source.body::text, ''::text)
                            ), 'D'::\"char\"
                        ) OPERATOR(pg_catalog.@@) pg_catalog.plainto_tsquery(
                            '\"pg_catalog\".\"simple\"'::pg_catalog.regconfig, 'postgres storage'
                        )",
                None,
                &[],
            )
            .expect("explain should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(1).ok().flatten())
            .collect::<Vec<_>>()
            .join("\n")
    });
    assert!(
        plan.contains(&index_name),
        "canonical lexical index expression must be planner-matchable, plan was: {plan}"
    );
}

#[pg_test]
fn indexed_and_exact_lexical_paths_return_the_same_results() {
    indexed_lexical_corpus("lex_index_parity", 60);
    let exact = indexed_lexical_keys("lex_index_parity", 30);
    Spi::run("SELECT pgcontext.create_lexical_index('lex_index_parity', 'article')")
        .expect("index creation should succeed");
    let indexed = indexed_lexical_keys("lex_index_parity", 30);
    assert_eq!(exact, indexed);
    assert!(!exact.is_empty());
}

#[pg_test]
fn gist_lexical_indexes_attach_and_report_lossy_serving() {
    indexed_lexical_corpus("lex_index_gist", 40);
    Spi::run("SELECT pgcontext.create_lexical_index('lex_index_gist', 'article', 'gist')")
        .expect("gist index creation should succeed");
    let lossy = Spi::get_one::<bool>(
        "SELECT index_is_lossy
           FROM pgcontext._visible_collection_lexical_sources
          WHERE source_name = 'article'",
    )
    .expect("lossy flag query should succeed")
    .expect("lossy flag should exist");
    assert!(lossy);

    let results = indexed_lexical_keys("lex_index_gist", 20);
    assert!(!results.is_empty());
}

#[pg_test]
fn dropping_an_attached_index_falls_back_to_the_complete_exact_path() {
    indexed_lexical_corpus("lex_index_dropped", 40);
    let index_name = Spi::get_one::<String>(
        "SELECT pgcontext.create_lexical_index('lex_index_dropped', 'article')",
    )
    .expect("index creation should succeed")
    .expect("index name should be returned");
    let indexed = indexed_lexical_keys("lex_index_dropped", 20);

    Spi::run(&format!("DROP INDEX public.{index_name}")).expect("index should be dropped");
    let fallback = indexed_lexical_keys("lex_index_dropped", 20);
    assert_eq!(indexed, fallback);
}

#[pg_test]
fn a_drifted_index_definition_fails_closed() {
    indexed_lexical_corpus("lex_index_drift", 30);
    let index_name = Spi::get_one::<String>(
        "SELECT pgcontext.create_lexical_index('lex_index_drift', 'article')",
    )
    .expect("index creation should succeed")
    .expect("index name should be returned");
    Spi::run(&format!(
        "UPDATE pgcontext._collection_lexical_sources
            SET index_definition = index_definition || ' -- drifted'
          WHERE index_name = '{index_name}'"
    ))
    .expect("stored index definition should be perturbed");

    let expected = format!(
        "lexical_source_catalog failed: attached lexical index definition drifted: {index_name}"
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.execute_query(
             'lex_index_drift',
             pgcontext.query_lexical(
                 'article', jsonb_build_object('form', 'plain', 'text', 'postgres'), NULL, 5
             )
         )",
        "XX000",
        &expected,
        "drifted lexical index definition",
    );
}

#[pg_test]
fn attaching_a_partial_or_foreign_index_is_rejected() {
    indexed_lexical_corpus("lex_index_reject", 30);
    Spi::run(
        "CREATE INDEX lex_index_reject_partial
             ON public.lex_index_reject
          USING gin ((pg_catalog.to_tsvector('pg_catalog.simple', body)))
          WHERE id > 5",
    )
    .expect("partial index should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.attach_lexical_index(
             'lex_index_reject', 'article', 'lex_index_reject_partial'
         )",
        "0A000",
        "lexical index must not be partial: public.lex_index_reject_partial",
        "partial lexical index",
    );

    Spi::run("CREATE TABLE public.lex_index_reject_other (id bigint, body text)")
        .expect("unrelated table should be created");
    Spi::run(
        "CREATE INDEX lex_index_reject_other_gin
             ON public.lex_index_reject_other
          USING gin ((pg_catalog.to_tsvector('pg_catalog.simple', body)))",
    )
    .expect("unrelated index should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.attach_lexical_index(
             'lex_index_reject', 'article', 'lex_index_reject_other_gin'
         )",
        "42809",
        "index public.lex_index_reject_other_gin is not defined on the registered source relation",
        "foreign lexical index",
    );
}

#[pg_test]
fn attaching_a_same_table_index_for_the_wrong_document_is_rejected() {
    indexed_lexical_corpus("lex_index_wrong_document", 30);
    Spi::run(
        "ALTER TABLE public.lex_index_wrong_document ADD COLUMN other text NOT NULL DEFAULT 'other';
         CREATE INDEX lex_index_wrong_document_gin
             ON public.lex_index_wrong_document
          USING gin ((pg_catalog.to_tsvector('pg_catalog.simple', other)))",
    )
    .expect("same-table index over another document should be created");

    shared_assert_sql_failure(
        "SELECT pgcontext.attach_lexical_index(
             'lex_index_wrong_document', 'article', 'lex_index_wrong_document_gin'
         )",
        "XX000",
        "lexical_source_catalog failed: lexical index does not match the registered document expression: lex_index_wrong_document_gin",
        "same-table lexical index over the wrong document",
    );
}

#[pg_test]
fn detaching_a_lexical_index_restores_exact_serving() {
    indexed_lexical_corpus("lex_index_detach", 30);
    Spi::run("SELECT pgcontext.create_lexical_index('lex_index_detach', 'article')")
        .expect("index creation should succeed");
    Spi::run("SELECT pgcontext.detach_lexical_index('lex_index_detach', 'article')")
        .expect("index detach should succeed");
    let attached = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext._visible_collection_lexical_sources
          WHERE source_name = 'article' AND index_oid IS NOT NULL",
    )
    .expect("attachment query should succeed")
    .expect("attachment count should not be null");
    assert_eq!(attached, 0);
    assert!(!indexed_lexical_keys("lex_index_detach", 10).is_empty());
}

#[pg_test]
fn indexed_weight_restricted_negation_matches_the_exact_path() {
    // Regression: `ts_filter` removes lexemes, and removing a lexeme can make a
    // negated clause become true, so a weight-restricted match is NOT a subset
    // of the unrestricted match. An index probe on the unrestricted document
    // silently dropped this row while still reporting a complete result.
    Spi::run(
        "CREATE TABLE public.lex_index_negation (
             id bigint PRIMARY KEY,
             title text NOT NULL,
             body text NOT NULL
         );
         INSERT INTO public.lex_index_negation VALUES
             (1, 'alpha', 'beta'),
             (2, 'alpha', 'gamma'),
             (3, 'delta', 'beta');
         SELECT pgcontext.create_collection(
             'lex_index_negation', 'public.lex_index_negation'
         );
         SELECT pgcontext.backfill_points('lex_index_negation', 100);
         SELECT pgcontext.register_lexical_source(
             'lex_index_negation', 'article', ARRAY['title', 'body'],
             'pg_catalog.simple', ARRAY['A', 'D']
         );",
    )
    .expect("negation fixture should be created");

    let plan = "pgcontext.query_lexical(
        'article',
        jsonb_build_object(
            'form', 'weight_restricted',
            'weights', jsonb_build_array('a'),
            'query', jsonb_build_object(
                'form', 'boolean', 'operator', 'and', 'clauses',
                jsonb_build_array(
                    jsonb_build_object('form', 'plain', 'text', 'alpha'),
                    jsonb_build_object(
                        'form', 'boolean', 'operator', 'not', 'clauses',
                        jsonb_build_array(
                            jsonb_build_object('form', 'plain', 'text', 'beta')
                        )
                    )
                )
            )
        ),
        NULL, 10
    )";
    let exact = lexical_query_source_keys(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query('lex_index_negation', {plan})"
    ));
    assert_eq!(exact, vec!["1".to_owned(), "2".to_owned()]);

    Spi::run("SELECT pgcontext.create_lexical_index('lex_index_negation', 'article')")
        .expect("index creation should succeed");
    let indexed = lexical_query_source_keys(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query('lex_index_negation', {plan})"
    ));
    assert_eq!(
        indexed, exact,
        "a weight-restricted negation must not lose rows on the indexed path"
    );
}

#[pg_test]
fn an_indexed_probe_past_its_allowance_fails_closed() {
    indexed_lexical_corpus("lex_index_allowance", 60);
    Spi::run("SELECT pgcontext.create_lexical_index('lex_index_allowance', 'article')")
        .expect("index creation should succeed");
    Spi::run("SET pgcontext.lexical_candidate_budget = 1")
        .expect("candidate budget should be lowered");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.execute_query(
             'lex_index_allowance',
             pgcontext.query_lexical(
                 'article',
                 jsonb_build_object('form', 'plain', 'text', 'postgres storage'),
                 NULL, 5
             )
         )",
        "54000",
        "query execution exhausted its work budget",
        "indexed probe past its candidate allowance",
    );
    Spi::run("RESET pgcontext.lexical_candidate_budget")
        .expect("candidate budget should be reset");
}
