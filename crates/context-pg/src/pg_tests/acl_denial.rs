#[pg_test]
fn create_collection_denies_source_tables_without_select() {
    acl_create_role("m2_acl_table_owner_a");
    acl_create_role("m2_acl_denied_create");
    acl_grant_api_access("m2_acl_table_owner_a");
    acl_grant_api_access("m2_acl_denied_create");

    acl_set_session_user("m2_acl_table_owner_a");
    acl_create_source_table("m2_acl_source_denied");
    acl_reset_session_user();

    acl_set_session_user("m2_acl_denied_create");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.create_collection('m2_acl_denied_create', 'public.m2_acl_source_denied')",
        "permission denied for source table: public.m2_acl_source_denied",
    );
    acl_reset_session_user();
}

#[pg_test]
fn register_vector_denies_non_owner_collections() {
    acl_create_role("m2_acl_collection_owner_a");
    acl_create_role("m2_acl_denied_register");
    acl_grant_api_access("m2_acl_collection_owner_a");
    acl_grant_api_access("m2_acl_denied_register");

    acl_set_session_user("m2_acl_collection_owner_a");
    acl_create_source_table("m2_acl_non_owner_vector");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'm2_acl_non_owner_vector',
             'public.m2_acl_non_owner_vector'
         )",
    )
    .expect("collection owner should create collection");
    acl_reset_session_user();

    Spi::run("GRANT SELECT ON public.m2_acl_non_owner_vector TO m2_acl_denied_register")
        .expect("denied role should receive source-table select");

    acl_set_session_user("m2_acl_denied_register");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.register_vector(
             'm2_acl_non_owner_vector',
             'embedding',
             'embedding',
             3,
             'l2'
         )",
        "permission denied for collection m2_acl_non_owner_vector: owner is m2_acl_collection_owner_a",
    );
    acl_reset_session_user();
}

#[pg_test]
fn register_vector_denies_revoked_source_table_select() {
    acl_create_role("m2_acl_table_owner_b");
    acl_create_role("m2_acl_collection_owner_b");
    acl_grant_api_access("m2_acl_table_owner_b");
    acl_grant_api_access("m2_acl_collection_owner_b");

    acl_set_session_user("m2_acl_table_owner_b");
    acl_create_source_table("m2_acl_revoke_vector");
    acl_reset_session_user();

    Spi::run("GRANT SELECT ON public.m2_acl_revoke_vector TO m2_acl_collection_owner_b")
        .expect("collection owner should receive source-table select");

    acl_set_session_user("m2_acl_collection_owner_b");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'm2_acl_revoke_vector',
             'public.m2_acl_revoke_vector'
         )",
    )
    .expect("collection should be created while SELECT is granted");
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.m2_acl_revoke_vector FROM m2_acl_collection_owner_b")
        .expect("source-table select should be revoked");

    acl_set_session_user("m2_acl_collection_owner_b");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.register_vector(
             'm2_acl_revoke_vector',
             'embedding',
             'embedding',
             3,
             'l2'
         )",
        "permission denied for source table: public.m2_acl_revoke_vector",
    );
    acl_reset_session_user();
}

#[pg_test]
fn point_mutations_deny_non_owner_collections() {
    acl_create_role("m2_acl_collection_owner_c");
    acl_create_role("m2_acl_denied_points");
    acl_grant_api_access("m2_acl_collection_owner_c");
    acl_grant_api_access("m2_acl_denied_points");

    acl_set_session_user("m2_acl_collection_owner_c");
    acl_create_source_table("m2_acl_point_owner");
    Spi::run("SELECT pgcontext.create_collection('m2_acl_point_owner', 'public.m2_acl_point_owner')")
        .expect("collection owner should create collection");
    Spi::run("SELECT pgcontext.upsert_points('m2_acl_point_owner', ARRAY['doc-1'])")
        .expect("collection owner should upsert a point");
    acl_reset_session_user();

    acl_set_session_user("m2_acl_denied_points");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.upsert_points('m2_acl_point_owner', ARRAY['doc-2'])",
        "permission denied for collection m2_acl_point_owner",
    );
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.delete_points('m2_acl_point_owner', ARRAY['doc-1'])",
        "permission denied for collection m2_acl_point_owner",
    );
    acl_reset_session_user();
}

#[pg_test]
fn facet_denies_non_owner_collections() {
    acl_create_role("m5_acl_facet_owner_a");
    acl_create_role("m5_acl_facet_denied_a");
    acl_grant_api_access("m5_acl_facet_owner_a");
    acl_grant_api_access("m5_acl_facet_denied_a");

    acl_set_session_user("m5_acl_facet_owner_a");
    acl_create_facet_source_table("m5_acl_facet_owner");
    Spi::run("SELECT pgcontext.create_collection('m5_acl_facet_owner', 'public.m5_acl_facet_owner')")
        .expect("collection owner should create collection");
    Spi::run("SELECT pgcontext.register_vector('m5_acl_facet_owner', 'embedding', 'embedding', 3, 'l2')")
        .expect("collection owner should register vector");
    Spi::run("SELECT pgcontext.register_filter_column('m5_acl_facet_owner', 'status', 'status')")
        .expect("collection owner should register filter column");
    Spi::run("SELECT pgcontext.upsert_points('m5_acl_facet_owner', ARRAY['1'])")
        .expect("collection owner should upsert points");
    acl_reset_session_user();

    Spi::run("GRANT SELECT ON public.m5_acl_facet_owner TO m5_acl_facet_denied_a")
        .expect("denied role should receive source-table select");

    acl_set_session_user("m5_acl_facet_denied_a");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.facet('m5_acl_facet_owner', 'status', NULL, 10)",
        "permission denied for collection m5_acl_facet_owner",
    );
    acl_reset_session_user();
}

#[pg_test]
fn facet_denies_revoked_source_table_select() {
    acl_create_role("m5_acl_facet_owner_b");
    acl_grant_api_access("m5_acl_facet_owner_b");

    acl_set_session_user("m5_acl_facet_owner_b");
    acl_create_facet_source_table("m5_acl_facet_revoke");
    Spi::run(
        "SELECT pgcontext.create_collection('m5_acl_facet_revoke', 'public.m5_acl_facet_revoke')",
    )
    .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector('m5_acl_facet_revoke', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("collection owner should register vector");
    Spi::run("SELECT pgcontext.register_filter_column('m5_acl_facet_revoke', 'status', 'status')")
        .expect("collection owner should register filter column");
    Spi::run("SELECT pgcontext.upsert_points('m5_acl_facet_revoke', ARRAY['1'])")
        .expect("collection owner should upsert points");
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.m5_acl_facet_revoke FROM m5_acl_facet_owner_b")
        .expect("source-table select should be revoked");

    acl_set_session_user("m5_acl_facet_owner_b");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.facet('m5_acl_facet_revoke', 'status', NULL, 10)",
        "permission denied for source table: public.m5_acl_facet_revoke",
    );
    acl_reset_session_user();
}

#[pg_test]
fn count_denies_non_owner_collections() {
    acl_create_role("m5_acl_count_owner_a");
    acl_create_role("m5_acl_count_denied_a");
    acl_grant_api_access("m5_acl_count_owner_a");
    acl_grant_api_access("m5_acl_count_denied_a");

    acl_set_session_user("m5_acl_count_owner_a");
    acl_create_facet_source_table("m5_acl_count_owner");
    Spi::run("SELECT pgcontext.create_collection('m5_acl_count_owner', 'public.m5_acl_count_owner')")
        .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector('m5_acl_count_owner', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("collection owner should register vector");
    Spi::run("SELECT pgcontext.upsert_points('m5_acl_count_owner', ARRAY['1'])")
        .expect("collection owner should upsert points");
    acl_reset_session_user();

    Spi::run("GRANT SELECT ON public.m5_acl_count_owner TO m5_acl_count_denied_a")
        .expect("denied role should receive source-table select");

    acl_set_session_user("m5_acl_count_denied_a");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.count('m5_acl_count_owner')",
        "permission denied for collection m5_acl_count_owner",
    );
    acl_reset_session_user();
}

#[pg_test]
fn count_denies_revoked_source_table_select() {
    acl_create_role("m5_acl_count_owner_b");
    acl_grant_api_access("m5_acl_count_owner_b");

    acl_set_session_user("m5_acl_count_owner_b");
    acl_create_facet_source_table("m5_acl_count_revoke");
    Spi::run(
        "SELECT pgcontext.create_collection('m5_acl_count_revoke', 'public.m5_acl_count_revoke')",
    )
    .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector('m5_acl_count_revoke', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("collection owner should register vector");
    Spi::run("SELECT pgcontext.upsert_points('m5_acl_count_revoke', ARRAY['1'])")
        .expect("collection owner should upsert points");
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.m5_acl_count_revoke FROM m5_acl_count_owner_b")
        .expect("source-table select should be revoked");

    acl_set_session_user("m5_acl_count_owner_b");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.count('m5_acl_count_revoke')",
        "permission denied for source table: public.m5_acl_count_revoke",
    );
    acl_reset_session_user();
}

#[pg_test]
fn candidate_recheck_denies_non_owner_collections() {
    acl_create_role("m8_acl_recheck_owner_a");
    acl_create_role("m8_acl_recheck_denied_a");
    acl_grant_api_access("m8_acl_recheck_owner_a");
    acl_grant_api_access("m8_acl_recheck_denied_a");

    acl_set_session_user("m8_acl_recheck_owner_a");
    acl_create_source_table("m8_acl_recheck_owner");
    Spi::run(
        "SELECT pgcontext.create_collection('m8_acl_recheck_owner', 'public.m8_acl_recheck_owner')",
    )
    .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector('m8_acl_recheck_owner', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("collection owner should register vector");
    acl_reset_session_user();

    Spi::run("GRANT SELECT ON public.m8_acl_recheck_owner TO m8_acl_recheck_denied_a")
        .expect("denied role should receive source-table select");

    acl_set_session_user("m8_acl_recheck_denied_a");
    acl_expect_insufficient_privilege(
        "SELECT * FROM pgcontext.search(
             'm8_acl_recheck_owner',
             '[0,0,0]'::vector,
             ARRAY[1]::bigint[],
             10
         )",
        "permission denied for collection m8_acl_recheck_owner",
    );
    acl_reset_session_user();
}

#[pg_test]
fn candidate_recheck_denies_revoked_source_table_select() {
    acl_create_role("m8_acl_recheck_owner_b");
    acl_grant_api_access("m8_acl_recheck_owner_b");

    acl_set_session_user("m8_acl_recheck_owner_b");
    acl_create_source_table("m8_acl_recheck_revoke");
    Spi::run(
        "SELECT pgcontext.create_collection('m8_acl_recheck_revoke', 'public.m8_acl_recheck_revoke')",
    )
    .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector('m8_acl_recheck_revoke', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("collection owner should register vector");
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.m8_acl_recheck_revoke FROM m8_acl_recheck_owner_b")
        .expect("source-table select should be revoked");

    acl_set_session_user("m8_acl_recheck_owner_b");
    acl_expect_insufficient_privilege(
        "SELECT * FROM pgcontext.search(
             'm8_acl_recheck_revoke',
             '[0,0,0]'::vector,
             ARRAY[1]::bigint[],
             10
         )",
        "permission denied for source table: public.m8_acl_recheck_revoke",
    );
    acl_reset_session_user();
}

#[pg_test]
fn search_respects_source_table_rls_policy() {
    acl_create_role("m8_rls_acme");
    acl_grant_api_access("m8_rls_acme");

    acl_set_session_user("m8_rls_acme");
    Spi::run(
        "CREATE TABLE public.m8_rls_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL
         )",
    )
    .expect("RLS source table should be created");
    Spi::run(
        "INSERT INTO public.m8_rls_docs (id, embedding, tenant_id)
         VALUES (1, '[1,0]'::vector, 'm8_rls_acme'),
                (2, '[0.5,0]'::vector, 'other')",
    )
    .expect("RLS source rows should be inserted");
    Spi::run("ALTER TABLE public.m8_rls_docs ENABLE ROW LEVEL SECURITY")
        .expect("RLS should be enabled");
    Spi::run("ALTER TABLE public.m8_rls_docs FORCE ROW LEVEL SECURITY")
        .expect("RLS should be forced");
    Spi::run(
        "CREATE POLICY m8_rls_docs_tenant
            ON public.m8_rls_docs
         USING (tenant_id = SESSION_USER)",
    )
    .expect("tenant RLS policy should be created");
    Spi::run("SELECT pgcontext.create_collection('m8_rls_docs', 'public.m8_rls_docs')")
        .expect("RLS collection should be created");
    Spi::run("SELECT pgcontext.register_vector('m8_rls_docs', 'embedding', 'embedding', 2, 'l2')")
        .expect("RLS vector should be registered");
    Spi::run("SELECT pgcontext.register_filter_column('m8_rls_docs', 'tenant_id', 'tenant_id')")
        .expect("RLS tenant filter should be registered");
    Spi::run("SELECT pgcontext.upsert_points('m8_rls_docs', ARRAY['1', '2'])")
        .expect("RLS points should be upserted");

    let visible_sources = Spi::get_one::<String>(
        "SELECT coalesce(string_agg(source_key, ',' ORDER BY source_key), '')
           FROM pgcontext.search('m8_rls_docs', '[0,0]'::vector, 10)",
    )
    .expect("RLS search should execute")
    .unwrap_or_default();

    assert_eq!(visible_sources, "1");
    acl_reset_session_user();
}

#[pg_test]
fn search_preserves_split_owner_source_table_rls_policy() {
    acl_create_role("m8_rls_table_owner");
    acl_create_role("m8_rls_collection_owner");
    acl_create_role("m8_rls_denied");
    acl_grant_api_access("m8_rls_table_owner");
    acl_grant_api_access("m8_rls_collection_owner");
    acl_grant_api_access("m8_rls_denied");

    acl_set_session_user("m8_rls_table_owner");
    Spi::run(
        "CREATE TABLE public.m8_rls_split_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant text NOT NULL
         )",
    )
    .expect("split-owner RLS table should be created");
    Spi::run(
        "INSERT INTO public.m8_rls_split_docs (id, embedding, tenant)
         VALUES (1, '[0,0]'::vector, 'acme'),
                (2, '[1,0]'::vector, 'acme'),
                (3, '[0,0]'::vector, 'other'),
                (4, '[1,0]'::vector, 'other')",
    )
    .expect("split-owner RLS rows should be inserted");
    Spi::run("ALTER TABLE public.m8_rls_split_docs ENABLE ROW LEVEL SECURITY")
        .expect("RLS should be enabled");
    Spi::run("ALTER TABLE public.m8_rls_split_docs FORCE ROW LEVEL SECURITY")
        .expect("RLS should be forced");
    Spi::run(
        "CREATE POLICY m8_rls_split_docs_tenant
            ON public.m8_rls_split_docs
         USING (tenant = current_setting('pgcontext_test.tenant', true))
         WITH CHECK (tenant = current_setting('pgcontext_test.tenant', true))",
    )
    .expect("tenant RLS policy should be created");
    Spi::run(
        "GRANT SELECT ON public.m8_rls_split_docs
            TO m8_rls_collection_owner, m8_rls_denied",
    )
    .expect("source table SELECT should be granted");
    acl_reset_session_user();

    acl_set_session_user("m8_rls_collection_owner");
    Spi::run("SELECT pgcontext.create_collection('m8_rls_split_docs', 'public.m8_rls_split_docs')")
        .expect("collection owner should create split-owner collection");
    Spi::run("SELECT pgcontext.register_vector('m8_rls_split_docs', 'embedding', 'embedding', 2, 'l2')")
        .expect("collection owner should register split-owner vector");
    Spi::run("SELECT pgcontext.register_filter_column('m8_rls_split_docs', 'tenant', 'tenant')")
        .expect("collection owner should register split-owner tenant filter");
    Spi::run("SELECT pgcontext.upsert_points('m8_rls_split_docs', ARRAY['1', '2', '3', '4'])")
        .expect("collection owner should upsert split-owner points");

    let visible_sources = Spi::get_one::<String>(
        "SET pgcontext_test.tenant = 'acme';
         SELECT coalesce(string_agg(source_key, ',' ORDER BY source_key), '')
           FROM pgcontext.search('m8_rls_split_docs', '[0,0]'::vector, 10)",
    )
    .expect("split-owner RLS search should execute")
    .expect("split-owner visible source aggregate should not be null");
    assert_eq!(visible_sources, "1,2");

    let filtered_count = Spi::get_one::<i64>(
        "SET pgcontext_test.tenant = 'acme';
         SELECT count(*)
           FROM pgcontext.search(
               'm8_rls_split_docs',
               '[0,0]'::vector,
               '{\"must\":[{\"key\":\"tenant\",\"match\":\"other\"}]}',
               10
           )",
    )
    .expect("split-owner filtered RLS search should execute")
    .expect("split-owner filtered count should not be null");
    assert_eq!(filtered_count, 0);
    acl_reset_session_user();

    acl_set_session_user("m8_rls_denied");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.search('m8_rls_split_docs', '[0,0]'::vector, 1)",
        "permission denied for collection m8_rls_split_docs",
    );
    acl_reset_session_user();

    Spi::run("REVOKE SELECT ON public.m8_rls_split_docs FROM m8_rls_collection_owner")
        .expect("source-table SELECT should be revoked");

    acl_set_session_user("m8_rls_collection_owner");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.search('m8_rls_split_docs', '[0,0]'::vector, 1)",
        "permission denied for source table: public.m8_rls_split_docs",
    );
    acl_reset_session_user();
}

#[pg_test]
fn search_limit_policy_does_not_require_private_catalog_table_grants() {
    acl_create_role("m12_acl_limits_owner");
    acl_grant_api_access("m12_acl_limits_owner");

    acl_set_session_user("m12_acl_limits_owner");
    Spi::run(
        "CREATE TABLE public.m12_acl_limits_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )",
    )
    .expect("limit search source table should be created");
    Spi::run(
        "INSERT INTO public.m12_acl_limits_docs (id, embedding)
         VALUES (1, '[0,0]'::vector), (2, '[1,0]'::vector)",
    )
    .expect("limit search rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'm12_acl_limits_docs',
             'public.m12_acl_limits_docs'
         )",
    )
    .expect("collection owner should create collection");
    Spi::run(
        "SELECT pgcontext.register_vector(
             'm12_acl_limits_docs',
             'embedding',
             'embedding',
             2,
             'l2'
         )",
    )
    .expect("collection owner should register vector");
    Spi::run("SELECT pgcontext.upsert_points('m12_acl_limits_docs', ARRAY['1', '2'])")
        .expect("collection owner should upsert points");
    Spi::run(
        "SELECT pgcontext.configure_collection_limits(
             'm12_acl_limits_docs',
             true,
             NULL,
             NULL,
             NULL,
             NULL,
             1,
             NULL,
             NULL,
             NULL
         )",
    )
    .expect("collection owner should configure strict search limit");

    assert!(
        !acl_has_table_privilege("pgcontext._collections", "SELECT"),
        "collection owners must not need direct _collections SELECT"
    );
    assert!(
        !acl_has_table_privilege("pgcontext._collection_points", "SELECT"),
        "collection owners must not need direct _collection_points SELECT"
    );

    let visible = Spi::get_one::<String>(
        "SELECT coalesce(string_agg(source_key, ',' ORDER BY source_key), '')
           FROM pgcontext.search('m12_acl_limits_docs', '[0,0]'::vector, 1)",
    )
    .expect("strict in-budget search should execute without private catalog grants")
    .expect("search aggregate should not be null");
    assert_eq!(visible, "1");

    acl_expect_sqlstate(
        "SELECT pgcontext.search('m12_acl_limits_docs', '[0,0]'::vector, 2)",
        "program_limit_exceeded",
        "collection m12_acl_limits_docs max_search_limit 1 exceeded: 2",
    );
    acl_reset_session_user();
}

#[pg_test]
fn hybrid_query_does_not_require_private_catalog_table_grants() {
    acl_create_role("m5_acl_query_owner");
    acl_grant_api_access("m5_acl_query_owner");

    acl_set_session_user("m5_acl_query_owner");
    create_hybrid_collection("m5_acl_query_docs");
    upsert_search_points("m5_acl_query_docs", &["10", "20", "30", "40"]);

    // A collection owner in production is a plain (non-superuser) role that
    // holds no direct privilege on the private catalog tables — only the
    // PUBLIC visibility views. `pgcontext.query` must resolve the collection
    // through those views, exactly as `pgcontext.search` does.
    assert!(
        !acl_has_table_privilege("pgcontext._collections", "SELECT"),
        "collection owners must not need direct _collections SELECT"
    );
    assert!(
        !acl_has_table_privilege("pgcontext._collection_points", "SELECT"),
        "collection owners must not need direct _collection_points SELECT"
    );

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_acl_query_docs',
                '[0,0]'::vector,
                'database',
                'body',
                4
           )",
    );
    let source_keys = rows
        .into_iter()
        .map(|(_point_id, source_key, _score)| source_key)
        .collect::<Vec<_>>();
    assert_eq!(
        source_keys,
        vec![
            "20".to_owned(),
            "10".to_owned(),
            "30".to_owned(),
            "40".to_owned()
        ],
        "owner without private catalog grants should get fused hybrid results"
    );

    acl_reset_session_user();
}

#[pg_test]
fn hybrid_query_refreshes_drifted_source_table_without_catalog_writes() {
    acl_create_role("m5_acl_drift_owner");
    acl_grant_api_access("m5_acl_drift_owner");

    acl_set_session_user("m5_acl_drift_owner");
    create_hybrid_collection("m5_acl_drift_docs");
    upsert_search_points("m5_acl_drift_docs", &["10", "20", "30", "40"]);

    // Simulate a dump/restore: rebuild the source table so its oid changes.
    // The next query detects the drift and must persist the refreshed oid, but
    // a plain collection owner holds no direct write privilege on the private
    // catalog tables. The refresh therefore has to run through the SECURITY
    // DEFINER helpers rather than a direct UPDATE.
    Spi::run("DROP TABLE public.m5_acl_drift_docs").expect("source table should drop");
    Spi::run(
        "CREATE TABLE public.m5_acl_drift_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL
         )",
    )
    .expect("source table should be recreated");
    Spi::run(
        "INSERT INTO public.m5_acl_drift_docs (id, embedding, body)
         VALUES (10, '[1,0]'::vector, 'database internals'),
                (20, '[0,0]'::vector, 'database database'),
                (30, '[2,0]'::vector, 'storage database'),
                (40, '[3,0]'::vector, 'unrelated')",
    )
    .expect("source rows should be reinserted");

    assert!(
        !acl_has_table_privilege("pgcontext._collections", "UPDATE"),
        "collection owners must not need direct _collections UPDATE"
    );
    assert!(
        !acl_has_table_privilege("pgcontext._collection_vectors", "UPDATE"),
        "collection owners must not need direct _collection_vectors UPDATE"
    );

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_acl_drift_docs',
                '[0,0]'::vector,
                'database',
                'body',
                4
           )",
    );
    assert_eq!(
        rows.len(),
        4,
        "drift refresh should self-heal through SECURITY DEFINER helpers and return results"
    );

    acl_reset_session_user();
}

#[pg_test]
fn late_interaction_search_does_not_require_private_catalog_table_grants() {
    acl_create_role("m14_acl_late_owner");
    acl_grant_api_access("m14_acl_late_owner");

    acl_set_session_user("m14_acl_late_owner");
    create_late_interaction_collection("m14_acl_late_docs");
    upsert_hybrid_points("m14_acl_late_docs", &["10", "20", "30", "40"]);

    // The late-interaction resolve needs collection-level source metadata. A
    // plain owner must reach it through the membership-filtered
    // `_visible_collections` view, never the base `_collections` table.
    assert!(
        !acl_has_table_privilege("pgcontext._collections", "SELECT"),
        "collection owners must not need direct _collections SELECT"
    );

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction(
                'm14_acl_late_docs',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                3
           )",
    );
    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("10".to_owned(), 2.0),
            ("20".to_owned(), 1.5),
            ("30".to_owned(), 1.0),
        ],
        "late-interaction search should work for a member without private catalog grants"
    );

    acl_reset_session_user();
}

#[pg_test]
fn automatic_query_stats_are_written_and_visible_through_membership_views() {
    acl_create_role("m15_stats_owner");
    acl_create_role("m15_stats_member");
    acl_create_role("m15_stats_outsider");
    acl_grant_api_access("m15_stats_owner");
    acl_grant_api_access("m15_stats_member");
    acl_grant_api_access("m15_stats_outsider");
    Spi::run("GRANT m15_stats_owner TO m15_stats_member")
        .expect("member should receive the collection owner role");

    acl_set_session_user("m15_stats_owner");
    Spi::run(
        "CREATE TABLE public.m15_stats_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         );
         INSERT INTO public.m15_stats_docs VALUES
             (1, '[1,0]'::vector),
             (2, '[0,1]'::vector);
         SELECT pgcontext.create_collection(
             'm15_stats_docs', 'public.m15_stats_docs'
         );
         SELECT pgcontext.register_vector(
             'm15_stats_docs', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('m15_stats_docs', 100);
         CREATE INDEX m15_stats_docs_hnsw
             ON public.m15_stats_docs
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'm15_stats_docs', 'embedding', 'public.m15_stats_docs_hnsw'
         );",
    )
    .expect("member-visible collection should be created");
    acl_reset_session_user();

    acl_set_session_user("m15_stats_member");
    assert!(
        !acl_has_table_privilege("pgcontext._query_stats", "SELECT"),
        "collection members must not need direct _query_stats SELECT"
    );
    let result_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.execute_query(
               'm15_stats_docs',
               pgcontext.query_nearest('[1,0]'::vector, 1)
           )",
    )
    .expect("member query should execute")
    .expect("member query count should not be null");
    assert_eq!(result_count, 1);
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("member collection lookup should execute")
    .expect("member collection should be visible");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].strategy, "dense_hnsw");
    assert_eq!(events[0].completion, "complete");
    acl_reset_session_user();

    // The pgrx wrapper suite runs each fixture in one uncommitted transaction,
    // so its test build captures async events backend-locally. Seed the private
    // sink as the extension owner to verify the production visibility boundary;
    // the live heavy gate verifies the worker performs this insert itself.
    Spi::run_with_args(
        "INSERT INTO pgcontext._query_stats (
             collection_id, cohort, query_kind, result_count, candidate_count,
             rows_rechecked, rows_pruned, latency_bucket, lifecycle_state,
             latency_ms, strategy, visits, filter_candidates, candidates,
             rechecks, stages, expansions, completion
         ) VALUES (
             $1, 'automatic', 'search', 1, 1, 1, 0, 'Lt1Ms', 'Indexed',
             0.1, 'dense_hnsw', 2, 0, 1, 1, 2, 1, 'complete'
         )",
        &[collection_id.into()],
    )
    .expect("extension owner should seed the visibility fixture");

    acl_set_session_user("m15_stats_member");
    let visible_count = Spi::get_one::<i64>(
        "SELECT COALESCE(sum(query_count), 0)::bigint
           FROM pgcontext.query_execution_stats()
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("member telemetry query should execute")
    .expect("member telemetry count should not be null");
    assert_eq!(visible_count, 1);
    acl_reset_session_user();

    acl_set_session_user("m15_stats_outsider");
    let outsider_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.query_execution_stats()
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("outsider telemetry query should execute")
    .expect("outsider telemetry count should not be null");
    assert_eq!(outsider_count, 0);
    acl_reset_session_user();
}

#[pg_test]
fn query_telemetry_queue_health_requires_pg_monitor() {
    acl_create_role("m15_queue_health_denied");
    acl_grant_api_access("m15_queue_health_denied");
    acl_set_session_user("m15_queue_health_denied");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.query_telemetry_queue_stats()",
        "42501",
        "query telemetry queue health requires membership in pg_monitor",
        "automatic telemetry queue health privilege",
    );
    acl_reset_session_user();
}

#[pg_test]
fn refresh_helpers_cannot_repoint_collection_to_caller_controlled_table() {
    acl_create_role("m5_acl_defkey_owner");
    acl_create_role("m5_acl_defkey_member");
    acl_grant_api_access("m5_acl_defkey_owner");
    acl_grant_api_access("m5_acl_defkey_member");
    // The member is a member of the owner role, so it passes the definer
    // helper's ownership gate.
    Spi::run("GRANT m5_acl_defkey_owner TO m5_acl_defkey_member")
        .expect("member should be granted the owner role");

    acl_set_session_user("m5_acl_defkey_owner");
    create_hybrid_collection("m5_acl_defkey_docs");
    upsert_search_points("m5_acl_defkey_docs", &["10", "20"]);
    acl_reset_session_user();

    acl_set_session_user("m5_acl_defkey_member");
    Spi::run(
        "CREATE TABLE public.m5_acl_defkey_scratch (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL
         )",
    )
    .expect("member scratch table should be created");
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'm5_acl_defkey_docs'",
    )
    .expect("acl lookup should succeed")
    .expect("collection id should exist");
    // The definer helpers are PUBLIC-executable, but they accept no oid and
    // re-derive the registered source table, so the member cannot point the
    // collection at the scratch table they control (confused-deputy vector).
    Spi::run(&format!(
        "SELECT pgcontext._refresh_collection_source_table({collection_id})"
    ))
    .expect("member refresh call should succeed");
    Spi::run(&format!(
        "SELECT pgcontext._refresh_vector_source_binding({collection_id}, 'embedding')"
    ))
    .expect("member vector refresh call should succeed");
    acl_reset_session_user();

    let pinned_to_registered_table = Spi::get_one::<bool>(
        "SELECT source_table_oid = 'public.m5_acl_defkey_docs'::regclass
            AND source_table_oid <> 'public.m5_acl_defkey_scratch'::regclass
           FROM pgcontext._collections
          WHERE collection_name = 'm5_acl_defkey_docs'",
    )
    .expect("collection oid lookup should succeed")
    .expect("collection row should exist");
    assert!(
        pinned_to_registered_table,
        "refresh helpers must keep source_table_oid pinned to the registered table"
    );
}

#[pg_test]
fn search_refreshes_drifted_source_table_without_catalog_writes() {
    acl_create_role("m5_acl_search_drift_owner");
    acl_grant_api_access("m5_acl_search_drift_owner");

    acl_set_session_user("m5_acl_search_drift_owner");
    create_hybrid_collection("m5_acl_search_drift_docs");
    upsert_search_points("m5_acl_search_drift_docs", &["10", "20", "30", "40"]);

    Spi::run("DROP TABLE public.m5_acl_search_drift_docs").expect("source table should drop");
    Spi::run(
        "CREATE TABLE public.m5_acl_search_drift_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL
         )",
    )
    .expect("source table should be recreated");
    Spi::run(
        "INSERT INTO public.m5_acl_search_drift_docs (id, embedding, body)
         VALUES (10, '[1,0]'::vector, 'a'),
                (20, '[0,0]'::vector, 'b'),
                (30, '[2,0]'::vector, 'c'),
                (40, '[3,0]'::vector, 'd')",
    )
    .expect("source rows should be reinserted");

    assert!(
        !acl_has_table_privilege("pgcontext._collections", "UPDATE"),
        "collection owners must not need direct _collections UPDATE"
    );

    let count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.search('m5_acl_search_drift_docs', '[0,0]'::vector, 4)",
    )
    .expect("search should execute")
    .expect("search count should not be null");
    assert_eq!(
        count, 4,
        "search should self-heal source-table drift through SECURITY DEFINER helpers"
    );

    acl_reset_session_user();
}

#[pg_test]
fn sparse_search_refreshes_drifted_source_table_without_catalog_writes() {
    acl_create_role("m5_acl_sparse_drift_owner");
    acl_grant_api_access("m5_acl_sparse_drift_owner");

    acl_set_session_user("m5_acl_sparse_drift_owner");
    create_dense_sparse_collection("m5_acl_sparse_drift_docs");
    upsert_hybrid_points("m5_acl_sparse_drift_docs", &["10", "20", "30"]);

    Spi::run("DROP TABLE public.m5_acl_sparse_drift_docs").expect("source table should drop");
    Spi::run(
        "CREATE TABLE public.m5_acl_sparse_drift_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             lexical sparsevec NOT NULL,
             body text NOT NULL
         )",
    )
    .expect("source table should be recreated");
    Spi::run(
        "INSERT INTO public.m5_acl_sparse_drift_docs (id, embedding, lexical, body)
         VALUES (10, '[0,0]'::vector, pgcontext.sparsevec('{1:1}/4'), 'first'),
                (20, '[3,0]'::vector, pgcontext.sparsevec('{1:3}/4'), 'second'),
                (30, '[2,0]'::vector, pgcontext.sparsevec('{1:2}/4'), 'third')",
    )
    .expect("source rows should be reinserted");

    assert!(
        !acl_has_table_privilege("pgcontext._collection_sparse_vectors", "UPDATE"),
        "collection owners must not need direct _collection_sparse_vectors UPDATE"
    );

    let count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.search_sparse(
                'm5_acl_sparse_drift_docs',
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    )
    .expect("sparse search should execute")
    .expect("sparse search count should not be null");
    assert!(
        count >= 1,
        "sparse search should self-heal source-table drift through SECURITY DEFINER helpers"
    );

    acl_reset_session_user();
}

#[pg_test]
fn sparse_ann_preserves_non_superuser_acl_and_source_rls() {
    acl_create_role("m5_acl_sparse_ann_owner");
    acl_grant_api_access("m5_acl_sparse_ann_owner");

    acl_set_session_user("m5_acl_sparse_ann_owner");
    Spi::run(
        "CREATE TABLE public.m5_acl_sparse_ann_docs (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.m5_acl_sparse_ann_docs (id, lexical, tenant)
         SELECT value,
                pg_catalog.format('{1:%s}/4', value)::sparsevec,
                'other'
           FROM generate_series(1, 64) AS value;
         INSERT INTO public.m5_acl_sparse_ann_docs (id, lexical, tenant)
         VALUES (100, pgcontext.sparsevec('{1:100}/4'), 'm5_acl_sparse_ann_owner');
         ALTER TABLE public.m5_acl_sparse_ann_docs ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.m5_acl_sparse_ann_docs FORCE ROW LEVEL SECURITY;
         CREATE POLICY m5_acl_sparse_ann_tenant
             ON public.m5_acl_sparse_ann_docs
          USING (tenant = SESSION_USER);
         SELECT pgcontext.create_collection(
             'm5_acl_sparse_ann_docs', 'public.m5_acl_sparse_ann_docs'
         );
         SELECT pgcontext.register_sparse_vector(
             'm5_acl_sparse_ann_docs', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'm5_acl_sparse_ann_docs',
             ARRAY(
                 SELECT value::text FROM generate_series(1, 64) AS value
                 UNION ALL SELECT '100'
             )
         );
         CREATE INDEX m5_acl_sparse_ann_docs_hnsw
             ON public.m5_acl_sparse_ann_docs USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm5_acl_sparse_ann_docs', 'lexical',
             'public.m5_acl_sparse_ann_docs_hnsw'
         );
         SET LOCAL pgcontext.hnsw_candidate_budget = 8;",
    )
    .expect("non-superuser sparse ANN fixture should be created");

    assert!(
        !acl_has_table_privilege("pgcontext._collection_sparse_vectors", "SELECT"),
        "sparse ANN callers must not need private sparse catalog SELECT"
    );
    let visible_sources = Spi::get_one::<String>(
        "SELECT coalesce(string_agg(source_key, ',' ORDER BY source_key), '')
           FROM pgcontext.search_sparse(
                'm5_acl_sparse_ann_docs', 'lexical',
                pgcontext.sparsevec('{}/4'), 1
           )",
    )
    .expect("non-superuser sparse ANN should execute")
    .expect("sparse ANN aggregate should not be null");
    assert_eq!(visible_sources, "100");

    acl_reset_session_user();
}

#[pg_test]
fn composite_execute_query_preserves_non_superuser_acl_rls_and_mvcc() {
    acl_create_role("stage_g_composite_rls_owner");
    acl_grant_api_access("stage_g_composite_rls_owner");

    acl_set_session_user("stage_g_composite_rls_owner");
    Spi::run(
        "CREATE TABLE public.stage_g_composite_rls_docs (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.stage_g_composite_rls_docs VALUES
             (1, '[1,0]', 'stage_g_composite_rls_owner'),
             (2, '[0.9,0.1]', 'other'),
             (3, '[0,1]', 'other');
         SELECT pgcontext.create_collection(
             'stage_g_composite_rls_docs', 'public.stage_g_composite_rls_docs'
         );
         SELECT pgcontext.register_vector(
             'stage_g_composite_rls_docs', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'stage_g_composite_rls_docs', ARRAY['1', '2', '3']
         );
         CREATE INDEX stage_g_composite_rls_docs_hnsw
             ON public.stage_g_composite_rls_docs
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'stage_g_composite_rls_docs', 'embedding',
             'public.stage_g_composite_rls_docs_hnsw'
         );
         ALTER TABLE public.stage_g_composite_rls_docs ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.stage_g_composite_rls_docs FORCE ROW LEVEL SECURITY;
         CREATE POLICY stage_g_composite_rls_tenant
             ON public.stage_g_composite_rls_docs
          USING (tenant = SESSION_USER)
          WITH CHECK (tenant = SESSION_USER);",
    )
    .expect("non-superuser composite fixture should be created");

    assert!(
        !acl_has_table_privilege("pgcontext._collection_points", "SELECT"),
        "composite callers must not need private point-catalog SELECT"
    );
    let visible = Spi::get_one::<String>(
        "SELECT coalesce(string_agg(source_key, ',' ORDER BY source_key), '')
           FROM pgcontext.execute_query(
               'stage_g_composite_rls_docs',
               pgcontext.query_nearest('[1,0]'::vector, 3)
           )",
    )
    .expect("non-superuser composite query should execute")
    .expect("visible aggregate should not be null");
    assert_eq!(visible, "1");

    Spi::run(
        "UPDATE public.stage_g_composite_rls_docs
            SET embedding = '[0,1]'::vector
          WHERE id = 1",
    )
    .expect("visible source update should succeed");
    let score = Spi::get_one::<f32>(
        "SELECT score
           FROM pgcontext.execute_query(
               'stage_g_composite_rls_docs',
               pgcontext.query_nearest('[0,1]'::vector, 1)
           )",
    )
    .expect("updated composite query should execute")
    .expect("updated visible row should remain searchable");
    assert_eq!(score, 0.0);

    Spi::run("DELETE FROM public.stage_g_composite_rls_docs WHERE id = 1")
        .expect("visible source delete should succeed");
    let remaining = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.execute_query(
               'stage_g_composite_rls_docs',
               pgcontext.query_nearest('[0,1]'::vector, 3)
           )",
    )
    .expect("post-delete composite query should execute")
    .expect("post-delete count should not be null");
    assert_eq!(remaining, 0);

    acl_reset_session_user();
}

#[pg_test]
fn drop_collection_denies_non_owner_collections() {
    acl_create_role("m2_acl_collection_owner_d");
    acl_create_role("m2_acl_denied_drop");
    acl_grant_api_access("m2_acl_collection_owner_d");
    acl_grant_api_access("m2_acl_denied_drop");

    acl_set_session_user("m2_acl_collection_owner_d");
    Spi::run("SELECT pgcontext.create_collection('m2_acl_drop_owner')")
        .expect("collection owner should create collection");
    acl_reset_session_user();

    acl_set_session_user("m2_acl_denied_drop");
    acl_expect_insufficient_privilege(
        "SELECT pgcontext.drop_collection('m2_acl_drop_owner')",
        "permission denied for collection m2_acl_drop_owner: owner is m2_acl_collection_owner_d",
    );
    acl_reset_session_user();
}

fn acl_create_role(role_name: &str) {
    Spi::run(&format!("CREATE ROLE {role_name}")).expect("role should be created");
}

fn acl_grant_api_access(role_name: &str) {
    Spi::run(&format!("GRANT USAGE ON SCHEMA public, pgcontext TO {role_name}"))
        .expect("role should receive schema usage");
    Spi::run(&format!("GRANT CREATE ON SCHEMA public TO {role_name}"))
        .expect("role should receive public schema create");
    Spi::run(&format!(
        "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO {role_name}"
    ))
    .expect("role should receive function execute");
    Spi::run(&format!("GRANT USAGE ON TYPE vector TO {role_name}"))
        .expect("role should receive vector type usage");
}

fn acl_set_session_user(role_name: &str) {
    Spi::run(&format!("SET SESSION AUTHORIZATION {role_name}"))
        .expect("session authorization should change");
}

fn acl_reset_session_user() {
    Spi::run("RESET SESSION AUTHORIZATION").expect("session authorization should reset");
}

fn acl_has_table_privilege(table_name: &str, privilege: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, $2)",
        &[table_name.into(), privilege.into()],
    )
    .expect("table privilege query should succeed")
    .expect("table privilege result should not be null")
}

fn acl_create_source_table(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             embedding vector
         )"
    ))
    .expect("source table should be created");
}

fn acl_create_facet_source_table(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             id bigint PRIMARY KEY,
             embedding vector,
             status text
         )"
    ))
    .expect("facet source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (id, embedding, status)
         VALUES (1, '[0,0,0]'::vector, 'open')"
    ))
    .expect("facet source row should be inserted");
}

fn acl_expect_insufficient_privilege(sql: &str, expected_message: &str) {
    acl_expect_sqlstate(sql, "insufficient_privilege", expected_message);
}

fn acl_expect_sqlstate(sql: &str, expected_condition: &str, expected_message: &str) {
    let expected_message = expected_message.replace('\'', "''");
    let expected_condition = expected_condition.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        BEGIN
            BEGIN
                PERFORM * FROM ({sql}) AS checked_call;
                RAISE EXCEPTION 'expected SQLSTATE condition {expected_condition}';
            EXCEPTION WHEN {expected_condition} THEN
                IF SQLERRM <> '{expected_message}' THEN
                    RAISE EXCEPTION 'unexpected SQLSTATE error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .expect("SQLSTATE condition should be raised and matched");
}
