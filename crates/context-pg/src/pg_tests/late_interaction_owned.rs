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
