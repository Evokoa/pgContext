#[pg_test]
fn exact_first_inspection_classifies_supported_columns_without_mutation() {
    Spi::run(
        "CREATE TABLE public.p14_inspect (
             id uuid PRIMARY KEY,
             embedding vector(3) NOT NULL,
             half_embedding halfvec(3),
             body text,
             document tsvector,
             metadata jsonb,
             observed_at timestamptz,
             opaque bytea
         )",
    )
    .expect("exact-first inspection fixture should be created");

    let rows = Spi::connect(|client| {
        let rows = client.select(
            "SELECT column_name, support_family, supported
               FROM pgcontext.inspect_exact_first_source('public.p14_inspect')
              ORDER BY ordinal_position",
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("column name should exist"),
                row.get::<String>(2)?,
                row.get::<bool>(3)?.expect("supported flag should exist"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("exact-first inspection should return rows");

    assert_eq!(rows[0], ("id".to_owned(), Some("temporal_uuid".to_owned()), true));
    assert_eq!(rows[1].1.as_deref(), Some("vector"));
    assert_eq!(rows[2].1.as_deref(), Some("halfvec"));
    assert_eq!(rows[3].1.as_deref(), Some("text"));
    assert_eq!(rows[4].1.as_deref(), Some("tsvector"));
    assert_eq!(rows[5].1.as_deref(), Some("jsonb"));
    assert_eq!(rows[6].1.as_deref(), Some("temporal_uuid"));
    assert_eq!(rows[7], ("opaque".to_owned(), None, false));
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM pgcontext._exact_first_registrations")
            .expect("registration count should load"),
        Some(0)
    );
}

#[pg_test]
fn exact_first_registration_is_idempotent_and_immediately_exact() {
    Spi::run(
        "CREATE TABLE public.p14_register (
             id bigint PRIMARY KEY,
             embedding vector(3) NOT NULL,
             body text NOT NULL,
             tenant_id uuid NOT NULL
         );
         INSERT INTO public.p14_register
         VALUES (1, '[1,0,0]'::vector, 'alpha',
                 '00000000-0000-0000-0000-000000000001'::uuid);",
    )
    .expect("exact-first registration fixture should be created");
    let specification = "jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(
            jsonb_build_object(
                'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                'dimensions', 3, 'metric', 'cosine', 'normalization', 'unit_l2'
            ),
            jsonb_build_object(
                'name', 'tenant', 'column', 'tenant_id', 'kind', 'filter'
            )
        )
    )";
    let sql = format!(
        "SELECT registration_revision, readiness_state, readiness_reason
           FROM pgcontext.register_exact_first(
               'p14_register', 'public.p14_register', {specification}
           )"
    );
    let first = Spi::get_three::<i64, String, String>(&sql)
        .expect("first exact-first registration should succeed");
    let second = Spi::get_three::<i64, String, String>(&sql)
        .expect("identical exact-first registration should converge");
    assert_eq!(first, second);
    assert_eq!(first.0, Some(1));
    assert_eq!(first.1.as_deref(), Some("exact_only"));
    assert_eq!(first.2.as_deref(), Some("current_exact_path"));
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM pgcontext._exact_first_registrations"
        )
        .expect("registration count should load"),
        Some(1)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM pgcontext._exact_first_columns")
            .expect("binding count should load"),
        Some(2)
    );

    let readiness = Spi::get_two::<String, String>(
        "SELECT readiness_state, readiness_reason
           FROM pgcontext.exact_first_readiness('p14_register')",
    )
    .expect("exact-first readiness should load");
    assert_eq!(
        (readiness.0.as_deref(), readiness.1.as_deref()),
        (Some("exact_only"), Some("current_exact_path"))
    );
    let exact = Spi::get_two::<String, f32>(
        "SELECT source_key, score
           FROM pgcontext.exact_first_search(
               'p14_register', 'embedding', '[1,0,0]'::vector, 1
           )",
    )
    .expect("exact-first source query should be immediately available");
    assert_eq!(exact.0.as_deref(), Some("1"));
    assert_eq!(exact.1, Some(0.0));
    let compatible = Spi::get_two::<i64, String>(
        "SELECT point_id, source_key
           FROM pgcontext.search('p14_register', '[1,0,0]'::vector, 1)",
    )
    .expect("the existing dense search surface should use the exact-first fallback");
    assert_eq!(compatible.0, Some(1));
    assert_eq!(compatible.1.as_deref(), Some("1"));
}

#[pg_test]
fn exact_first_registration_rejects_conflict_and_invalid_key_contracts() {
    Spi::run(
        "CREATE TABLE public.p14_conflict (
             id bigint PRIMARY KEY,
             other_id bigint,
             embedding vector(2) NOT NULL
         )",
    )
    .expect("exact-first conflict fixture should be created");
    let base = "jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(jsonb_build_object(
            'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
            'dimensions', 2, 'metric', 'l2'
        ))
    )";
    Spi::run(&format!(
        "SELECT * FROM pgcontext.register_exact_first(
             'p14_conflict', 'public.p14_conflict', {base}
         )"
    ))
    .expect("base exact-first registration should succeed");

    assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.register_exact_first(
                 'p14_conflict', 'public.p14_conflict',
                 jsonb_set({base}, '{{bindings,0,metric}}', '\"cosine\"'::jsonb)
             )"
        ),
        "42710",
        "exact-first registration already exists with a different specification",
        "exact-first conflicting registration",
    );

    Spi::run(
        "CREATE TABLE public.p14_bad_key (
             id bigint,
             embedding vector(2) NOT NULL
         );
         SELECT pgcontext.create_collection('p14_bad_key', 'public.p14_bad_key');",
    )
    .expect("exact-first bad-key fixture should be created");
    assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.register_exact_first(
                 'p14_bad_key', 'public.p14_bad_key', {base}
             )"
        ),
        "22023",
        "exact-first source key must be non-null bigint/integer/smallint/text/varchar/uuid",
        "exact-first invalid key contract",
    );
}

#[pg_test]
fn exact_first_readiness_fails_honestly_on_column_drift() {
    Spi::run(
        "CREATE TABLE public.p14_drift (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             metadata jsonb NOT NULL
         )",
    )
    .expect("exact-first drift fixture should be created");
    Spi::run(
        "SELECT * FROM pgcontext.register_exact_first(
             'p14_drift', 'public.p14_drift',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(
                     jsonb_build_object(
                         'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                         'dimensions', 2, 'metric', 'l2'
                     ),
                     jsonb_build_object(
                         'name', 'metadata', 'column', 'metadata', 'kind', 'payload'
                     )
                 )
             )
         )",
    )
    .expect("exact-first drift registration should succeed");
    Spi::run(
        "ALTER TABLE public.p14_drift
             ALTER COLUMN metadata TYPE text USING metadata::text",
    )
    .expect("exact-first source column should drift");
    let readiness = Spi::get_two::<String, String>(
        "SELECT readiness_state, readiness_reason
           FROM pgcontext.exact_first_readiness('p14_drift')",
    )
    .expect("drift readiness should load");
    assert_eq!(readiness.0.as_deref(), Some("stale"));
    assert_eq!(readiness.1.as_deref(), Some("source_column_changed"));
}

#[pg_test]
fn exact_first_source_key_rename_is_stale_before_dynamic_search_sql() {
    Spi::run(
        "CREATE TABLE public.p14_key_rename (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL
         );
         INSERT INTO public.p14_key_rename VALUES (1, '[0,0]');
         SELECT * FROM pgcontext.register_exact_first(
             'p14_key_rename', 'public.p14_key_rename',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 2, 'metric', 'l2'
                 ))
             )
         );
         ALTER TABLE public.p14_key_rename RENAME COLUMN id TO renamed_id;",
    )
    .expect("exact-first key-rename fixture should be prepared");
    let readiness = Spi::get_two::<String, String>(
        "SELECT readiness_state, readiness_reason
           FROM pgcontext.exact_first_readiness('p14_key_rename')",
    )
    .expect("renamed key readiness should load");
    assert_eq!(readiness.0.as_deref(), Some("stale"));
    assert_eq!(readiness.1.as_deref(), Some("source_key_changed"));
    assert_sql_failure(
        "SELECT * FROM pgcontext.exact_first_search(
             'p14_key_rename', 'embedding', '[0,0]'::vector, 1
         )",
        "55000",
        "exact-first registration is stale",
        "exact-first renamed key search",
    );
}

#[pg_test]
fn exact_first_logical_restore_refreshes_only_equivalent_physical_bindings() {
    Spi::run(
        "CREATE TABLE public.p14_restore (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             tenant int4 NOT NULL
         );
         INSERT INTO public.p14_restore VALUES (1, '[0,0]', 7);
         SELECT * FROM pgcontext.register_exact_first(
             'p14_restore', 'public.p14_restore',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(
                     jsonb_build_object(
                         'name', 'embedding', 'column', 'embedding',
                         'kind', 'dense', 'dimensions', 2, 'metric', 'l2'
                     ),
                     jsonb_build_object(
                         'name', 'tenant', 'column', 'tenant', 'kind', 'filter'
                     )
                 )
             )
         );
         UPDATE pgcontext._exact_first_registrations
            SET registration_system_identifier = 0,
                registration_database_oid = 0,
                source_table_oid = 0,
                source_key_attnum = 32767,
                source_key_index_oid = 0;
         UPDATE pgcontext._exact_first_columns
            SET column_attnum = 32767, column_type_oid = 0;",
    )
    .expect("logical restore identity fixture should be prepared");

    let readiness = Spi::get_two::<String, String>(
        "SELECT readiness_state, readiness_reason
           FROM pgcontext.exact_first_readiness('p14_restore')",
    )
    .expect("logical restore refresh should succeed");
    assert_eq!(
        (readiness.0.as_deref(), readiness.1.as_deref()),
        (Some("exact_only"), Some("current_exact_path"))
    );
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT registrations.source_table_oid = 'public.p14_restore'::regclass
                    AND registrations.source_key_attnum =
                        (SELECT attnum FROM pg_attribute
                          WHERE attrelid = 'public.p14_restore'::regclass
                            AND attname = 'id')
                    AND bool_and(columns.column_attnum = attributes.attnum
                                 AND columns.column_type_oid = attributes.atttypid)
               FROM pgcontext._exact_first_registrations AS registrations
               JOIN pgcontext._exact_first_columns AS columns
                 USING (exact_first_registration_id)
               JOIN pg_attribute AS attributes
                 ON attributes.attrelid = registrations.source_table_oid
                AND attributes.attname = columns.column_name
              GROUP BY registrations.source_table_oid,
                       registrations.source_key_attnum"
        )
        .expect("refreshed exact-first identities should load"),
        Some(true)
    );
}

#[pg_test]
fn exact_first_advisor_freezes_quoted_idempotent_hnsw_plan() {
    Spi::run(
        "CREATE TABLE public.p14_advisor (
             id bigint PRIMARY KEY,
             embedding vector(3) NOT NULL
         );
         INSERT INTO public.p14_advisor
         SELECT value, ARRAY[value::real, 0::real, 1::real]::vector
           FROM generate_series(1, 12000) AS value;
         ANALYZE public.p14_advisor;
         SELECT * FROM pgcontext.register_exact_first(
             'p14_advisor', 'public.p14_advisor',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 3, 'metric', 'cosine'
                 ))
             )
         )",
    )
    .expect("exact-first advisor fixture should register");
    let advisor_sql = "SELECT pg_catalog.concat_ws('|', plan_revision, recommendation,
                                                    precision, generated_ddl)
         FROM pgcontext.exact_first_advisor(
             'p14_advisor',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000
             )
         )";
    let first = Spi::get_one::<String>(advisor_sql)
        .expect("first exact-first advice should succeed")
        .expect("first exact-first advice should return a row");
    let second = Spi::get_one::<String>(advisor_sql)
        .expect("identical exact-first advice should converge")
        .expect("identical exact-first advice should return a row");
    assert_eq!(first, second);
    let mut fields = first.splitn(4, '|');
    assert_eq!(fields.next(), Some("1"));
    assert_eq!(fields.next(), Some("hnsw"));
    assert_eq!(fields.next(), Some("full"));
    let ddl = fields.next().expect("HNSW advice should include DDL");
    assert!(ddl.starts_with("CREATE INDEX CONCURRENTLY IF NOT EXISTS \"pgcontext_ef_"));
    assert!(ddl.contains("ON \"public\".\"p14_advisor\" USING pgcontext_hnsw"));
    assert!(ddl.contains("\"embedding\" pgcontext.vector_hnsw_cosine_ops"));
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM pgcontext._exact_first_plans")
            .expect("exact-first plan count should load"),
        Some(1)
    );
}

#[pg_test]
fn exact_first_foreground_apply_publishes_only_a_validated_index() {
    Spi::run(
        "CREATE TABLE public.p14_foreground (
             id bigint PRIMARY KEY,
             embedding vector(3) NOT NULL
         );
         INSERT INTO public.p14_foreground
         SELECT value, ARRAY[value::real, 1::real, 0::real]::vector
           FROM generate_series(1, 10000) AS value;
         ANALYZE public.p14_foreground;
         SELECT * FROM pgcontext.register_exact_first(
             'p14_foreground', 'public.p14_foreground',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 3, 'metric', 'l2'
                 ))
             )
         );
         SELECT * FROM pgcontext.exact_first_advisor(
             'p14_foreground',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000
             )
         )",
    )
    .expect("exact-first foreground fixture and plan should be created");
    Spi::run(
        "INSERT INTO pgcontext._exact_first_targets (
             exact_first_registration_id, exact_first_plan_id,
             index_oid, index_fingerprint_sha256, lifecycle_state,
             structurally_validated
         )
         SELECT registrations.exact_first_registration_id,
                plans.exact_first_plan_id, NULL,
                decode(repeat('00', 32), 'hex'), 'retired', false
           FROM pgcontext._exact_first_registrations AS registrations
           JOIN pgcontext._exact_first_plans AS plans
             USING (exact_first_registration_id)
          CROSS JOIN generate_series(1, 16)
          WHERE registrations.collection_id = (
                SELECT collection_id FROM pgcontext._collections
                 WHERE collection_name = 'p14_foreground'
          )",
    )
    .expect("bounded retired target history should be seeded");
    let result = Spi::get_three::<String, String, pg_sys::Oid>(
        "SELECT plan_status, readiness_state, index_oid
           FROM pgcontext.apply_exact_first_plan(
               'p14_foreground', 1, 'apply_foreground'
           )",
    )
    .expect("exact-first foreground apply should succeed");
    assert_eq!(result.0.as_deref(), Some("published"));
    assert_eq!(result.1.as_deref(), Some("indexed"));
    assert!(result.2.is_some());
    let replay = Spi::get_two::<String, String>(
        "SELECT plan_status, readiness_state
           FROM pgcontext.apply_exact_first_plan(
               'p14_foreground', 1, 'apply_foreground'
           )",
    )
    .expect("exact-first foreground replay should converge");
    assert_eq!(replay.0.as_deref(), Some("published"));
    assert_eq!(replay.1.as_deref(), Some("indexed"));
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT count(*) = 16
                    AND count(*) FILTER (WHERE lifecycle_state = 'current') = 1
                    AND bool_and(recall_bps IS NULL)
                    AND bool_or(structurally_validated)
               FROM pgcontext._exact_first_targets"
        )
        .expect("bounded exact-first target history should load"),
        Some(true)
    );
    let index_name = Spi::get_one_with_args::<String>(
        "SELECT $1::regclass::text",
        &[result.2.expect("published index OID should exist").into()],
    )
    .expect("published exact-first index name should load")
    .expect("published exact-first index should resolve");
    Spi::run(&format!("DROP INDEX {index_name}"))
        .expect("published exact-first index should be droppable");
    let readiness = Spi::get_two::<String, String>(
        "SELECT readiness_state, readiness_reason
           FROM pgcontext.exact_first_readiness('p14_foreground')",
    )
    .expect("post-drop exact-first readiness should load");
    assert_eq!(readiness.0.as_deref(), Some("exact_only"));
    assert_eq!(readiness.1.as_deref(), Some("current_exact_path"));
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT plan_status FROM pgcontext.exact_first_progress('p14_foreground')"
        )
        .expect("post-drop exact-first progress should load")
        .as_deref(),
        Some("frozen")
    );
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._visible_exact_first_plans
              WHERE plan_revision = 1"
        )
        .expect("post-drop visible plan should load")
        .as_deref(),
        Some("frozen")
    );
}

#[pg_test]
fn exact_first_advisor_cannot_displace_an_active_build() {
    Spi::run(
        "CREATE TABLE public.p14_stale_publish (
             id bigint PRIMARY KEY,
             embedding vector(3) NOT NULL
         );
         INSERT INTO public.p14_stale_publish
         SELECT value, ARRAY[value::real, 1::real, 0::real]::vector
           FROM generate_series(1, 10000) AS value;
         ANALYZE public.p14_stale_publish;
         SELECT * FROM pgcontext.register_exact_first(
             'p14_stale_publish', 'public.p14_stale_publish',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 3, 'metric', 'l2'
                 ))
             )
         );
         SELECT * FROM pgcontext.exact_first_advisor(
             'p14_stale_publish',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000
             )
         );
         SELECT * FROM pgcontext.apply_exact_first_plan(
             'p14_stale_publish', 1, 'enqueue'
         )",
    )
    .expect("stale-publication fixture should enqueue");
    let claim = Spi::get_two::<i64, String>(
        "SELECT lease_token, generated_ddl
           FROM pgcontext.claim_exact_first_build(
               'p14_stale_publish', 'stale-publish-worker', 60000
           )",
    )
    .expect("stale-publication fixture should claim");
    let token = claim.0.expect("stale-publication claim should have a token");
    let foreground_ddl = claim
        .1
        .expect("stale-publication claim should have DDL")
        .replacen("CREATE INDEX CONCURRENTLY ", "CREATE INDEX ", 1);
    Spi::run(&foreground_ddl).expect("stale-publication test index should build");
    assert_sql_failure(
        "SELECT * FROM pgcontext.exact_first_advisor(
             'p14_stale_publish',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000,
                 'filter_selectivity_bps', 400
             )
         )",
        "55000",
        "an active exact-first build must finish or become terminal before replacing its plan",
        "exact-first active plan replacement",
    );
    Spi::run(
        &format!(
            "SELECT * FROM pgcontext.publish_exact_first_build(
                 'p14_stale_publish', 1, {token}
             )"
        ),
    )
    .expect("the still-current exact-first build should publish");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT registrations.current_plan_revision = 1
                    AND (SELECT count(*) FROM pgcontext._exact_first_plans AS plans
                          WHERE plans.exact_first_registration_id =
                                registrations.exact_first_registration_id) = 1
                    AND NOT EXISTS (
                        SELECT 1
                          FROM pgcontext._exact_first_plan_jobs AS plan_jobs
                          JOIN pgcontext._exact_first_plans AS plans
                            USING (exact_first_plan_id)
                         WHERE plans.exact_first_registration_id =
                               registrations.exact_first_registration_id
                    )
                    AND NOT EXISTS (
                        SELECT 1
                          FROM pgcontext._build_jobs AS jobs
                         WHERE jobs.collection_id = registrations.collection_id
                           AND jobs.artifact_kind = 'index'
                           AND jobs.artifact_name = 'exact_first'
                           AND jobs.status IN (
                               'planned','building','validating','publishing',
                               'cancel_requested'
                           )
                    )
                    AND EXISTS (
                        SELECT 1
                          FROM pgcontext._exact_first_targets AS targets
                          JOIN pgcontext._exact_first_plans AS plans
                            USING (exact_first_plan_id)
                         WHERE targets.exact_first_registration_id =
                               registrations.exact_first_registration_id
                           AND targets.lifecycle_state = 'current'
                           AND targets.structurally_validated
                           AND targets.index_oid = pg_catalog.to_regclass(
                               pg_catalog.format(
                                   '%I.%I', registrations.source_schema_name,
                                   plans.evidence->>'index_name'
                               )
                           )
                    )
               FROM pgcontext._exact_first_registrations AS registrations
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE collections.collection_name = 'p14_stale_publish'"
        )
        .expect("active-plan publication catalog state should load"),
        Some(true)
    );
}

#[pg_test]
fn exact_first_controller_fences_top_level_builds_and_retries_failures() {
    Spi::run(
        "CREATE TABLE public.p14_controller (
             id bigint PRIMARY KEY,
             embedding vector(3) NOT NULL
         );
         INSERT INTO public.p14_controller
         SELECT value, ARRAY[value::real, 0::real, 1::real]::vector
           FROM generate_series(1, 10000) AS value;
         ANALYZE public.p14_controller;
         SELECT * FROM pgcontext.register_exact_first(
             'p14_controller', 'public.p14_controller',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 3, 'metric', 'cosine'
                 ))
             )
         );
         SELECT * FROM pgcontext.exact_first_advisor(
             'p14_controller',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000
             )
         );
         SELECT * FROM pgcontext.apply_exact_first_plan(
             'p14_controller', 1, 'enqueue'
         )",
    )
    .expect("exact-first controller fixture should enqueue");
    let claim = Spi::get_two::<i64, String>(
        "SELECT lease_token, generated_ddl
           FROM pgcontext.claim_exact_first_build(
               'p14_controller', 'pg-test-controller', 60000
           )",
    )
    .expect("exact-first controller should claim queued work");
    let token = claim.0.expect("exact-first claim should return a token");
    let ddl = claim.1.expect("exact-first claim should return DDL");
    assert!(ddl.starts_with("CREATE INDEX CONCURRENTLY IF NOT EXISTS"));
    assert_eq!(
        Spi::get_one_with_args::<bool>(
            "SELECT pgcontext.heartbeat_exact_first_build(
                 'p14_controller', 1, $1, 60000
             )",
            &[token.into()],
        )
        .expect("exact-first heartbeat should succeed"),
        Some(true)
    );
    let foreground_ddl = ddl.replacen("CREATE INDEX CONCURRENTLY ", "CREATE INDEX ", 1);
    Spi::run(&foreground_ddl).expect("test controller should execute reviewed DDL");
    let published = Spi::get_two_with_args::<String, String>(
        "SELECT plan_status, readiness_state
           FROM pgcontext.publish_exact_first_build('p14_controller', 1, $1)",
        &[token.into()],
    )
    .expect("exact-first controller should publish the validated index");
    assert_eq!(published.0.as_deref(), Some("published"));
    assert_eq!(published.1.as_deref(), Some("indexed"));

    Spi::run(
        "SELECT * FROM pgcontext.exact_first_advisor(
             'p14_controller',
             jsonb_build_object(
                 'version', 'exact_first_advisor_v1',
                 'memory_budget_bytes', 1000000000,
                 'build_window_seconds', 3600,
                 'update_millihertz', 10000,
                 'filter_selectivity_bps', 400
             )
         );
         SELECT * FROM pgcontext.apply_exact_first_plan(
             'p14_controller', 2, 'enqueue'
         )",
    )
    .expect("second exact-first plan should enqueue");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pgcontext.cancel_exact_first_build('p14_controller')"
        )
        .expect("queued exact-first cancellation should succeed"),
        Some(true)
    );
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pgcontext.retry_exact_first_build('p14_controller')"
        )
        .expect("cancelled exact-first plan should retry"),
        Some(true)
    );
    let retry_token = Spi::get_one::<i64>(
        "SELECT lease_token
           FROM pgcontext.claim_exact_first_build(
               'p14_controller', 'pg-test-controller', 60000
           )",
    )
    .expect("retried exact-first plan should claim")
    .expect("retried claim should return token");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pgcontext.cancel_exact_first_build('p14_controller')"
        )
        .expect("active exact-first cancellation should succeed"),
        Some(true)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM pgcontext.claim_exact_first_build(
                 'p14_controller', 'must-not-reclaim-cancelled', 60000
             )"
        )
        .expect("unexpired cancelled claim count should load"),
        Some(0)
    );
    assert_eq!(
        Spi::get_one_with_args::<bool>(
            "SELECT pgcontext.heartbeat_exact_first_build(
                 'p14_controller', 2, $1, 60000
             )",
            &[retry_token.into()],
        )
        .expect("cancel-requested heartbeat should be fenced"),
        Some(false)
    );
    Spi::run(
        "UPDATE pgcontext._exact_first_plan_jobs
            SET lease_expires_at = pg_catalog.clock_timestamp() - interval '1 second'
          WHERE status = 'cancel_requested';
         SELECT * FROM pgcontext.claim_exact_first_build(
             'p14_controller', 'cancel-terminalizer', 60000
         );",
    )
    .expect("expired cancellation should terminalize without a claim");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pgcontext.retry_exact_first_build('p14_controller')"
        )
        .expect("terminalized exact-first cancellation should retry"),
        Some(true)
    );
    let retry_token = Spi::get_one::<i64>(
        "SELECT lease_token
           FROM pgcontext.claim_exact_first_build(
               'p14_controller', 'pg-test-controller-retry', 60000
           )",
    )
    .expect("second retried exact-first plan should claim")
    .expect("second retried claim should return token");
    assert_eq!(
        Spi::get_one_with_args::<bool>(
            "SELECT pgcontext.fail_exact_first_build(
                 'p14_controller', 2, $1, 'injected_failure'
             )",
            &[retry_token.into()],
        )
        .expect("fenced exact-first failure should succeed"),
        Some(true)
    );
    let progress = Spi::get_two::<String, String>(
        "SELECT plan_status, error_code
           FROM pgcontext.exact_first_progress('p14_controller')",
    )
    .expect("exact-first progress should report failure");
    assert_eq!(progress.0.as_deref(), Some("failed"));
    assert_eq!(progress.1.as_deref(), Some("injected_failure"));
    let dump_contract = Spi::get_two::<bool, bool>(
        "SELECT
             'pgcontext._exact_first_plans'::regclass = ANY(extension.extconfig),
             'pgcontext._exact_first_plan_jobs'::regclass = ANY(extension.extconfig)
           FROM pg_catalog.pg_extension AS extension
          WHERE extension.extname = 'pgcontext'",
    )
    .expect("exact-first dump contract should load");
    assert_eq!(dump_contract.0, Some(true));
    assert_eq!(dump_contract.1, Some(false));
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT EXISTS (
                 SELECT 1
                   FROM pg_catalog.pg_attribute
                  WHERE attrelid = 'pgcontext._exact_first_plans'::regclass
                    AND attname = 'build_job_id'
                    AND NOT attisdropped
             )"
        )
        .expect("exact-first plan layout should load"),
        Some(false)
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM pgcontext._exact_first_plan_jobs")
            .expect("transient exact-first job links should load"),
        Some(1)
    );
}

#[pg_test]
fn exact_first_rejects_unreleased_mutating_apply_policies() {
    Spi::run(
        "CREATE TABLE public.p14_policy (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL
         )",
    )
    .expect("exact-first policy fixture should be created");
    assert_sql_failure(
        "SELECT * FROM pgcontext.register_exact_first(
             'p14_policy', 'public.p14_policy',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                     'dimensions', 2, 'metric', 'l2'
                 ))
             ),
             'enqueue'
         )",
        "0A000",
        "exact-first optimization application is not enabled before the P14 supervised-build gate",
        "exact-first unreleased apply policy",
    );
}

#[pg_test]
fn exact_first_rejects_registration_without_a_complete_public_exact_adapter() {
    Spi::run(
        "CREATE TABLE public.p14_unsupported_adapter (
             id bigint PRIMARY KEY,
             body text NOT NULL
         )",
    )
    .expect("exact-first adapter fixture should be created");
    assert_sql_failure(
        "SELECT * FROM pgcontext.register_exact_first(
             'p14_unsupported_adapter', 'public.p14_unsupported_adapter',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(jsonb_build_object(
                     'name', 'body', 'column', 'body', 'kind', 'lexical',
                     'text_configuration', 'pg_catalog.english'
                 ))
             )
         )",
        "0A000",
        "exact-first registration requires at least one dense vector binding with a complete public exact adapter",
        "exact-first incomplete adapter",
    );
}

#[pg_test]
fn exact_first_rejects_uncertified_search_bindings_even_with_dense_fallback() {
    Spi::run(
        "CREATE TABLE public.p14_mixed_unsupported_adapter (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             body text NOT NULL
         )",
    )
    .expect("mixed exact-first adapter fixture should be created");
    assert_sql_failure(
        "SELECT * FROM pgcontext.register_exact_first(
             'p14_mixed_unsupported_adapter',
             'public.p14_mixed_unsupported_adapter',
             jsonb_build_object(
                 'version', 'exact_first_registration_v1',
                 'key_column', 'id',
                 'bindings', jsonb_build_array(
                     jsonb_build_object(
                         'name', 'embedding', 'column', 'embedding',
                         'kind', 'dense', 'dimensions', 2, 'metric', 'l2'
                     ),
                     jsonb_build_object(
                         'name', 'body', 'column', 'body', 'kind', 'lexical',
                         'text_configuration', 'pg_catalog.simple'
                     )
                 )
             )
         )",
        "0A000",
        "exact-first registration currently supports dense, filter, and payload bindings",
        "exact-first uncertified mixed adapter",
    );
}

#[pg_test]
fn exact_first_json_admission_rejects_excessive_depth_before_serde() {
    Spi::run("CREATE TABLE public.p14_json_depth (id bigint PRIMARY KEY)")
        .expect("exact-first JSON-depth fixture should be created");
    assert_sql_failure(
        "WITH RECURSIVE nested(depth, value) AS (
             SELECT 0, '{}'::jsonb
             UNION ALL
             SELECT depth + 1, jsonb_build_object('nested', value)
               FROM nested
              WHERE depth < 65
         )
         SELECT *
           FROM pgcontext.inspect_exact_first_source(
                'public.p14_json_depth',
                (SELECT value FROM nested WHERE depth = 65)
           )",
        "54000",
        "exact-first objectives exceed the JSON allocation budget",
        "exact-first JSON depth admission",
    );
}
