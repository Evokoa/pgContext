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
