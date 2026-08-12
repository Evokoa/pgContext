#[pg_test]
fn semantic_rerank_registration_enforces_collection_owner_and_source_select() {
    sql_test_create_role("semantic_register_owner");
    sql_test_create_role("semantic_register_outsider");
    sql_test_grant_api_access("semantic_register_owner");
    sql_test_grant_api_access("semantic_register_outsider");
    Spi::run(
        "CREATE TABLE public.semantic_register_acl (
             id bigint PRIMARY KEY,
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.semantic_register_acl (id, body) VALUES (1, 'postgres');
         GRANT SELECT ON public.semantic_register_acl TO semantic_register_owner;",
    )
    .expect("registration ACL fixture");

    sql_test_set_session_user("semantic_register_owner");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'semantic_register_acl', 'public.semantic_register_acl'
         )",
    )
    .expect("owner should create collection");
    sql_test_reset_session_user();
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id FROM pgcontext._collections
          WHERE collection_name = 'semantic_register_acl'",
    )
    .expect("collection lookup")
    .expect("collection id");

    sql_test_set_session_user("semantic_register_outsider");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_semantic_rerank_source(
             'semantic_register_acl', 'body', 'body', 'source_version'
         )",
        "42501",
        "permission denied for collection semantic_register_acl",
        "semantic rerank public registration by non-owner",
    );
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext._register_semantic_rerank_source(
                 {collection_id}, 'body', 'body', 'source_version'
             )"
        ),
        "42501",
        &format!("permission denied for collection {collection_id}"),
        "semantic rerank definer registration by non-owner",
    );
    sql_test_reset_session_user();

    Spi::run("REVOKE SELECT ON public.semantic_register_acl FROM semantic_register_owner")
        .expect("source SELECT revocation");
    sql_test_set_session_user("semantic_register_owner");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_semantic_rerank_source(
             'semantic_register_acl', 'body', 'body', 'source_version'
         )",
        "42501",
        "permission denied for semantic rerank source",
        "semantic rerank registration without source SELECT",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn semantic_rerank_registration_rejects_missing_and_wrong_column_types() {
    Spi::run(
        "CREATE TABLE public.semantic_register_columns (
             id bigint PRIMARY KEY,
             body_text text NOT NULL,
             body_integer integer NOT NULL,
             version_bigint bigint NOT NULL DEFAULT 1,
             version_text text NOT NULL DEFAULT '1'
         );
         SELECT pgcontext.create_collection(
             'semantic_register_columns', 'public.semantic_register_columns'
         );",
    )
    .expect("registration column fixture");

    for text_column in ["missing_body", "body_integer"] {
        shared_assert_sql_failure(
            &format!(
                "SELECT pgcontext.register_semantic_rerank_source(
                     'semantic_register_columns', 'body', '{text_column}', 'version_bigint'
                 )"
            ),
            "42703",
            "semantic rerank text column is missing or is not text",
            "semantic rerank text binding validation",
        );
    }
    for version_column in ["missing_version", "version_text"] {
        shared_assert_sql_failure(
            &format!(
                "SELECT pgcontext.register_semantic_rerank_source(
                     'semantic_register_columns', 'body', 'body_text', '{version_column}'
                 )"
            ),
            "42703",
            "semantic rerank source version column is missing or is not bigint",
            "semantic rerank version binding validation",
        );
    }
    let registered = Spi::get_one::<i64>(
        "SELECT pgcontext.register_semantic_rerank_source(
             'semantic_register_columns', 'body', 'body_text', 'version_bigint'
         )",
    )
    .expect("valid registration")
    .expect("registered source id");
    assert!(registered > 0);
}

#[pg_test]
fn semantic_rerank_visible_sources_are_isolated_by_collection_ownership() {
    for owner in ["semantic_visible_owner_a", "semantic_visible_owner_b"] {
        sql_test_create_role(owner);
        sql_test_grant_api_access(owner);
    }
    Spi::run(
        "CREATE TABLE public.semantic_visible_a (
             id bigint PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL DEFAULT 1
         );
         CREATE TABLE public.semantic_visible_b (
             id bigint PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL DEFAULT 1
         );
         GRANT SELECT ON public.semantic_visible_a TO semantic_visible_owner_a;
         GRANT SELECT ON public.semantic_visible_b TO semantic_visible_owner_b;",
    )
    .expect("visible source fixtures");

    for (owner, collection, table) in [
        ("semantic_visible_owner_a", "semantic_visible_a", "semantic_visible_a"),
        ("semantic_visible_owner_b", "semantic_visible_b", "semantic_visible_b"),
    ] {
        sql_test_set_session_user(owner);
        Spi::run(&format!(
            "SELECT pgcontext.create_collection('{collection}', 'public.{table}');
             SELECT pgcontext.register_semantic_rerank_source(
                 '{collection}', 'body', 'body', 'source_version'
             );"
        ))
        .expect("owner should register visible source");
        sql_test_reset_session_user();
    }

    for (owner, expected) in [
        ("semantic_visible_owner_a", "semantic_visible_a"),
        ("semantic_visible_owner_b", "semantic_visible_b"),
    ] {
        sql_test_set_session_user(owner);
        let visible = Spi::get_one::<String>(
            "SELECT collections.collection_name
               FROM pgcontext._visible_semantic_rerank_sources AS sources
               JOIN pgcontext._visible_collections AS collections USING (collection_id)",
        )
        .expect("visible source lookup")
        .expect("one visible source");
        assert_eq!(visible, expected);
        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM pgcontext._visible_semantic_rerank_sources",
        )
        .expect("visible source count")
        .expect("visible source count row");
        assert_eq!(count, 1);
        sql_test_reset_session_user();
    }
}
