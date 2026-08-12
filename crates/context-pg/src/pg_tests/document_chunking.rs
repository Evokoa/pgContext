#[pg_test]
fn automatic_chunking_publishes_only_complete_source_linked_generations() {
    Spi::run(
        r#"
        CREATE TABLE p13_documents (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_documents VALUES
            ('a', E'# Title\n\nAlpha beta gamma delta epsilon.', 1);
        SELECT pgcontext.create_collection('p13_docs', 'public.p13_documents');
        SELECT pgcontext.create_document_chunk_projection('public.p13_chunks');
        SELECT pgcontext.register_chunking_profile(
            'default', 'markdown_v1', 4, 4, 1, 1, 8388608, true
        );
        SELECT pgcontext.register_document_source(
            'p13_docs', 'body', 'body', 'source_version',
            'public.p13_chunks', 'default'
        );
        "#,
    )
    .expect("register P13 objects");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.enqueue_document_chunking('p13_docs', 'body', ARRAY['a'])"
        )
        .expect("enqueue"),
        Some(1)
    );
    let claim = Spi::get_one::<JsonB>(
        "SELECT pg_catalog.to_jsonb(claimed)
           FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'worker-a') AS claimed",
    )
    .expect("claim")
    .expect("claimed row");
    let job_id = claim.0["job_id"].as_i64().expect("job identity");
    let lease_token = claim.0["lease_token"].as_i64().expect("lease token");

    assert_sqlstate(
        "SELECT pgcontext._register_chunking_profile(\
            'bypass', 'plain_text_v1', 4, 8, 1, 1, 1024, false, \
            decode(repeat('00', 32), 'hex')\
         )",
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );
    assert_sqlstate(
        "SELECT pgcontext._register_document_source(
             0, 'bypass', 0, 0, 0, 0, 'public', 'bypass',
             ARRAY[]::int2[], ARRAY[]::oid[], ARRAY[]::bigint[]
         )",
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );
    for sql in [
        "SELECT pgcontext._prepare_chunking_profile_alias(0, 0)",
        "SELECT pgcontext._promote_chunking_profile_alias(0, 0)",
        "SELECT pgcontext._rollback_chunking_profile_alias(0)",
        "SELECT pgcontext._drain_chunking_profile_alias(0, 0)",
    ] {
        assert_sqlstate(sql, PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE);
    }
    assert_sqlstate(
        &format!(
            "SELECT pgcontext._stage_document_chunk_response(\
                {job_id}, {lease_token}, '{{}}'::jsonb, decode(repeat('00', 32), 'hex'), 0, 0, 2\
             )"
        ),
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );
    assert_sqlstate(
        &format!(
            "SELECT pgcontext._complete_document_chunk_publication(\
                {job_id}, {lease_token}, decode(repeat('00', 32), 'hex'), \
                decode(repeat('00', 32), 'hex'), 0, 0, 2\
             )"
        ),
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );

    assert!(
        Spi::get_one_with_args::<i64>(
            "SELECT pgcontext.fake_process_document_chunk_job($1, $2)",
            &[job_id.into(), lease_token.into()],
        )
        .expect("fake worker publication")
        .is_some()
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(
                'p13_docs', 'body', ARRAY['a']
             )"
        )
        .expect("current chunks"),
        Some(3)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_document_embedding_jobs"
        )
        .expect("fake embedding work"),
        Some(3)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._visible_document_chunk_jobs
              WHERE status <> 'ready'"
        )
        .expect("job status"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_preserves_prior_ready_on_cancel_and_supersession() {
    Spi::run(
        r#"
        CREATE TABLE p13_versions (
            id text PRIMARY KEY,
            body text NOT NULL,
            source_version bigint NOT NULL
        );
        INSERT INTO p13_versions VALUES ('a', 'first generation', 1);
        SELECT pgcontext.create_collection('p13_versions', 'public.p13_versions');
        SELECT pgcontext.create_document_chunk_projection('public.p13_version_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_versions_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_versions', 'body', 'body', 'source_version',
            'public.p13_version_chunks', 'p13_versions_profile'
        );
        SELECT pgcontext.install_document_chunk_trigger('p13_versions', 'body');
        SELECT pgcontext.enqueue_document_chunking('p13_versions', 'body', ARRAY['a']);
        "#,
    )
    .expect("setup versions");
    let first = Spi::get_one::<JsonB>(
        "SELECT pg_catalog.to_jsonb(claimed)
           FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'worker-a') AS claimed",
    )
    .expect("first claim")
    .expect("first row");
    let first_job = first.0["job_id"].as_i64().expect("job");
    let first_token = first.0["lease_token"].as_i64().expect("token");
    Spi::run_with_args(
        "SELECT pgcontext.fake_process_document_chunk_job($1, $2)",
        &[first_job.into(), first_token.into()],
    )
    .expect("publish first");
    let first_generation = Spi::get_one::<i64>(
        "SELECT generation_id
           FROM pgcontext._visible_current_document_chunk_generations
          WHERE source_key = 'a'",
    )
    .expect("first generation query")
    .expect("first generation");

    Spi::run(
        "UPDATE p13_versions SET body = 'second generation', source_version = 2 WHERE id = 'a';",
    )
    .expect("enqueue second");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(
                'p13_versions', 'body', ARRAY['a']
             )"
        )
        .expect("stale prior generation is hidden"),
        Some(0)
    );
    let second_job = Spi::get_one::<i64>(
        "SELECT jobs.job_id FROM pgcontext._visible_document_chunk_jobs AS jobs
          WHERE jobs.source_version = 2",
    )
    .expect("second job")
    .expect("queued second job");
    assert_eq!(
        Spi::get_one_with_args::<bool>(
            "SELECT pgcontext.cancel_document_chunk_job($1)",
            &[second_job.into()],
        )
        .expect("cancel queued"),
        Some(true)
    );
    assert_eq!(
        Spi::get_one_with_args::<bool>(
            "SELECT pgcontext.retry_document_chunk_job($1)",
            &[second_job.into()],
        )
        .expect("retry cancelled"),
        Some(true)
    );
    let second = Spi::get_one::<JsonB>(
        "SELECT pg_catalog.to_jsonb(claimed)
           FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'worker-b') AS claimed",
    )
    .expect("second claim")
    .expect("second claimed row");
    let second_token = second.0["lease_token"].as_i64().expect("second token");
    Spi::run_with_args(
        "SELECT pgcontext.fake_process_document_chunk_job($1, $2)",
        &[second_job.into(), second_token.into()],
    )
    .expect("publish second");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT original_text FROM pgcontext.current_document_chunks(
                'p13_versions', 'body', ARRAY['a']
             ) LIMIT 1"
        )
        .expect("new ready"),
        Some("second generation".to_owned())
    );
    let progress = Spi::get_one::<JsonB>(
        "SELECT pgcontext.document_chunking_progress('p13_versions', 'body')",
    )
    .expect("progress report")
    .expect("progress row");
    assert_eq!(progress.0["current_documents"].as_i64(), Some(1));
    assert_eq!(progress.0["ready"].as_i64(), Some(1));
    assert_sqlstate(
        &format!(
            "SELECT pgcontext.rollback_document_chunk_generation(
                 'p13_versions', 'body', 'a', {first_generation}
             )"
        ),
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    Spi::run("DELETE FROM p13_versions WHERE id = 'a'").expect("delete source");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(
                'p13_versions', 'body', ARRAY['a']
             )"
        )
        .expect("deleted chunks"),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._document_chunk_generations
              WHERE source_key = 'a' ORDER BY source_version DESC LIMIT 1"
        )
        .expect("retired generation"),
        Some("retired".to_owned())
    );
}

#[pg_test]
fn automatic_chunking_fences_stale_leases_and_rejects_partial_output() {
    Spi::run(
        r#"
        CREATE TABLE p13_fencing_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_fencing_docs VALUES ('a', 'lease fenced document', 1);
        SELECT pgcontext.create_collection('p13_fencing', 'public.p13_fencing_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_fencing_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_fencing_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_fencing', 'body', 'body', 'source_version',
            'public.p13_fencing_chunks', 'p13_fencing_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_fencing', 'body', ARRAY['a']);
        CREATE TEMP TABLE p13_first_claim AS
        SELECT job_id, lease_token
          FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'first-worker');
        UPDATE pgcontext._document_chunk_jobs
           SET lease_expires_at = pg_catalog.clock_timestamp() - INTERVAL '1 millisecond'
         WHERE job_id = (SELECT job_id FROM p13_first_claim);
        CREATE TEMP TABLE p13_second_claim AS
        SELECT job_id, lease_token
          FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'second-worker');
        "#,
    )
    .expect("set up fenced lease");
    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(
             (SELECT job_id FROM p13_first_claim),
             (SELECT lease_token FROM p13_first_claim)
         )",
        PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
    );
    assert_sqlstate(
        "SELECT pgcontext.stage_document_chunks(
             (SELECT job_id FROM p13_second_claim),
             (SELECT lease_token FROM p13_second_claim),
             '{\"version\":\"chunk_worker_response_v1\",\"complete\":false,\"chunks\":[]}'::jsonb
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.fake_process_document_chunk_job(
                 (SELECT job_id FROM p13_second_claim),
                 (SELECT lease_token FROM p13_second_claim)
             )"
        )
        .expect("current worker publishes")
        .is_some()
    );
}

#[pg_test]
fn automatic_chunking_fails_closed_on_source_and_projection_identity_drift() {
    Spi::run(
        r#"
        CREATE TABLE p13_drift_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_drift_docs VALUES ('a', 'original content', 1);
        SELECT pgcontext.create_collection('p13_drift', 'public.p13_drift_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_drift_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_drift_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_drift', 'body', 'body', 'source_version',
            'public.p13_drift_chunks', 'p13_drift_profile'
        );
        SELECT pgcontext.install_document_chunk_trigger('p13_drift', 'body');
        SELECT pgcontext.enqueue_document_chunking('p13_drift', 'body', ARRAY['a']);
        CREATE FUNCTION pg_temp.p13_same_version_drift()
        RETURNS bigint LANGUAGE plpgsql AS $function$
        BEGIN
            UPDATE p13_drift_docs SET body = 'different content' WHERE id = 'a';
            RETURN 1;
        END
        $function$;
        "#,
    )
    .expect("set up drift fixture");
    assert_sqlstate(
        "SELECT pg_temp.p13_same_version_drift()",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    Spi::run(
        r#"
        CREATE TEMP TABLE p13_drift_claim AS
        SELECT job_id, lease_token
          FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'drift-worker');
        DO $drop_trigger$
        DECLARE trigger_name text;
        BEGIN
            SELECT triggers.tgname INTO trigger_name
              FROM pg_catalog.pg_trigger AS triggers
             WHERE triggers.tgrelid = 'public.p13_drift_docs'::regclass
               AND NOT triggers.tgisinternal;
            EXECUTE pg_catalog.format(
                'DROP TRIGGER %I ON public.p13_drift_docs', trigger_name
            );
        END
        $drop_trigger$;
        ALTER TABLE p13_drift_docs
            ALTER COLUMN body TYPE varchar(64) USING body::varchar(64);
        "#,
    )
    .expect("change source column contract after claim");
    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(\
             (SELECT job_id FROM p13_drift_claim),\
             (SELECT lease_token FROM p13_drift_claim)\
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    Spi::run(
        r#"
        ALTER TABLE p13_drift_docs
            ALTER COLUMN body TYPE text USING body::text;
        DROP TABLE p13_drift_chunks;
        SELECT pgcontext.create_document_chunk_projection('public.p13_drift_chunks');
        "#,
    )
    .expect("replace projection after claim");
    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(
             (SELECT job_id FROM p13_drift_claim),
             (SELECT lease_token FROM p13_drift_claim)
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_current_document_chunk_generations"
        )
        .expect("no alias after drift"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_fails_closed_on_projection_attribute_and_collation_drift() {
    Spi::run(
        r#"
        CREATE TABLE p13_projection_identity_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_projection_identity_docs VALUES ('a', 'projection identity', 1);
        SELECT pgcontext.create_collection(
            'p13_projection_identity', 'public.p13_projection_identity_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_projection_identity_chunks'
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_projection_identity_profile',
            'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_projection_identity', 'body', 'body', 'source_version',
            'public.p13_projection_identity_chunks',
            'p13_projection_identity_profile'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_projection_identity', 'body', ARRAY['a']
        );
        CREATE TEMP TABLE p13_projection_identity_claim AS
        SELECT *
          FROM pgcontext.claim_document_chunk_jobs(
              1, 60000, 'projection-identity-worker'
          );
        ALTER TABLE p13_projection_identity_chunks DROP COLUMN context_prefix;
        ALTER TABLE p13_projection_identity_chunks ADD COLUMN context_prefix text;
        ALTER TABLE p13_projection_identity_chunks
            ALTER COLUMN original_text TYPE text COLLATE "C";
        "#,
    )
    .expect("projection identity drift fixture");

    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(\
             (SELECT job_id FROM p13_projection_identity_claim),\
             (SELECT lease_token FROM p13_projection_identity_claim)\
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._visible_current_document_chunk_generations"
        )
        .expect("no alias after projection attribute drift"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_requires_exact_pg_catalog_projection_type_oids() {
    Spi::run(
        r#"
        CREATE SCHEMA p13_projection_types;
        CREATE DOMAIN p13_projection_types.jsonb AS pg_catalog.jsonb;
        CREATE TABLE p13_projection_type_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        SELECT pgcontext.create_collection(
            'p13_projection_type', 'public.p13_projection_type_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_projection_type_chunks'
        );
        ALTER TABLE p13_projection_type_chunks
            ALTER COLUMN fake_embedding TYPE p13_projection_types.jsonb
            USING fake_embedding::p13_projection_types.jsonb;
        SELECT pgcontext.register_chunking_profile(
            'p13_projection_type_profile',
            'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        "#,
    )
    .expect("projection type OID fixture");

    assert_sqlstate(
        "SELECT pgcontext.register_document_source(
             'p13_projection_type', 'body', 'body', 'source_version',
             'public.p13_projection_type_chunks', 'p13_projection_type_profile'
         )",
        PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
    );
}

#[pg_test]
fn automatic_chunking_rechecks_source_acl_and_keeps_catalogs_private() {
    Spi::run(
        r#"
        CREATE TABLE p13_acl_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_acl_docs VALUES ('a', 'authorized at registration', 1);
        SELECT pgcontext.create_collection('p13_acl', 'public.p13_acl_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_acl_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_acl_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_acl', 'body', 'body', 'source_version',
            'public.p13_acl_chunks', 'p13_acl_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_acl', 'body', ARRAY['a']);
        CREATE ROLE p13_acl_worker;
        GRANT USAGE ON SCHEMA pgcontext TO p13_acl_worker;
        GRANT p13_acl_worker TO CURRENT_USER;
        REVOKE SELECT ON p13_acl_docs FROM PUBLIC, p13_acl_worker;
        SET SESSION AUTHORIZATION p13_acl_worker;
        "#,
    )
    .expect("set up ACL fixture");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.has_table_privilege(CURRENT_USER,
                 'pgcontext._document_chunk_jobs', 'SELECT')"
        )
        .expect("private job catalog privilege"),
        Some(false)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_document_chunk_jobs"
        )
        .expect("membership-filtered jobs"),
        Some(0)
    );
    Spi::run("RESET SESSION AUTHORIZATION").expect("restore test session user");
}

#[pg_test]
fn automatic_chunking_denies_hydration_after_source_select_is_revoked() {
    Spi::run(
        r#"
        CREATE ROLE p13_source_owner;
        CREATE ROLE p13_collection_owner;
        GRANT p13_source_owner, p13_collection_owner TO CURRENT_USER;
        GRANT CREATE ON SCHEMA public TO p13_collection_owner;
        GRANT USAGE ON SCHEMA pgcontext TO p13_collection_owner;
        CREATE TABLE p13_acl_revoke_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_acl_revoke_docs VALUES
            ('a', 'published before revoke', 1),
            ('b', 'queued before revoke', 1);
        ALTER TABLE p13_acl_revoke_docs OWNER TO p13_source_owner;
        GRANT SELECT ON p13_acl_revoke_docs TO p13_collection_owner;
        SET SESSION AUTHORIZATION p13_collection_owner;
        SELECT pgcontext.create_collection('p13_acl_revoke', 'public.p13_acl_revoke_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_acl_revoke_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_acl_revoke_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_acl_revoke', 'body', 'body', 'source_version',
            'public.p13_acl_revoke_chunks', 'p13_acl_revoke_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_acl_revoke', 'body', ARRAY['a','b']);
        CREATE TEMP TABLE p13_acl_published_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'acl-publish-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
          FROM p13_acl_published_claim;
        RESET SESSION AUTHORIZATION;
        REVOKE SELECT ON p13_acl_revoke_docs FROM p13_collection_owner;
        SET SESSION AUTHORIZATION p13_collection_owner;
        "#,
    )
    .expect("set up revoked source SELECT");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'acl-worker')"
        )
        .expect("revoked source claims are skipped"),
        Some(0)
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.current_document_chunks(
             'p13_acl_revoke', 'body', ARRAY['a']
         )",
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM information_schema.columns
              WHERE table_schema = 'pgcontext'
                AND ((table_name = '_visible_document_chunk_staging'
                      AND column_name IN ('response_json','response_sha256','lease_token'))
                  OR (table_name = '_visible_document_chunk_jobs'
                      AND column_name IN (
                          'source_key','lease_token','lease_worker','lease_expires_at'
                      )))"
        )
        .expect("content-free worker views under revoked source ACL"),
        Some(0)
    );
    Spi::run("RESET SESSION AUTHORIZATION").expect("restore superuser after ACL test");
    Spi::run(
        "SELECT pgcontext.cancel_document_chunk_job(job_id)
           FROM pgcontext._visible_document_chunk_jobs WHERE status = 'queued'",
    )
    .expect("cancel skipped revoked-source jobs");
}

#[pg_test]
fn automatic_chunking_rechecks_forced_rls_before_releasing_or_publishing_text() {
    Spi::run(
        r#"
        CREATE ROLE p13_rls_source_owner;
        CREATE ROLE p13_rls_collection_owner;
        GRANT p13_rls_source_owner, p13_rls_collection_owner TO CURRENT_USER;
        GRANT CREATE ON SCHEMA public TO p13_rls_collection_owner;
        GRANT USAGE ON SCHEMA pgcontext TO p13_rls_collection_owner;
        CREATE TABLE p13_rls_docs (
            id text PRIMARY KEY,
            tenant text NOT NULL,
            body text NOT NULL,
            source_version bigint NOT NULL
        );
        INSERT INTO p13_rls_docs VALUES
            ('a', 'tenant-a', 'visible only to tenant a', 1),
            ('b', 'tenant-b', 'visible only to tenant b', 1);
        ALTER TABLE p13_rls_docs OWNER TO p13_rls_source_owner;
        ALTER TABLE p13_rls_docs ENABLE ROW LEVEL SECURITY;
        ALTER TABLE p13_rls_docs FORCE ROW LEVEL SECURITY;
        CREATE POLICY p13_tenant_policy ON p13_rls_docs
          USING (tenant = pg_catalog.current_setting('p13.tenant', true));
        GRANT SELECT ON p13_rls_docs TO p13_rls_collection_owner;
        SET ROLE p13_rls_collection_owner;
        SELECT pg_catalog.set_config('p13.tenant', 'tenant-a', false);
        SELECT pgcontext.create_collection('p13_rls', 'public.p13_rls_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_rls_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_rls_profile', 'plain_text_v1', 4, 8, 1, 1, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_rls', 'body', 'body', 'source_version',
            'public.p13_rls_chunks', 'p13_rls_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_rls', 'body', ARRAY['a']);
        CREATE TEMP TABLE p13_rls_claim AS
        SELECT job_id, lease_token
          FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'rls-worker');
        SELECT pg_catalog.set_config('p13.tenant', 'tenant-b', false);
        "#,
    )
    .expect("set up forced-RLS chunking fixture");
    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(\
             (SELECT job_id FROM p13_rls_claim),\
             (SELECT lease_token FROM p13_rls_claim)\
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    Spi::run("SELECT pg_catalog.set_config('p13.tenant', 'tenant-a', false)")
        .expect("restore authorized tenant");
    assert!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.fake_process_document_chunk_job(\
                 (SELECT job_id FROM p13_rls_claim),\
                 (SELECT lease_token FROM p13_rls_claim)\
             )"
        )
        .expect("publish under authorized tenant")
        .is_some()
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(\
                 'p13_rls', 'body', ARRAY['a']\
             )"
        )
        .expect("authorized current chunks"),
        Some(1)
    );
    Spi::run("SELECT pg_catalog.set_config('p13.tenant', 'tenant-b', false)")
        .expect("switch to hidden tenant");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(\
                 'p13_rls', 'body', ARRAY['a']\
             )"
        )
        .expect("RLS-hidden current chunks"),
        Some(0)
    );
    for view in [
        "_visible_document_chunk_generations",
        "_visible_current_document_chunk_generations",
        "_visible_document_embedding_jobs",
    ] {
        let sql = format!("SELECT pg_catalog.count(*) FROM pgcontext.{view}");
        assert_eq!(
            Spi::get_one::<i64>(&sql).expect("RLS-filtered lifecycle view"),
            Some(0),
            "{view} leaked a source-row-derived object"
        );
    }
    Spi::run("RESET ROLE").expect("restore superuser after RLS test");
}

#[pg_test]
fn automatic_chunking_scopes_occurrences_to_rows_and_replays_publication() {
    Spi::run(
        r#"
        CREATE TABLE p13_identity_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_identity_docs VALUES
            ('a', 'identical document body', 1),
            ('b', 'identical document body', 1);
        SELECT pgcontext.create_collection('p13_identity', 'public.p13_identity_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_identity_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_identity_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_identity', 'body', 'body', 'source_version',
            'public.p13_identity_chunks', 'p13_identity_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_identity', 'body', ARRAY['a','b']);
        CREATE TEMP TABLE p13_identity_claims AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(2, 60000, 'identity-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
          FROM p13_identity_claims ORDER BY job_id;
        "#,
    )
    .expect("publish identical documents");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(DISTINCT occurrence_id)
               FROM p13_identity_chunks WHERE ordinal = 0"
        )
        .expect("row-scoped occurrence identities"),
        Some(2)
    );
    let replay = Spi::get_one::<i64>(
        "SELECT pgcontext.publish_document_chunk_generation(job_id, lease_token)
           FROM p13_identity_claims ORDER BY job_id LIMIT 1",
    )
    .expect("publication replay")
    .expect("replayed generation");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT generation_id FROM pgcontext._visible_current_document_chunk_generations
              ORDER BY source_key LIMIT 1"
        )
        .expect("current generation"),
        Some(replay)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM information_schema.columns
              WHERE table_schema = 'pgcontext'
                AND table_name = '_visible_document_chunk_staging'
                AND column_name IN ('response_json','response_sha256','lease_token')"
        )
        .expect("content-free staging view"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_cancellation_failure_and_attempt_exhaustion_are_terminal() {
    Spi::run(
        r#"
        CREATE TABLE p13_terminal_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_terminal_docs VALUES
            ('cancel', 'cancel this job', 1), ('exhaust', 'exhaust this job', 1);
        SELECT pgcontext.create_collection('p13_terminal', 'public.p13_terminal_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_terminal_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_terminal_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_terminal', 'body', 'body', 'source_version',
            'public.p13_terminal_chunks', 'p13_terminal_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_terminal', 'body', ARRAY['cancel']);
        CREATE TEMP TABLE p13_cancel_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'terminal-worker');
        SELECT pgcontext.checkpoint_document_chunk_job(job_id, lease_token, 'parsing', 0, 1)
          FROM p13_cancel_claim;
        SELECT pgcontext.checkpoint_document_chunk_job(job_id, lease_token, 'chunking', 0, 1)
          FROM p13_cancel_claim;
        SELECT pgcontext.checkpoint_document_chunk_job(job_id, lease_token, 'embedding', 1, 1)
          FROM p13_cancel_claim;
        SELECT pgcontext.cancel_document_chunk_job(job_id) FROM p13_cancel_claim;
        "#,
    )
    .expect("cancel leased work");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._visible_document_chunk_jobs
              WHERE job_id = (SELECT job_id FROM p13_cancel_claim)"
        )
        .expect("cooperative cancellation request"),
        Some("cancel_requested".to_owned())
    );
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pgcontext.heartbeat_document_chunk_job(job_id, lease_token, 60000)
               FROM p13_cancel_claim"
        )
        .expect("worker acknowledges cooperative cancellation"),
        Some(false),
    );
    Spi::run(
        r#"
        SELECT pgcontext.retry_document_chunk_job(job_id) FROM p13_cancel_claim;
        CREATE TEMP TABLE p13_fail_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'terminal-worker');
        SELECT pgcontext.fail_document_chunk_job(job_id, lease_token, 'worker_crash')
          FROM p13_fail_claim;
        SELECT pgcontext.enqueue_document_chunking('p13_terminal', 'body', ARRAY['exhaust']);
        "#,
    )
    .expect("record worker failure");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT error_code FROM pgcontext._visible_document_chunk_jobs
              WHERE job_id = (SELECT job_id FROM p13_fail_claim)"
        )
        .expect("worker failure code"),
        Some("worker_crash".to_owned())
    );
    for attempt in 0..3 {
        let claim = Spi::get_one::<JsonB>(
            "SELECT pg_catalog.to_jsonb(claimed)
               FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'exhaust-worker') AS claimed",
        )
        .expect("claim exhausted job")
        .unwrap_or_else(|| panic!("attempt {attempt} should claim"));
        let job_id = claim.0["job_id"].as_i64().expect("job id");
        Spi::run_with_args(
            "UPDATE pgcontext._document_chunk_jobs
                SET lease_expires_at = pg_catalog.clock_timestamp() - INTERVAL '1 millisecond'
              WHERE job_id = $1",
            &[job_id.into()],
        )
        .expect("expire claim");
    }
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'exhaust-worker')"
        )
        .expect("exhaustion sweep"),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._visible_document_chunk_jobs
              WHERE attempt = 3 AND error_code = 'attempts_exhausted'"
        )
        .expect("exhausted terminal status"),
        Some("failed".to_owned())
    );
}

#[pg_test]
fn automatic_chunking_rejects_projection_rows_suppressed_by_user_trigger() {
    Spi::run(
        r#"
        CREATE TABLE p13_projection_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_projection_docs VALUES ('a', 'projection completeness', 1);
        SELECT pgcontext.create_collection('p13_projection', 'public.p13_projection_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_projection_chunks');
        CREATE FUNCTION p13_suppress_chunk() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RETURN NULL; END
        $$;
        CREATE TRIGGER p13_suppress_chunk BEFORE INSERT ON p13_projection_chunks
          FOR EACH ROW EXECUTE FUNCTION p13_suppress_chunk();
        SELECT pgcontext.register_chunking_profile(
            'p13_projection_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_projection', 'body', 'body', 'source_version',
            'public.p13_projection_chunks', 'p13_projection_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_projection', 'body', ARRAY['a']);
        CREATE TEMP TABLE p13_projection_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'projection-worker');
        "#,
    )
    .expect("projection suppression fixture");
    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(
             (SELECT job_id FROM p13_projection_claim),
             (SELECT lease_token FROM p13_projection_claim)
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._visible_current_document_chunk_generations"
        )
        .expect("alias not flipped"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_rejects_projection_embedding_and_provenance_mutation() {
    Spi::run(
        r#"
        CREATE TABLE p13_projection_mutation_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL);
        INSERT INTO p13_projection_mutation_docs VALUES ('a', 'projection mutation', 1);
        SELECT pgcontext.create_collection(
            'p13_projection_mutation', 'public.p13_projection_mutation_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_projection_mutation_chunks'
        );
        CREATE FUNCTION p13_mutate_chunk_projection() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            NEW.fake_embedding := '[999]'::jsonb;
            NEW.provenance := '{"mutated":true}'::jsonb;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER p13_mutate_chunk_projection
          BEFORE INSERT ON p13_projection_mutation_chunks
          FOR EACH ROW EXECUTE FUNCTION p13_mutate_chunk_projection();
        SELECT pgcontext.register_chunking_profile(
            'p13_projection_mutation_profile',
            'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_projection_mutation', 'body', 'body', 'source_version',
            'public.p13_projection_mutation_chunks',
            'p13_projection_mutation_profile'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_projection_mutation', 'body', ARRAY['a']
        );
        CREATE TEMP TABLE p13_projection_mutation_claim AS
        SELECT *
          FROM pgcontext.claim_document_chunk_jobs(
              1, 60000, 'projection-mutation-worker'
          );
        "#,
    )
    .expect("projection mutation fixture");

    assert_sqlstate(
        "SELECT pgcontext.fake_process_document_chunk_job(\
             (SELECT job_id FROM p13_projection_mutation_claim),\
             (SELECT lease_token FROM p13_projection_mutation_claim)\
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._visible_current_document_chunk_generations"
        )
        .expect("alias not flipped after projection row mutation"),
        Some(0)
    );
}
