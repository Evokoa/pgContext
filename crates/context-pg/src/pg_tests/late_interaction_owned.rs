#[pg_test]
fn owned_late_interaction_catalog_has_private_storage_and_public_visibility_views() {
    let table_names = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(class.relname::text ORDER BY class.relname)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )
            AND class.relkind = 'r'",
    )
    .expect("owned late-interaction catalog query should succeed")
    .expect("owned late-interaction catalog tables should exist");
    assert_eq!(
        table_names,
        vec![
            "_collection_late_interaction".to_owned(),
            "_collection_late_interaction_tokens".to_owned(),
        ]
    );

    let visible_to_public = Spi::get_one::<bool>(
        "SELECT bool_and(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_visible_collection_late_interaction',
                '_visible_collection_late_interaction_tokens'
            )
            AND class.relkind = 'v'",
    )
    .expect("owned late-interaction visibility query should succeed")
    .expect("owned late-interaction visibility views should exist");
    assert!(visible_to_public);

    let private_storage_visible_to_public = Spi::get_one::<bool>(
        "SELECT bool_or(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )
            AND class.relkind = 'r'",
    )
    .expect("owned late-interaction private storage query should succeed")
    .expect("owned late-interaction private tables should exist");
    assert!(!private_storage_visible_to_public);
}

#[pg_test]
fn owned_late_interaction_catalog_is_extension_configuration_data() {
    let dumped_relations = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(class.oid::regclass::text ORDER BY class.relname)
           FROM pg_catalog.pg_extension AS extension
           JOIN unnest(extension.extconfig) AS configured(oid) ON true
           JOIN pg_catalog.pg_class AS class ON class.oid = configured.oid
          WHERE extension.extname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )",
    )
    .expect("extension configuration query should succeed")
    .expect("owned late-interaction catalog should be configuration data");
    assert_eq!(
        dumped_relations,
        vec![
            "pgcontext._collection_late_interaction".to_owned(),
            "pgcontext._collection_late_interaction_tokens".to_owned(),
        ]
    );
}

#[pg_test]
fn owned_late_interaction_tokens_enforce_point_and_ordinal_identity() {
    Spi::run(
        "CREATE TABLE public.m14_owned_catalog_docs (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.m14_owned_catalog_docs
         VALUES (1, ARRAY['[1,0]'::vector]);
         SELECT pgcontext.create_collection(
             'm14_owned_catalog_docs',
             'public.m14_owned_catalog_docs'
         );
         SELECT pgcontext.upsert_points('m14_owned_catalog_docs', ARRAY['1']);",
    )
    .expect("owned token catalog fixture should be created");

    Spi::run(
        "INSERT INTO pgcontext._collection_late_interaction_tokens (
             collection_id,
             point_id,
             token_ordinal,
             token_vector
         )
         SELECT collections.collection_id,
                points.point_id,
                1,
                '[1,0]'::vector
           FROM pgcontext._collections AS collections
           JOIN pgcontext._collection_points AS points USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_catalog_docs'",
    )
    .expect("owned token row should satisfy its composite point identity");

    let ordinal = Spi::get_one::<i32>(
        "SELECT token_ordinal
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_catalog_docs'",
    )
    .expect("owned token ordinal query should succeed")
    .expect("owned token row should exist");
    assert_eq!(ordinal, 1);
}

#[pg_test]
fn register_late_interaction_materializes_owned_tokens_and_builds_hnsw() {
    create_owned_late_interaction_fixture("m14_owned_register");

    let summary = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(
             '|',
             collection,
             source_table,
             token_source,
             dimensions::text,
             point_count::text,
             token_count::text,
             status
         )
           FROM pgcontext.register_late_interaction(
               'm14_owned_register',
               'public.m14_owned_register',
               'token_vectors'
           )",
    )
    .expect("owned late-interaction registration should succeed")
    .expect("owned late-interaction registration should return a summary");
    assert_eq!(
        summary,
        "m14_owned_register|public.m14_owned_register|token_vectors|2|2|4|ready"
    );

    let catalog = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(
             '|',
             registrations.dimensions::text,
             registrations.status,
             access_method.amname,
             pg_catalog.count(tokens.token_id)::text
         )
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pg_catalog.pg_class AS index_class
             ON index_class.oid = registrations.hnsw_index_oid
           JOIN pg_catalog.pg_am AS access_method
             ON access_method.oid = index_class.relam
           LEFT JOIN pgcontext._collection_late_interaction_tokens AS tokens
             USING (collection_id)
          GROUP BY registrations.dimensions,
                   registrations.status,
                   access_method.amname",
    )
    .expect("owned late-interaction catalog query should succeed")
    .expect("owned late-interaction registration should exist");
    assert_eq!(catalog, "2|ready|pgcontext_hnsw|4");

    let source_trigger_exists = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_trigger
              WHERE tgrelid = 'public.m14_owned_register'::regclass
                AND tgname LIKE 'pgcontext_late_interaction_%'
                AND NOT tgisinternal
         )",
    )
    .expect("owned late-interaction trigger query should succeed")
    .expect("owned late-interaction trigger existence should not be null");
    assert!(source_trigger_exists);
}

#[pg_test]
fn owned_late_interaction_source_dml_updates_tokens_in_the_same_transaction() {
    create_owned_late_interaction_fixture("m14_owned_dml");
    register_owned_late_interaction("m14_owned_dml");

    Spi::run(
        "INSERT INTO public.m14_owned_dml
         VALUES (3, ARRAY['[0.25,0.75]'::vector, '[0.75,0.25]'::vector]);",
    )
    .expect("source insert should synchronously capture owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 2);

    Spi::run(
        "UPDATE public.m14_owned_dml
            SET token_vectors = ARRAY['[1,1]'::vector]
          WHERE id = 3",
    )
    .expect("source update should synchronously replace owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 1);
    let updated_vector = Spi::get_one::<String>(
        "SELECT tokens.token_vector::text
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collection_points AS points
             USING (collection_id, point_id)
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_dml'
            AND points.source_key = '3'",
    )
    .expect("updated owned token query should succeed")
    .expect("updated owned token should exist");
    assert_eq!(updated_vector, "[1,1]");

    Spi::run(
        "DO $$
         BEGIN
             BEGIN
                 UPDATE public.m14_owned_dml
                    SET token_vectors = ARRAY['[0,1]'::vector, '[1,0]'::vector]
                  WHERE id = 3;
                 RAISE EXCEPTION 'force rollback';
             EXCEPTION WHEN others THEN
                 NULL;
             END;
         END $$;",
    )
    .expect("source update savepoint should roll back cleanly");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 1);

    Spi::run("DELETE FROM public.m14_owned_dml WHERE id = 3")
        .expect("source delete should synchronously remove owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 0);
    let deleted = Spi::get_one::<bool>(
        "SELECT points.deleted_at IS NOT NULL
           FROM pgcontext._collection_points AS points
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_dml'
            AND points.source_key = '3'",
    )
    .expect("deleted point query should succeed")
    .expect("deleted point mapping should remain");
    assert!(deleted);
}

#[pg_test]
fn dropping_owned_late_interaction_collection_removes_dynamic_objects() {
    create_owned_late_interaction_fixture("m14_owned_cleanup");
    register_owned_late_interaction("m14_owned_cleanup");
    let index_name = Spi::get_one::<String>(
        "SELECT index_class.oid::regclass::text
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pg_catalog.pg_class AS index_class
             ON index_class.oid = registrations.hnsw_index_oid",
    )
    .expect("owned late-interaction index query should succeed")
    .expect("owned late-interaction index should exist");

    Spi::run("SELECT pgcontext.drop_collection('m14_owned_cleanup')")
        .expect("owned late-interaction collection drop should succeed");

    let index_exists = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.to_regclass($1) IS NOT NULL",
        &[index_name.as_str().into()],
    )
    .expect("dropped owned index query should succeed")
    .expect("dropped owned index existence should not be null");
    assert!(!index_exists);
    let source_trigger_exists = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_trigger
              WHERE tgrelid = 'public.m14_owned_cleanup'::regclass
                AND tgname LIKE 'pgcontext_late_interaction_%'
                AND NOT tgisinternal
         )",
    )
    .expect("dropped source trigger query should succeed")
    .expect("dropped source trigger existence should not be null");
    assert!(!source_trigger_exists);
}

fn create_owned_late_interaction_fixture(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.{collection_name}
         VALUES (1, ARRAY['[1,0]'::vector, '[0,1]'::vector]),
                (2, ARRAY['[0.9,0.1]'::vector, '[0.1,0.9]'::vector]);
         SELECT pgcontext.create_collection(
             '{collection_name}',
             'public.{collection_name}'
         );"
    ))
    .expect("owned late-interaction fixture should be created");
}

fn register_owned_late_interaction(collection_name: &str) {
    Spi::run(&format!(
        "SELECT pgcontext.register_late_interaction(
             '{collection_name}',
             'public.{collection_name}',
             'token_vectors'
         )"
    ))
    .expect("owned late-interaction fixture should be registered");
}

fn owned_token_count(collection_name: &str, source_key: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collection_points AS points
             USING (collection_id, point_id)
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1
            AND points.source_key = $2",
        &[collection_name.into(), source_key.into()],
    )
    .expect("owned token count query should succeed")
    .expect("owned token count should not be null")
}
