#[pg_test]
fn semantic_rerank_cancellation_never_degrades_or_finalizes() {
    semantic_rerank_fixture("semantic_cancelled");
    let envelope = prepare_semantic_fixture("semantic_cancelled", "allow_fused_fallback", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run(&format!(
        "DO $$
         BEGIN
             PERFORM pgcontext.finalize_semantic_rerank(
                 {request_id}, NULL, 'cancelled'
             );
             RAISE EXCEPTION 'expected semantic rerank cancellation';
         EXCEPTION WHEN query_canceled THEN
             IF SQLERRM <> 'semantic rerank was cancelled' THEN
                 RAISE EXCEPTION 'unexpected semantic rerank cancellation: %', SQLERRM;
             END IF;
         END $$"
    ))
    .expect("semantic rerank cancellation should be explicit and catchable");
    let status = Spi::get_one::<String>(&format!(
        "SELECT status
           FROM pgcontext._visible_semantic_rerank_requests
          WHERE request_id = {request_id}"
    ))
    .expect("request status query should succeed")
    .expect("request should remain visible");
    assert_eq!(status, "prepared");
}

#[pg_test]
fn semantic_rerank_metadata_is_registered_authoritative_and_rechecked() {
    semantic_rerank_fixture("semantic_metadata");
    let mut candidates = semantic_candidates("semantic_metadata");
    candidates.0[0]["metadata"][0]["key"] = json!("unregistered");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_metadata', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "42703",
        "semantic rerank filter references an unknown field",
        "unregistered semantic rerank metadata",
    );

    let mut candidates = semantic_candidates("semantic_metadata");
    candidates.0[0]["metadata"][0]["value"] = json!("spoofed");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_metadata', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "55000",
        "semantic rerank metadata does not match the authoritative source",
        "spoofed semantic rerank metadata",
    );

    let envelope = prepare_semantic_fixture("semantic_metadata", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run("UPDATE public.semantic_metadata SET kind = 'note' WHERE id = 1")
        .expect("metadata edit should succeed");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3,
                     'request_id', {request_id},
                     'model', 'linear-pair-v1',
                     'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.9),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.2)
                     )
                 ),
                 NULL
             )"
        ),
        "55000",
        "semantic rerank source metadata changed before finalization",
        "semantic rerank metadata drift",
    );
}

#[pg_test]
fn semantic_rerank_registration_revalidates_acl_and_catalog_identity() {
    semantic_rerank_fixture("semantic_catalog_drift");
    let candidates = semantic_candidates("semantic_catalog_drift");
    Spi::run(
        "ALTER TABLE public.semantic_catalog_drift
             ALTER COLUMN body TYPE text COLLATE \"C\" USING body::text",
    )
    .expect("text collation should drift");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_catalog_drift', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "55000",
        "semantic rerank source is stale",
        "semantic rerank collation drift",
    );

    semantic_rerank_fixture("semantic_version_drift");
    let candidates = semantic_candidates("semantic_version_drift");
    Spi::run(
        "ALTER TABLE public.semantic_version_drift
             ALTER COLUMN source_version TYPE integer
             USING source_version::integer",
    )
    .expect("source version type should drift");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_version_drift', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "55000",
        "semantic rerank source is stale",
        "semantic rerank version type drift",
    );

    sql_test_create_role("semantic_rerank_acl_owner");
    sql_test_grant_api_access("semantic_rerank_acl_owner");
    Spi::run(
        "CREATE TABLE public.semantic_acl_source (
             id bigint PRIMARY KEY,
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.semantic_acl_source (id, body) VALUES (1, 'postgres');
         GRANT SELECT ON public.semantic_acl_source TO semantic_rerank_acl_owner;",
    )
    .expect("ACL source should be created");
    sql_test_set_session_user("semantic_rerank_acl_owner");
    Spi::run(
        "SELECT pgcontext.create_collection('semantic_acl_source', 'public.semantic_acl_source');
         SELECT pgcontext.backfill_points('semantic_acl_source', 10);
         SELECT pgcontext.register_semantic_rerank_source(
             'semantic_acl_source', 'body', 'body', 'source_version'
         );",
    )
    .expect("ACL owner should register the source");
    sql_test_reset_session_user();
    let point_id = semantic_point_id("semantic_acl_source", 1);
    Spi::run("REVOKE SELECT ON public.semantic_acl_source FROM semantic_rerank_acl_owner")
        .expect("source SELECT should be revoked");
    sql_test_set_session_user("semantic_rerank_acl_owner");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_acl_source', 'body', 'postgres',
                 '[{{
                    \"occurrence_id\":1,
                    \"point_id\":{point_id},
                    \"fused_rank\":1,
                    \"fused_score\":0.1,
                    \"contributions\":[{{
                       \"profile\":\"fixture\",\"rank\":1,
                       \"native_score\":0.1,\"weight\":1.0,
                       \"contribution\":0.01
                    }}]
                 }}]'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )"
        ),
        "42501",
        "permission denied for semantic rerank source",
        "semantic rerank source ACL loss",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn semantic_rerank_relation_oid_change_after_prepare_fails_closed() {
    semantic_rerank_fixture("semantic_oid_drift");
    let envelope = prepare_semantic_fixture("semantic_oid_drift", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run(
        "DROP TABLE public.semantic_oid_drift;
         CREATE TABLE public.semantic_oid_drift (
             id bigint PRIMARY KEY,
             tenant text NOT NULL,
             kind text NOT NULL DEFAULT 'article',
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.semantic_oid_drift (id, tenant, body) VALUES
             (1, 'red', 'postgres storage internals'),
             (2, 'red', 'rust extension development'),
             (3, 'blue', 'unrelated document');",
    )
    .expect("source relation should be recreated with a new OID");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "55000",
        "semantic rerank source registration changed before finalization",
        "semantic rerank source relation OID drift",
    );
}

#[pg_test]
fn semantic_rerank_same_name_filter_column_replacement_fails_closed() {
    semantic_rerank_fixture("semantic_filter_oid_drift");
    Spi::run(
        "ALTER TABLE public.semantic_filter_oid_drift DROP COLUMN kind;
         ALTER TABLE public.semantic_filter_oid_drift
             ADD COLUMN kind text NOT NULL DEFAULT 'article'",
    )
    .expect("filter column should be replaced at a new attribute number");
    let candidates = semantic_candidates("semantic_filter_oid_drift");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_filter_oid_drift', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "42703",
        "semantic rerank filter references an unknown field",
        "semantic rerank filter attribute drift",
    );
}

#[pg_test]
fn semantic_rerank_filter_rebinding_after_prepare_fails_closed() {
    semantic_rerank_fixture("semantic_filter_rebind");
    let envelope = prepare_semantic_fixture("semantic_filter_rebind", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");

    Spi::run(
        "ALTER TABLE public.semantic_filter_rebind DROP COLUMN kind;
         ALTER TABLE public.semantic_filter_rebind
             ADD COLUMN kind text NOT NULL DEFAULT 'article';
         SELECT pgcontext._refresh_payload_source_bindings(collection_id)
           FROM pgcontext._collection_acl
          WHERE collection_name = 'semantic_filter_rebind';",
    )
    .expect("replacement filter column binding should be refreshed");

    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "55000",
        "semantic rerank filter registration changed before finalization",
        "semantic rerank detached filter binding drift",
    );
}

#[pg_test]
fn semantic_rerank_filter_collation_drift_after_prepare_fails_closed() {
    semantic_rerank_fixture("semantic_filter_collation");
    Spi::run(
        "ALTER TABLE public.semantic_filter_collation
             ALTER COLUMN kind TYPE text COLLATE \"C\" USING kind::text",
    )
    .expect("initial filter collation should be explicit");
    let envelope = prepare_semantic_fixture("semantic_filter_collation", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run(
        "ALTER TABLE public.semantic_filter_collation
             ALTER COLUMN kind TYPE text COLLATE \"POSIX\" USING kind::text",
    )
    .expect("filter collation should drift without changing its attribute number");

    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "55000",
        "semantic rerank filter registration changed before finalization",
        "semantic rerank detached filter collation drift",
    );
}

#[pg_test]
fn semantic_rerank_filter_typmod_drift_after_prepare_fails_closed() {
    semantic_rerank_fixture("semantic_filter_typmod");
    Spi::run(
        "ALTER TABLE public.semantic_filter_typmod
             ALTER COLUMN kind TYPE varchar(16) USING kind::varchar(16)",
    )
    .expect("initial filter typmod should be explicit");
    let envelope = prepare_semantic_fixture("semantic_filter_typmod", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run(
        "ALTER TABLE public.semantic_filter_typmod
             ALTER COLUMN kind TYPE varchar(32) USING kind::varchar(32)",
    )
    .expect("filter typmod should drift without changing its attribute number or type OID");

    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "55000",
        "semantic rerank filter registration changed before finalization",
        "semantic rerank detached filter typmod drift",
    );
}

#[pg_test]
fn semantic_rerank_acl_and_rls_changes_after_prepare_fail_closed() {
    sql_test_create_role("semantic_after_prepare");
    sql_test_grant_api_access("semantic_after_prepare");
    Spi::run(
        "CREATE TABLE public.semantic_after_prepare_source (
             id bigint PRIMARY KEY,
             tenant text NOT NULL,
             kind text NOT NULL DEFAULT 'article',
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.semantic_after_prepare_source (id, tenant, body) VALUES
             (1, 'red', 'postgres storage internals'),
             (2, 'red', 'rust extension development');
         GRANT SELECT ON public.semantic_after_prepare_source TO semantic_after_prepare;",
    )
    .expect("post-prepare ACL fixture");
    sql_test_set_session_user("semantic_after_prepare");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'semantic_after_prepare_source', 'public.semantic_after_prepare_source'
         );
         SELECT pgcontext.register_filter_column(
             'semantic_after_prepare_source', 'tenant', 'tenant'
         );
         SELECT pgcontext.register_filter_column(
             'semantic_after_prepare_source', 'kind', 'kind'
         );
         SELECT pgcontext.backfill_points('semantic_after_prepare_source', 10);
         SELECT pgcontext.register_semantic_rerank_source(
             'semantic_after_prepare_source', 'body', 'body', 'source_version'
         );",
    )
    .expect("role should own registered source");
    let envelope =
        prepare_semantic_fixture("semantic_after_prepare_source", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    sql_test_reset_session_user();
    Spi::run("REVOKE SELECT ON public.semantic_after_prepare_source FROM semantic_after_prepare")
        .expect("SELECT should be revoked after prepare");
    sql_test_set_session_user("semantic_after_prepare");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "42501",
        "permission denied for semantic rerank source",
        "semantic rerank ACL revocation after prepare",
    );
    sql_test_reset_session_user();

    Spi::run(
        "GRANT SELECT ON public.semantic_after_prepare_source TO semantic_after_prepare;
         ALTER TABLE public.semantic_after_prepare_source ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.semantic_after_prepare_source FORCE ROW LEVEL SECURITY;
         CREATE POLICY semantic_after_prepare_tenant
             ON public.semantic_after_prepare_source
             USING (tenant = current_setting('pgcontext.test_tenant', true));",
    )
    .expect("RLS policy should be installed");
    sql_test_set_session_user("semantic_after_prepare");
    Spi::run("SELECT set_config('pgcontext.test_tenant', 'red', false)")
        .expect("red tenant should be selected");
    let envelope =
        prepare_semantic_fixture("semantic_after_prepare_source", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run("SELECT set_config('pgcontext.test_tenant', 'blue', false)")
        .expect("tenant visibility should change after prepare");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "42501",
        "semantic rerank candidate visibility changed before finalization",
        "semantic rerank RLS tenant change after prepare",
    );
    sql_test_reset_session_user();
}
