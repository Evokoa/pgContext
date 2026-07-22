#[pg_test]
fn security_definer_functions_exclude_public_from_fixed_search_paths() {
    let insecure = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_proc AS procedure
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = procedure.pronamespace
          WHERE namespace.nspname = 'pgcontext'
            AND procedure.prosecdef
            AND (
                NOT EXISTS (
                    SELECT 1
                      FROM unnest(coalesce(procedure.proconfig, ARRAY[]::text[])) AS setting
                     WHERE setting LIKE 'search_path=%'
                )
                OR EXISTS (
                    SELECT 1
                      FROM unnest(coalesce(procedure.proconfig, ARRAY[]::text[])) AS setting
                     WHERE setting LIKE 'search_path=%public%'
                )
            )",
    )
    .expect("security-definer search paths should be inspectable")
    .expect("security-definer search path count should not be null");

    assert_eq!(insecure, 0);
}

#[pg_test]
fn sql_enums_are_owned_by_the_trusted_extension_schema() {
    let enum_names = "ARRAY[
        'buildjobstatus',
        'embeddingmigrationstatus',
        'indexadvisorrecommendation',
        'indexdiagnosticstatus',
        'indexlifecyclestatus',
        'indexmemoryestimatestatus',
        'optimizationstatus',
        'querycohortstatus',
        'queryexplainstatus',
        'querylatencybucket',
        'querylifecyclestate',
        'recallcheckstatus',
        'telemetrystatus',
        'vacuumadvicestatus'
    ]::text[]";
    let trusted = security_count(&format!(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_type AS type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = type.typnamespace
          WHERE type.typtype = 'e'
            AND lower(type.typname) = ANY ({enum_names})
            AND namespace.nspname = 'pgcontext'"
    ));
    let misplaced = security_count(&format!(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_type AS type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = type.typnamespace
          WHERE type.typtype = 'e'
            AND lower(type.typname) = ANY ({enum_names})
            AND namespace.nspname <> 'pgcontext'"
    ));

    assert_eq!(trusted, 14);
    assert_eq!(misplaced, 0);
}

#[pg_test]
fn vector_types_are_canonical_extension_owned_objects() {
    let canonical = security_count(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_type AS type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = type.typnamespace
           JOIN pg_catalog.pg_depend AS dependency
             ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
            AND dependency.objid = type.oid
            AND dependency.deptype = 'e'
           JOIN pg_catalog.pg_extension AS extension
             ON extension.oid = dependency.refobjid
          WHERE namespace.nspname = 'pgcontext'
            AND type.typname = ANY (ARRAY['vector', 'halfvec', 'sparsevec', 'bitvec'])
            AND extension.extname = 'pgcontext'",
    );
    let misplaced = security_count(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_type AS type
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = type.typnamespace
           JOIN pg_catalog.pg_depend AS dependency
             ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
            AND dependency.objid = type.oid
            AND dependency.deptype = 'e'
           JOIN pg_catalog.pg_extension AS extension
             ON extension.oid = dependency.refobjid
          WHERE namespace.nspname <> 'pgcontext'
            AND type.typname = ANY (ARRAY['vector', 'halfvec', 'sparsevec', 'bitvec'])
            AND extension.extname = 'pgcontext'",
    );

    assert_eq!(canonical, 4);
    assert_eq!(misplaced, 0);
}

#[pg_test]
fn canonical_vector_type_families_bind_only_pgcontext_objects() {
    let complete_families = security_count(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_type AS base_type
           JOIN pg_catalog.pg_namespace AS base_namespace
             ON base_namespace.oid = base_type.typnamespace
           JOIN pg_catalog.pg_type AS array_type
             ON array_type.oid = base_type.typarray
            AND array_type.typelem = base_type.oid
            AND array_type.typnamespace = base_type.typnamespace
           JOIN pg_catalog.pg_proc AS input_function
             ON input_function.oid = base_type.typinput
            AND input_function.pronamespace = base_type.typnamespace
           JOIN pg_catalog.pg_proc AS output_function
             ON output_function.oid = base_type.typoutput
            AND output_function.pronamespace = base_type.typnamespace
           JOIN pg_catalog.pg_proc AS typmod_input
             ON typmod_input.oid = base_type.typmodin
            AND typmod_input.pronamespace = base_type.typnamespace
           JOIN pg_catalog.pg_proc AS typmod_output
             ON typmod_output.oid = base_type.typmodout
            AND typmod_output.pronamespace = base_type.typnamespace
          WHERE base_namespace.nspname = 'pgcontext'
            AND base_type.typname = ANY (ARRAY['vector', 'halfvec', 'sparsevec', 'bitvec'])
            AND EXISTS (
                SELECT 1
                  FROM pg_catalog.pg_opclass AS operator_class
                  JOIN pg_catalog.pg_am AS access_method
                    ON access_method.oid = operator_class.opcmethod
                 WHERE operator_class.opcintype = base_type.oid
                   AND operator_class.opcnamespace = base_type.typnamespace
                   AND access_method.amname = 'btree'
            )
            AND EXISTS (
                SELECT 1
                  FROM pg_catalog.pg_opclass AS operator_class
                  JOIN pg_catalog.pg_am AS access_method
                    ON access_method.oid = operator_class.opcmethod
                 WHERE operator_class.opcintype = base_type.oid
                   AND operator_class.opcnamespace = base_type.typnamespace
                   AND access_method.amname = 'pgcontext_hnsw'
            )",
    );

    assert_eq!(complete_families, 4);
}

#[pg_test]
fn security_definer_collection_create_ignores_hostile_search_path() {
    Spi::run("CREATE SCHEMA msec_shadow").expect("shadow schema should be created");
    Spi::run("CREATE TABLE msec_shadow._collections (collection_name text)")
        .expect("shadow collection table should be created");
    Spi::run(
        "CREATE FUNCTION msec_shadow.to_regclass(text)
         RETURNS oid
         LANGUAGE sql
         AS $$ SELECT 0::oid $$",
    )
    .expect("shadow to_regclass function should be created");
    Spi::run(
        "CREATE TABLE public.msec_shadow_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )",
    )
    .expect("source table should be created");

    Spi::run("SET LOCAL search_path = msec_shadow, public, pgcontext, pg_catalog")
        .expect("hostile search_path should be set");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'msec_shadow_docs',
             'public.msec_shadow_docs'
         )",
    )
    .expect("collection creation should ignore hostile search_path");

    assert_eq!(security_count("SELECT count(*)::bigint FROM msec_shadow._collections"), 0);
    assert_eq!(
        security_count(
            "SELECT count(*)::bigint
               FROM pgcontext._collections
              WHERE collection_name = 'msec_shadow_docs'",
        ),
        1
    );
}

#[pg_test]
fn security_definer_catalog_writers_ignore_hostile_shadow_objects() {
    Spi::run("CREATE SCHEMA msec_catalog_shadow").expect("shadow schema should be created");
    create_security_shadow_catalog_tables("msec_catalog_shadow");
    Spi::run(
        "CREATE TABLE public.msec_catalog_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             status text NOT NULL,
             metadata jsonb NOT NULL
         )",
    )
    .expect("source table should be created");
    Spi::run(
        "INSERT INTO public.msec_catalog_docs (id, embedding, status, metadata)
         VALUES (1, '[0,0]'::vector, 'open', '{\"topic\":\"rust\"}'::jsonb)",
    )
    .expect("source row should be inserted");

    Spi::run("SET LOCAL search_path = msec_catalog_shadow, public, pgcontext, pg_catalog")
        .expect("hostile search_path should be set");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'msec_catalog_docs',
             'public.msec_catalog_docs'
         )",
    )
    .expect("collection creation should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.register_vector(
             'msec_catalog_docs',
             'embedding',
             'embedding',
             2,
             'l2'
         )",
    )
    .expect("vector registration should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.register_filter_column(
             'msec_catalog_docs',
             'status',
             'status'
         )",
    )
    .expect("filter registration should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.register_jsonb_path(
             'msec_catalog_docs',
             'topic',
             'metadata',
             ARRAY['topic']
         )",
    )
    .expect("JSONB path registration should ignore shadow catalog");
    Spi::run("SELECT pgcontext.upsert_points('msec_catalog_docs', ARRAY['1'])")
        .expect("point upsert should ignore shadow catalog");
    Spi::run("SELECT pgcontext.delete_points('msec_catalog_docs', ARRAY['1'])")
        .expect("point delete should ignore shadow catalog");
    Spi::run("SELECT pgcontext.upsert_points('msec_catalog_docs', ARRAY['1'])")
        .expect("point reactivation should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.register_model_version(
             'msec_catalog_docs',
             'model',
             'v1',
             2,
             'l2'
         )",
    )
    .expect("source model version should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.register_model_version(
             'msec_catalog_docs',
             'model',
             'v2',
             2,
             'l2'
         )",
    )
    .expect("target model version should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.create_embedding_migration(
             'msec_catalog_docs',
             'model',
             'v1',
             'model',
             'v2',
             1
         )",
    )
    .expect("embedding migration should ignore shadow catalog");
    Spi::run(
        "SELECT pgcontext.record_query_stat(
             'msec_catalog_docs',
             'release/security',
             'search',
             1,
             1,
             0.25
         )",
    )
    .expect("query stat should ignore shadow catalog");
    Spi::run("SELECT * FROM pgcontext.collection_info('msec_catalog_docs')")
        .expect("collection info should ignore shadow catalog");

    assert_eq!(
        security_count(
            "SELECT count(*)::bigint
               FROM pgcontext._embedding_migrations
              WHERE total_points = 1",
        ),
        1
    );
    assert_eq!(
        security_count(
            "SELECT count(*)::bigint
               FROM pgcontext._query_stats
              WHERE cohort = 'release/security'",
        ),
        1
    );
    assert_eq!(security_shadow_catalog_row_count("msec_catalog_shadow"), 0);

    Spi::run("SELECT pgcontext.drop_collection('msec_catalog_docs')")
        .expect("drop collection should ignore shadow catalog");
    assert_eq!(
        security_count(
            "SELECT count(*)::bigint
               FROM pgcontext._collections
              WHERE collection_name = 'msec_catalog_docs'",
        ),
        0
    );
    assert_eq!(security_shadow_catalog_row_count("msec_catalog_shadow"), 0);
}

fn create_security_shadow_catalog_tables(schema_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._collections (
             collection_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_name text,
             owner_role oid,
             source_table_oid oid,
             source_schema_name text,
             source_table_name text
         )"
    ))
    .expect("shadow collections table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._collection_vectors (
             vector_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             vector_name text,
             source_table_oid oid,
             source_schema_name text,
             source_table_name text,
             vector_column_name text,
             vector_attnum int2,
             dimensions int4,
             metric text
         )"
    ))
    .expect("shadow vectors table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._collection_payload_columns (
             payload_column_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             filter_key text,
             source_table_oid oid,
             source_schema_name text,
             source_table_name text,
             column_name text,
             column_attnum int2,
             jsonb_path text[]
         )"
    ))
    .expect("shadow payload table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._collection_points (
             point_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             source_key text,
             deleted_at timestamptz
         )"
    ))
    .expect("shadow points table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._model_versions (
             model_version_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             model_name text,
             model_version text,
             dimensions int4,
             metric text,
             is_active boolean
         )"
    ))
    .expect("shadow model versions table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._embedding_migrations (
             migration_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             source_model_version_id bigint,
             target_model_version_id bigint,
             status text,
             total_points bigint,
             processed_points bigint
         )"
    ))
    .expect("shadow embedding migrations table should be created");
    Spi::run(&format!(
        "CREATE TABLE {schema_name}._query_stats (
             query_stat_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             collection_id bigint,
             cohort text,
             query_kind text,
             result_count bigint,
             candidate_count bigint,
             latency_ms double precision
         )"
    ))
    .expect("shadow query stats table should be created");
}

fn security_shadow_catalog_row_count(schema_name: &str) -> i64 {
    security_count(&format!(
        "SELECT (
            (SELECT count(*) FROM {schema_name}._collections) +
            (SELECT count(*) FROM {schema_name}._collection_vectors) +
            (SELECT count(*) FROM {schema_name}._collection_payload_columns) +
            (SELECT count(*) FROM {schema_name}._collection_points) +
            (SELECT count(*) FROM {schema_name}._model_versions) +
            (SELECT count(*) FROM {schema_name}._embedding_migrations) +
            (SELECT count(*) FROM {schema_name}._query_stats)
         )::bigint"
    ))
}

fn security_count(sql: &str) -> i64 {
    Spi::get_one::<i64>(sql)
        .expect("security count query should succeed")
        .expect("security count should not be null")
}
