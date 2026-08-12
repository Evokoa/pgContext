fn lexical_query_source_keys(sql: &str) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client
            .select(sql, None, &[])
            .expect("lexical query should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(2).ok().flatten())
            .collect::<Vec<_>>()
    })
}

fn lexical_query_scores(sql: &str) -> Vec<(String, f64)> {
    Spi::connect(|client| {
        let rows = client
            .select(sql, None, &[])
            .expect("lexical query should succeed");
        rows.into_iter()
            .filter_map(|row| {
                let source_key = row.get::<String>(2).ok().flatten()?;
                let score = row.get::<f32>(3).ok().flatten()?;
                Some((source_key, f64::from(score)))
            })
            .collect::<Vec<_>>()
    })
}

fn lexical_corpus(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             title text,
             body text,
             tenant text NOT NULL DEFAULT 'a',
             meta jsonb,
             saved_query tsquery
         );
         INSERT INTO public.{collection_name} (id, title, body, tenant, meta) VALUES
             (1, 'postgres storage', 'the postgres storage engine stores pages', 'a',
              '{{\"tags\": {{\"topic\": \"database\"}}}}'::jsonb),
             (2, 'rust systems', 'rust programs call postgres directly', 'b',
              '{{\"tags\": {{\"topic\": \"language\"}}}}'::jsonb),
             (3, 'quiet note', 'nothing relevant lives here', 'a',
              '{{\"tags\": {{\"topic\": \"other\"}}}}'::jsonb),
             (4, 'postgres tuning', 'tuning postgres storage for speed', 'a',
              '{{\"tags\": {{\"topic\": \"database\"}}}}'::jsonb);
         UPDATE public.{collection_name}
            SET saved_query = pg_catalog.plainto_tsquery('pg_catalog.simple', 'storage');
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');
         SELECT pgcontext.backfill_points('{collection_name}', 100);
         SELECT pgcontext.register_filter_column('{collection_name}', 'tenant', 'tenant');
         SELECT pgcontext.register_lexical_source(
             '{collection_name}', 'article', ARRAY['title', 'body'],
             'pg_catalog.simple', ARRAY['A', 'D']
         );"
    ))
    .expect("lexical corpus should be created");
}

#[pg_test]
fn plain_lexical_queries_match_the_direct_postgresql_oracle() {
    lexical_corpus("lex_query_plain");

    let executed = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_plain',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'postgres storage'),
                   NULL, 10
               )
           )",
    );

    let oracle = Spi::connect(|client| {
        let rows = client
            .select(
                "WITH document AS (
                     SELECT source.id::text AS source_key,
                            pg_catalog.setweight(
                                pg_catalog.to_tsvector(
                                    'pg_catalog.simple'::pg_catalog.regconfig,
                                    coalesce(source.title::text, ''::text)
                                ), 'A'
                            ) OPERATOR(pg_catalog.||)
                            pg_catalog.setweight(
                                pg_catalog.to_tsvector(
                                    'pg_catalog.simple'::pg_catalog.regconfig,
                                    coalesce(source.body::text, ''::text)
                                ), 'D'
                            ) AS vector
                       FROM public.lex_query_plain AS source
                 )
                 SELECT document.source_key
                   FROM document
                  WHERE document.vector @@ pg_catalog.plainto_tsquery(
                            'pg_catalog.simple'::pg_catalog.regconfig, 'postgres storage'
                        )
                  ORDER BY pg_catalog.ts_rank_cd(
                               ARRAY[0.1, 0.2, 0.4, 1.0]::real[],
                               document.vector,
                               pg_catalog.plainto_tsquery(
                                   'pg_catalog.simple'::pg_catalog.regconfig, 'postgres storage'
                               ),
                               0
                           ) DESC, document.source_key ASC",
                None,
                &[],
            )
            .expect("direct oracle should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(1).ok().flatten())
            .collect::<Vec<_>>()
    });

    assert_eq!(executed, oracle);
    assert!(!executed.is_empty());
}

#[pg_test]
fn every_lexical_form_matches_its_native_constructor() {
    lexical_corpus("lex_query_forms");
    Spi::run(
        "SELECT pgcontext.register_lexical_tsquery(
             'lex_query_forms', 'article', 'saved', 'saved_query'
         )",
    )
    .expect("row tsquery should be registered");

    let cases: [(&str, &str, Vec<&str>); 8] = [
        (
            "plain",
            "jsonb_build_object('form', 'plain', 'text', 'postgres')",
            vec!["1", "2", "4"],
        ),
        (
            "structured",
            "jsonb_build_object('form', 'structured', 'text', 'postgres & storage')",
            vec!["1", "4"],
        ),
        (
            "phrase",
            "jsonb_build_object('form', 'phrase', 'text', 'postgres storage')",
            vec!["1", "4"],
        ),
        (
            "web_search",
            "jsonb_build_object('form', 'web_search', 'text', 'postgres -rust')",
            vec!["1", "4"],
        ),
        (
            "prefix",
            "jsonb_build_object('form', 'prefix', 'term', 'postgr')",
            vec!["1", "2", "4"],
        ),
        (
            "distance",
            "jsonb_build_object('form', 'distance', 'left', 'postgres', 'right', 'storage', 'distance', 1)",
            vec!["1", "4"],
        ),
        (
            "boolean",
            "jsonb_build_object(
                 'form', 'boolean', 'operator', 'and', 'clauses',
                 jsonb_build_array(
                     jsonb_build_object('form', 'plain', 'text', 'postgres'),
                     jsonb_build_object(
                         'form', 'boolean', 'operator', 'not', 'clauses',
                         jsonb_build_array(jsonb_build_object('form', 'plain', 'text', 'rust'))
                     )
                 )
             )",
            vec!["1", "4"],
        ),
        (
            "registered_tsquery",
            "jsonb_build_object('form', 'registered_tsquery', 'name', 'saved')",
            vec!["1", "4"],
        ),
    ];

    for (label, query, expected) in cases {
        let mut observed = lexical_query_source_keys(&format!(
            "SELECT point_id, source_key, score
               FROM pgcontext.execute_query(
                   'lex_query_forms',
                   pgcontext.query_lexical('article', {query}, NULL, 10)
               )"
        ));
        observed.sort();
        let mut expected = expected
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(observed, expected, "lexical form {label} mismatched");
    }
}

#[pg_test]
fn weight_restriction_limits_matching_to_the_selected_document_weights() {
    lexical_corpus("lex_query_weights");

    let title_only = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_weights',
               pgcontext.query_lexical(
                   'article',
                   jsonb_build_object(
                       'form', 'weight_restricted',
                       'weights', jsonb_build_array('a'),
                       'query', jsonb_build_object('form', 'plain', 'text', 'engine')
                   ),
                   NULL, 10
               )
           )",
    );
    assert!(title_only.is_empty(), "engine only appears in the body");

    let body_included = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_weights',
               pgcontext.query_lexical(
                   'article',
                   jsonb_build_object(
                       'form', 'weight_restricted',
                       'weights', jsonb_build_array('a', 'd'),
                       'query', jsonb_build_object('form', 'plain', 'text', 'engine')
                   ),
                   NULL, 10
               )
           )",
    );
    assert_eq!(body_included, vec!["1".to_owned()]);
}

#[pg_test]
fn json_path_documents_index_only_the_selected_path() {
    lexical_corpus("lex_query_json");
    Spi::run(
        "SELECT pgcontext.register_lexical_source(
             'lex_query_json', 'topic', ARRAY['meta'], 'pg_catalog.simple',
             ARRAY['B'], ARRAY['tags.topic']
         )",
    )
    .expect("JSON-path lexical source should be registered");

    let mut matched = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_json',
               pgcontext.query_lexical(
                   'topic', jsonb_build_object('form', 'plain', 'text', 'database'), NULL, 10
               )
           )",
    );
    matched.sort();
    assert_eq!(matched, vec!["1".to_owned(), "4".to_owned()]);
}

#[pg_test]
fn lexical_leaves_apply_q1_filters_and_exclude_deleted_points() {
    lexical_corpus("lex_query_filters");

    let filtered = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_filters',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'postgres'),
                   jsonb_build_object(
                       'must', jsonb_build_array(
                           jsonb_build_object(
                               'key', 'tenant', 'match', jsonb_build_object('value', 'b')
                           )
                       )
                   ),
                   10
               )
           )",
    );
    assert_eq!(filtered, vec!["2".to_owned()]);

    Spi::run("SELECT pgcontext.delete_points('lex_query_filters', ARRAY['1'])")
        .expect("point should be deleted");
    let remaining = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_filters',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'postgres'), NULL, 10
               )
           )",
    );
    assert!(!remaining.contains(&"1".to_owned()));
}

#[pg_test]
fn lexical_filter_parameter_projection_has_an_inclusive_memory_boundary() {
    let one = crate::retrieval::lexical_filter_parameter_memory(1)
        .expect("one filter point should project");
    let two = crate::retrieval::lexical_filter_parameter_memory(2)
        .expect("two filter points should project");
    assert!(two > one);
    let per_point = two - one;
    let fixed = one - per_point;
    let maximum = context_query::DEFAULT_QUERY_MEMORY_BYTES;
    let boundary = (maximum - fixed) / per_point;
    assert!(
        crate::retrieval::lexical_filter_parameter_memory(boundary)
            .is_ok_and(|bytes| bytes <= maximum)
    );
    assert!(
        crate::retrieval::lexical_filter_parameter_memory(boundary + 1)
            .is_ok_and(|bytes| bytes > maximum)
    );
}

#[pg_test]
fn lexical_recheck_drops_rows_that_no_longer_match_the_registered_predicate() {
    lexical_corpus("lex_query_recheck");
    Spi::run("UPDATE public.lex_query_recheck SET body = 'no longer relevant' WHERE id = 2")
        .expect("source row should be updated");

    let matched = lexical_query_source_keys(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_recheck',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'directly'), NULL, 10
               )
           )",
    );
    assert!(matched.is_empty());
}

#[pg_test]
fn lexical_scores_are_finite_and_ordered_by_native_rank() {
    lexical_corpus("lex_query_scores");
    let scored = lexical_query_scores(
        "SELECT point_id, source_key, score
           FROM pgcontext.execute_query(
               'lex_query_scores',
               pgcontext.query_lexical(
                   'article', jsonb_build_object('form', 'plain', 'text', 'postgres'), NULL, 10
               )
           )",
    );
    assert!(!scored.is_empty());
    assert!(scored.iter().all(|(_, score)| score.is_finite()));
    assert!(
        scored
            .windows(2)
            .all(|pair| pair[0].1 >= pair[1].1 - f64::EPSILON)
    );
}

#[pg_test]
fn unregistered_lexical_sources_fail_closed() {
    lexical_corpus("lex_query_missing");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.execute_query(
             'lex_query_missing',
             pgcontext.query_lexical(
                 'absent', jsonb_build_object('form', 'plain', 'text', 'postgres'), NULL, 5
             )
         )",
        "42704",
        "lexical source is not registered or not visible: absent",
        "unregistered lexical source",
    );
}

#[pg_test]
fn lexical_headline_is_bounded_and_returns_native_markup() {
    lexical_corpus("lex_query_headline");
    let point_ids = Spi::get_one::<Vec<i64>>(
        "SELECT pg_catalog.array_agg(point_id ORDER BY point_id)
           FROM pgcontext._visible_collection_points
          WHERE deleted_at IS NULL",
    )
    .expect("point id query should succeed")
    .expect("point ids should exist");
    let ids = point_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    let highlighted = Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT point_id, headline
                       FROM pgcontext.lexical_headline(
                           'lex_query_headline', 'article', ARRAY[{ids}]::bigint[],
                           jsonb_build_object('form', 'plain', 'text', 'postgres')
                       )"
                ),
                None,
                &[],
            )
            .expect("headline query should succeed");
        rows.into_iter()
            .filter_map(|row| row.get::<String>(2).ok().flatten())
            .collect::<Vec<_>>()
    });
    assert_eq!(highlighted.len(), point_ids.len());
    assert!(highlighted.iter().any(|text| text.contains("<b>postgres")));

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.lexical_headline(
             'lex_query_headline', 'article', ARRAY[]::bigint[],
             jsonb_build_object('form', 'plain', 'text', 'postgres')
         )",
        "54000",
        "headline point count is empty or exceeds the admitted maximum",
        "empty headline request",
    );
}
