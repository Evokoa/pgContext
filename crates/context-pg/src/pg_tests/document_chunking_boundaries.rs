#[pg_test]
fn automatic_chunking_raw_datum_admission_rejects_non_jsonb_values() {
    assert_sqlstate(
        "SELECT pgcontext._document_chunk_raw_datum_bytes(1::int4)",
        PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
    );
}

#[pg_test]
fn automatic_chunking_profile_overlap_stops_at_the_certified_ceiling() {
    assert!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.register_chunking_profile(
                'p13_overlap_64','plain_text_v1',65,65,1,64,8388608,false
             )"
        )
        .expect("64-token overlap registration")
        .is_some()
    );
    assert_sqlstate(
        "SELECT pgcontext.register_chunking_profile(
            'p13_overlap_65','plain_text_v1',66,66,1,65,8388608,false
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
}

#[pg_test]
fn automatic_chunking_profile_alias_promotes_with_fallback_and_rolls_back() {
    Spi::run(
        r#"
        CREATE TABLE p13_alias_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_alias_docs VALUES ('a','one two three four five six seven eight',1);
        SELECT pgcontext.create_collection('p13_alias','public.p13_alias_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_alias_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_alias_v1','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_alias_v2','plain_text_v1',4,4,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_alias','body','body','source_version',
            'public.p13_alias_chunks','p13_alias_v1'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_alias','body',ARRAY['a']);
        CREATE TEMP TABLE p13_alias_first AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'alias-first');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_alias_first;
        INSERT INTO p13_alias_docs VALUES ('retry','retained retry target',1);
        SELECT pgcontext.enqueue_document_chunking('p13_alias','body',ARRAY['retry']);
        CREATE TEMP TABLE p13_alias_retained_cancelled AS
        SELECT job_id FROM pgcontext._visible_document_chunk_jobs
         WHERE source_version = 1 AND status = 'queued';
        SELECT pgcontext.cancel_document_chunk_job(job_id)
          FROM p13_alias_retained_cancelled;
        CREATE TEMP TABLE p13_alias_revisions AS
        SELECT
          (SELECT profile_revision FROM pgcontext._visible_chunking_profiles
            WHERE profile_name='p13_alias_v1') AS first_revision,
          (SELECT profile_revision FROM pgcontext._visible_chunking_profiles
            WHERE profile_name='p13_alias_v2') AS second_revision,
          (SELECT registration_revision FROM pgcontext._visible_document_sources
            WHERE source_name='body') AS registration_revision;
        SELECT pgcontext.prepare_chunking_profile_alias('p13_alias_v1','p13_alias_v2');
        "#,
    )
    .expect("profile alias promotion fixture");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(chunks.profile_revision = revisions.first_revision)
               FROM pgcontext.current_document_chunks('p13_alias','body',ARRAY['a']) AS chunks
               CROSS JOIN p13_alias_revisions AS revisions"
        )
        .expect("prior-ready alias fallback"),
        Some(true)
    );
    Spi::run("SELECT pgcontext.promote_chunking_profile_alias('p13_alias_v1','p13_alias_v2')")
        .expect("profile alias promotion before target publication");
    assert_sqlstate(
        "SELECT pgcontext.rebuild_document_chunk_job(job_id)
           FROM p13_alias_first",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_sqlstate(
        "SELECT pgcontext.retry_document_chunk_job(job_id)
           FROM p13_alias_retained_cancelled",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT jobs.status
               FROM pgcontext._visible_document_chunk_jobs AS jobs
               JOIN p13_alias_retained_cancelled AS retained USING (job_id)"
        )
        .expect("retained retry target remains intact"),
        Some("cancelled".to_owned()),
    );
    assert!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.publish_document_chunk_generation(job_id,lease_token)
               FROM p13_alias_first"
        )
        .expect("retained ready replay after promotion")
        .is_some()
    );
    Spi::run(
        "SELECT pgcontext.register_chunking_profile(
            'p13_alias_v1','plain_text_v1',8,8,1,0,8388608,false
         )",
    )
    .expect("immutable profile registration remains idempotent after promotion");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(chunks.profile_revision = revisions.first_revision)
               FROM pgcontext.current_document_chunks('p13_alias','body',ARRAY['a']) AS chunks
               CROSS JOIN p13_alias_revisions AS revisions"
        )
        .expect("prior-ready fallback after early promotion"),
        Some(true)
    );
    Spi::run(
        r#"
        SELECT pgcontext.register_document_source(
            'p13_alias','body','body','source_version',
            'public.p13_alias_chunks','p13_alias_v1'
        );
        DO $$ BEGIN
            IF (SELECT registration_revision
                  FROM pgcontext._visible_document_sources WHERE source_name='body') <>
               (SELECT registration_revision FROM p13_alias_revisions) THEN
                RAISE EXCEPTION 'idempotent registration changed after alias promotion';
            END IF;
            IF NOT EXISTS (
                SELECT 1 FROM pgcontext._visible_document_chunk_generations
                 WHERE status='ready'
            ) THEN
                RAISE EXCEPTION 'idempotent registration retired ready generations';
            END IF;
        END $$;
        SELECT pgcontext.enqueue_document_chunking('p13_alias','body',ARRAY['a']);
        CREATE TEMP TABLE p13_alias_second AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'alias-second');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_alias_second;
        "#,
    )
    .expect("promoted profile publication");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(chunks.profile_revision = revisions.second_revision)
               FROM pgcontext.current_document_chunks('p13_alias','body',ARRAY['a']) AS chunks
               CROSS JOIN p13_alias_revisions AS revisions"
        )
        .expect("promoted alias visibility"),
        Some(true)
    );
    Spi::run(
        r#"
        SELECT pgcontext.register_chunking_profile(
            'p13_alias_v3','plain_text_v1',2,2,1,0,8388608,false
        );
        SELECT pgcontext.prepare_chunking_profile_alias('p13_alias_v1','p13_alias_v3');
        SELECT pgcontext.enqueue_document_chunking_profile(
            'p13_alias','body','p13_alias_v3',ARRAY['a']
        );
        CREATE TEMP TABLE p13_alias_third AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'alias-third');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_alias_third;
        "#,
    )
    .expect("next shadow profile publication");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(chunks.profile_revision = revisions.second_revision)
               FROM pgcontext.current_document_chunks('p13_alias','body',ARRAY['a']) AS chunks
               CROSS JOIN p13_alias_revisions AS revisions"
        )
        .expect("prepared shadow never replaces prior fallback"),
        Some(true)
    );
    Spi::run("SELECT pgcontext.rollback_chunking_profile_alias('p13_alias_v1')")
        .expect("profile alias rollback");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(chunks.profile_revision = revisions.first_revision)
               FROM pgcontext.current_document_chunks('p13_alias','body',ARRAY['a']) AS chunks
               CROSS JOIN p13_alias_revisions AS revisions"
        )
        .expect("rolled-back alias visibility"),
        Some(true)
    );
}

#[pg_test]
fn automatic_chunking_claim_admits_aggregate_memory_before_returning_envelopes() {
    Spi::run(
        r#"
        CREATE TABLE p13_claim_memory_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_claim_memory_docs
        SELECT value, pg_catalog.repeat('x', 8 * 1024 * 1024), 1
          FROM pg_catalog.unnest(ARRAY['a','b']) AS value;
        SELECT pgcontext.create_collection('p13_claim_memory', 'public.p13_claim_memory_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_claim_memory_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_claim_memory_profile', 'plain_text_v1', 512, 512, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_claim_memory', 'body', 'body', 'source_version',
            'public.p13_claim_memory_chunks', 'p13_claim_memory_profile'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_claim_memory', 'body', ARRAY['a','b']
        );
        "#,
    )
    .expect("aggregate claim fixture");
    assert_sqlstate(
        "SELECT * FROM pgcontext.claim_document_chunk_jobs(2, 60000, 'memory-worker')",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_document_chunk_jobs
              WHERE status = 'queued'"
        )
        .expect("failed aggregate claim rolls back leases"),
        Some(2)
    );
}

#[pg_test]
fn automatic_chunking_rejects_oversized_source_before_lock_or_hash() {
    Spi::run(
        r#"
        CREATE TABLE p13_source_admission_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_source_admission_docs
        VALUES ('large', pg_catalog.repeat('x', 2 * 1024 * 1024), 1);
        SELECT pgcontext.create_collection(
            'p13_source_admission', 'public.p13_source_admission_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_source_admission_chunks'
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_source_admission_profile', 'plain_text_v1',
            8, 8, 1, 0, 1048576, false
        );
        SELECT pgcontext.register_document_source(
            'p13_source_admission', 'body', 'body', 'source_version',
            'public.p13_source_admission_chunks', 'p13_source_admission_profile'
        );
        "#,
    )
    .expect("oversized source admission fixture");
    assert_sqlstate(
        "SELECT pgcontext.enqueue_document_chunking(
            'p13_source_admission','body',ARRAY['large']
         )",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._visible_document_chunk_jobs"
        )
        .expect("oversized source creates no work"),
        Some(0),
    );
}

#[pg_test]
fn automatic_chunking_current_reads_budget_logical_detoasted_output() {
    Spi::run(
        r#"
        CREATE TABLE p13_read_memory_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_read_memory_docs VALUES ('a','alpha',1),('b','beta',1);
        SELECT pgcontext.create_collection('p13_read_memory', 'public.p13_read_memory_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_read_memory_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_read_memory_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_read_memory', 'body', 'body', 'source_version',
            'public.p13_read_memory_chunks', 'p13_read_memory_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_read_memory', 'body', ARRAY['a','b']);
        CREATE TEMP TABLE p13_read_memory_claims AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(2, 60000, 'read-memory-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
          FROM p13_read_memory_claims;
        DO $$ BEGIN
            IF EXISTS (SELECT 1 FROM pgcontext._visible_document_chunk_staging) THEN
                RAISE EXCEPTION 'published staging was retained';
            END IF;
        END $$;
        UPDATE p13_read_memory_chunks
           SET original_text = pg_catalog.repeat('z', 9 * 1024 * 1024),
               retrieval_text = pg_catalog.repeat('z', 9 * 1024 * 1024);
        "#,
    )
    .expect("aggregate current-read fixture");
    assert_sqlstate(
        "SELECT * FROM pgcontext.current_document_chunks(
             'p13_read_memory', 'body', ARRAY['a','b']
         )",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
}

#[pg_test]
fn automatic_chunking_current_read_budget_includes_each_returned_source_key() {
    Spi::run(
        r#"
        CREATE TABLE p13_read_key_memory_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_read_key_memory_docs
        VALUES (pg_catalog.repeat('k', 1024), 'alpha', 1);
        SELECT pgcontext.create_collection(
            'p13_read_key_memory', 'public.p13_read_key_memory_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_read_key_memory_chunks'
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_read_key_memory_profile', 'plain_text_v1',
            8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_read_key_memory', 'body', 'body', 'source_version',
            'public.p13_read_key_memory_chunks', 'p13_read_key_memory_profile'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_read_key_memory', 'body', ARRAY[pg_catalog.repeat('k', 1024)]
        );
        CREATE TEMP TABLE p13_read_key_memory_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(
            1, 60000, 'read-key-memory-worker'
        );
        SELECT pgcontext.fake_process_document_chunk_job(job_id, lease_token)
          FROM p13_read_key_memory_claim;
        UPDATE p13_read_key_memory_chunks
           SET original_text = pg_catalog.repeat(
               'z',
               (33554432
                - pg_catalog.octet_length(retrieval_text)
                - pg_catalog.octet_length(structure_kind)
                - COALESCE((
                    SELECT pg_catalog.sum(pg_catalog.octet_length(segment))
                      FROM pg_catalog.unnest(structure_path) AS segment
                  ), 0)
                - COALESCE(pg_catalog.octet_length(context_prefix), 0)
                - COALESCE(pgcontext._document_chunk_raw_datum_bytes(region), 0)
                - pgcontext._document_chunk_raw_datum_bytes(fake_embedding)
                - pgcontext._document_chunk_raw_datum_bytes(provenance)
                - 512
                - 512)::integer
           );
        "#,
    )
    .expect("source-key aggregate-read fixture");
    assert_sqlstate(
        "SELECT * FROM pgcontext.current_document_chunks(
             'p13_read_key_memory','body',ARRAY[pg_catalog.repeat('k', 1024)]
         )",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
}

#[pg_test]
fn automatic_chunking_manual_invalidation_supersedes_unpublished_work() {
    Spi::run(
        r#"
        CREATE TABLE p13_invalidate_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_invalidate_docs VALUES ('a','manual tombstone',1);
        SELECT pgcontext.create_collection('p13_invalidate', 'public.p13_invalidate_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_invalidate_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_invalidate_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_invalidate', 'body', 'body', 'source_version',
            'public.p13_invalidate_chunks', 'p13_invalidate_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_invalidate', 'body', ARRAY['a']);
        DELETE FROM p13_invalidate_docs WHERE id = 'a';
        SELECT pgcontext.invalidate_document_chunks('p13_invalidate', 'body', ARRAY['a']);
        "#,
    )
    .expect("manual invalidation");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._document_chunk_jobs LIMIT 1"
        )
        .expect("terminal invalidated job"),
        Some("superseded".to_owned())
    );
}

#[pg_test]
fn automatic_chunking_internal_mutation_helpers_require_one_shot_authority() {
    for sql in [
        "SELECT pgcontext._enqueue_document_chunk_job(1,'x',1,pg_catalog.sha256('x'::bytea),1)",
        "SELECT * FROM pgcontext._claim_document_chunk_jobs(1,1000,'worker')",
        "SELECT pgcontext._heartbeat_document_chunk_job(1,1,1000)",
        "SELECT pgcontext._cancel_document_chunk_job(1)",
        "SELECT pgcontext._retry_document_chunk_job(1)",
        "SELECT pgcontext._invalidate_document_chunk_aliases(1,ARRAY['x'])",
        "SELECT pgcontext._install_document_chunk_outbox_trigger(1)",
        "SELECT pgcontext._load_document_chunk_claim_source(1,1)",
        "SELECT pgcontext._lock_document_chunk_source(1,'x',1,1,8388608)",
        "SELECT pgcontext._lock_document_chunk_job_alias(1,1,false)",
        "SELECT pgcontext._lock_document_chunk_read_alias(1)",
    ] {
        assert_sqlstate(sql, PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE);
    }
}

#[pg_test]
fn automatic_chunking_expired_cancel_request_terminalizes_and_progress_never_regresses() {
    Spi::run(
        r#"
        CREATE TABLE p13_cancel_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_cancel_docs VALUES ('a','cancel me',1);
        SELECT pgcontext.create_collection('p13_cancel', 'public.p13_cancel_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_cancel_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_cancel_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_cancel', 'body', 'body', 'source_version',
            'public.p13_cancel_chunks', 'p13_cancel_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_cancel', 'body', ARRAY['a']);
        CREATE TEMP TABLE p13_cancel_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'cancel-worker');
        SELECT pgcontext.checkpoint_document_chunk_job(job_id,lease_token,'parsing',5,10)
          FROM p13_cancel_claim;
        "#,
    )
    .expect("cancel/progress fixture");
    assert_sqlstate(
        "SELECT pgcontext.checkpoint_document_chunk_job(job_id,lease_token,'parsing',4,10)
           FROM p13_cancel_claim",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    Spi::run(
        r#"
        SELECT pgcontext.cancel_document_chunk_job(job_id) FROM p13_cancel_claim;
        UPDATE pgcontext._document_chunk_jobs
           SET lease_expires_at = pg_catalog.clock_timestamp() - interval '1 second'
         WHERE job_id = (SELECT job_id FROM p13_cancel_claim);
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,1000,'cleanup-worker');
        "#,
    )
    .expect("expired cancellation cleanup");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT status FROM pgcontext._visible_document_chunk_jobs
              WHERE job_id = (SELECT job_id FROM p13_cancel_claim)"
        )
        .expect("terminal cancellation status"),
        Some("cancelled".to_owned())
    );
}

#[pg_test]
fn automatic_chunking_current_reads_and_rollback_reject_projection_tampering() {
    Spi::run(
        r#"
        CREATE TABLE p13_tamper_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_tamper_docs VALUES ('a','authoritative projection',1);
        SELECT pgcontext.create_collection('p13_tamper', 'public.p13_tamper_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_tamper_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_tamper_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_tamper', 'body', 'body', 'source_version',
            'public.p13_tamper_chunks', 'p13_tamper_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_tamper', 'body', ARRAY['a']);
        CREATE TEMP TABLE p13_tamper_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'tamper-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_tamper_claim;
        CREATE TEMP TABLE p13_tamper_generation AS
        SELECT generation_id FROM pgcontext._visible_current_document_chunk_generations;
        UPDATE p13_tamper_chunks SET retrieval_text = 'mutated after publication';
        "#,
    )
    .expect("post-publication tamper fixture");
    assert_sqlstate(
        "SELECT * FROM pgcontext.current_document_chunks('p13_tamper','body',ARRAY['a'])",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
    assert_sqlstate(
        "SELECT pgcontext.rollback_document_chunk_generation(
             'p13_tamper','body','a',(SELECT generation_id FROM p13_tamper_generation)
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
}

#[pg_test]
fn automatic_chunking_mixed_rls_claim_skips_hidden_jobs_without_starvation() {
    Spi::run(
        r#"
        CREATE ROLE p13_mixed_source_owner;
        CREATE ROLE p13_mixed_collection_owner;
        GRANT p13_mixed_source_owner, p13_mixed_collection_owner TO CURRENT_USER;
        GRANT CREATE ON SCHEMA public TO p13_mixed_collection_owner;
        GRANT USAGE ON SCHEMA pgcontext TO p13_mixed_collection_owner;
        CREATE TABLE p13_mixed_docs (
            id text PRIMARY KEY, tenant text NOT NULL,
            body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_mixed_docs VALUES
            ('hidden-first', 'b', 'hidden text', 1),
            ('visible-second', 'a', 'visible text', 1);
        ALTER TABLE p13_mixed_docs OWNER TO p13_mixed_source_owner;
        ALTER TABLE p13_mixed_docs ENABLE ROW LEVEL SECURITY;
        ALTER TABLE p13_mixed_docs FORCE ROW LEVEL SECURITY;
        CREATE POLICY p13_mixed_policy ON p13_mixed_docs
          USING (tenant = pg_catalog.current_setting('p13.mixed_tenant', true));
        GRANT SELECT ON p13_mixed_docs TO p13_mixed_collection_owner;
        SET ROLE p13_mixed_collection_owner;
        SELECT pg_catalog.set_config('p13.mixed_tenant', 'b', false);
        SELECT pgcontext.create_collection('p13_mixed', 'public.p13_mixed_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_mixed_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_mixed_profile', 'plain_text_v1', 8, 8, 1, 0, 8388608, false
        );
        SELECT pgcontext.register_document_source(
            'p13_mixed', 'body', 'body', 'source_version',
            'public.p13_mixed_chunks', 'p13_mixed_profile'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_mixed', 'body', ARRAY['hidden-first']
        );
        CREATE TEMP TABLE p13_mixed_hidden_job AS
        SELECT job_id FROM pgcontext._visible_document_chunk_jobs;
        SELECT pg_catalog.set_config('p13.mixed_tenant', 'a', false);
        SELECT pgcontext.enqueue_document_chunking(
            'p13_mixed', 'body', ARRAY['visible-second']
        );
        "#,
    )
    .expect("mixed RLS fixture");
    let claim = Spi::get_one::<JsonB>(
        "SELECT pg_catalog.to_jsonb(claimed)
           FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'mixed-worker') AS claimed",
    )
    .expect("mixed claim")
    .expect("visible claim");
    assert_eq!(
        claim.0["request"]["source_text"].as_str(),
        Some("visible text")
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_document_chunk_jobs
              WHERE status = 'queued'"
        )
        .expect("hidden claim remains invisible after release"),
        Some(0)
    );
    assert_sqlstate(
        "SELECT pgcontext.cancel_document_chunk_job(job_id)
           FROM p13_mixed_hidden_job",
        PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
    );
    Spi::run("RESET ROLE").expect("restore superuser");
}

#[pg_test]
fn automatic_chunking_enqueue_batch_is_atomic_when_a_later_key_is_missing() {
    Spi::run(
        r#"
        CREATE TABLE p13_atomic_enqueue_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_atomic_enqueue_docs VALUES ('visible','body',1);
        SELECT pgcontext.create_collection('p13_atomic_enqueue','public.p13_atomic_enqueue_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_atomic_enqueue_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_atomic_enqueue_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_atomic_enqueue','body','body','source_version',
            'public.p13_atomic_enqueue_chunks','p13_atomic_enqueue_profile'
        );
        "#,
    )
    .expect("atomic enqueue fixture");
    assert_sqlstate(
        "SELECT pgcontext.enqueue_document_chunking(
             'p13_atomic_enqueue','body',ARRAY['visible','missing']
         )",
        PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
    );
    assert_eq!(
        Spi::get_one::<i64>("SELECT pg_catalog.count(*) FROM pgcontext._document_chunk_jobs")
            .expect("atomic enqueue count"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_reregistration_is_idempotent_and_profile_changes_supersede_work() {
    Spi::run(
        r#"
        CREATE TABLE p13_reregister_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_reregister_docs VALUES ('a','same source',1);
        SELECT pgcontext.create_collection('p13_reregister','public.p13_reregister_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_reregister_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_reregister_a','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_reregister_b','plain_text_v1',16,16,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_reregister','body','body','source_version',
            'public.p13_reregister_chunks','p13_reregister_a'
        );
        CREATE TEMP TABLE p13_reregister_revision AS
        SELECT registration_revision FROM pgcontext._visible_document_sources;
        SELECT pgcontext.enqueue_document_chunking('p13_reregister','body',ARRAY['a']);
        SELECT pgcontext.register_document_source(
            'p13_reregister','body','body','source_version',
            'public.p13_reregister_chunks','p13_reregister_a'
        );
        DO $$ BEGIN
            IF (SELECT registration_revision FROM pgcontext._visible_document_sources)
               <> (SELECT registration_revision FROM p13_reregister_revision) THEN
                RAISE EXCEPTION 'idempotent registration changed revision';
            END IF;
            IF (SELECT status FROM pgcontext._visible_document_chunk_jobs) <> 'queued' THEN
                RAISE EXCEPTION 'idempotent registration changed queued work';
            END IF;
        END $$;
        SELECT pgcontext.register_document_source(
            'p13_reregister','body','body','source_version',
            'public.p13_reregister_chunks','p13_reregister_b'
        );
        DO $$ BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM pgcontext._document_chunk_jobs WHERE status = 'superseded'
            ) THEN RAISE EXCEPTION 'changed registration did not supersede old work'; END IF;
        END $$;
        SELECT pgcontext.enqueue_document_chunking('p13_reregister','body',ARRAY['a']);
        "#,
    )
    .expect("registration lifecycle");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._visible_document_chunk_jobs
              WHERE status = 'queued'"
        )
        .expect("replacement queued work"),
        Some(1)
    );
}

#[pg_test]
fn automatic_chunking_projection_change_creates_a_new_registration_generation() {
    Spi::run(
        r#"
        CREATE TABLE p13_binding_change_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_binding_change_docs VALUES ('a','unchanged source',1);
        SELECT pgcontext.create_collection('p13_binding_change','public.p13_binding_change_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_binding_change_chunks_a');
        SELECT pgcontext.create_document_chunk_projection('public.p13_binding_change_chunks_b');
        SELECT pgcontext.register_chunking_profile(
            'p13_binding_change_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_binding_change','body','body','source_version',
            'public.p13_binding_change_chunks_a','p13_binding_change_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_binding_change','body',ARRAY['a']);
        SELECT pgcontext.register_document_source(
            'p13_binding_change','body','body','source_version',
            'public.p13_binding_change_chunks_b','p13_binding_change_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_binding_change','body',ARRAY['a']);
        "#,
    )
    .expect("projection registration change");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(DISTINCT source_registration_revision)
               FROM pgcontext._document_chunk_generations"
        )
        .expect("registration-scoped generation identities"),
        Some(2)
    );
}

#[pg_test]
fn automatic_chunking_rejects_source_key_collation_drift() {
    Spi::run(
        r#"
        CREATE TABLE p13_key_collation_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_key_collation_docs VALUES ('a','body',1);
        SELECT pgcontext.create_collection(
            'p13_key_collation','public.p13_key_collation_docs'
        );
        SELECT pgcontext.create_document_chunk_projection('public.p13_key_collation_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_key_collation_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_key_collation','body','body','source_version',
            'public.p13_key_collation_chunks','p13_key_collation_profile'
        );
        ALTER TABLE p13_key_collation_docs
          ALTER COLUMN id TYPE text COLLATE "C" USING id::text;
        "#,
    )
    .expect("source-key collation drift fixture");
    assert_sqlstate(
        "SELECT pgcontext.enqueue_document_chunking(
             'p13_key_collation','body',ARRAY['a']
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
}

#[pg_test]
fn automatic_chunking_empty_document_publishes_and_reads_an_empty_generation() {
    Spi::run(
        r#"
        CREATE TABLE p13_empty_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_empty_docs VALUES ('a','',1);
        SELECT pgcontext.create_collection('p13_empty','public.p13_empty_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_empty_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_empty_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_empty','body','body','source_version',
            'public.p13_empty_chunks','p13_empty_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_empty','body',ARRAY['a']);
        CREATE TEMP TABLE p13_empty_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'empty-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_empty_claim;
        "#,
    )
    .expect("empty document publication");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext.current_document_chunks(
                 'p13_empty','body',ARRAY['a']
             )"
        )
        .expect("empty current generation"),
        Some(0)
    );
}

#[pg_test]
fn automatic_chunking_rebuild_repairs_a_tampered_ready_projection() {
    Spi::run(
        r#"
        CREATE TABLE p13_rebuild_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_rebuild_docs VALUES ('a','canonical body',1);
        SELECT pgcontext.create_collection('p13_rebuild','public.p13_rebuild_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_rebuild_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_rebuild_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_rebuild','body','body','source_version',
            'public.p13_rebuild_chunks','p13_rebuild_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_rebuild','body',ARRAY['a']);
        CREATE TEMP TABLE p13_rebuild_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'rebuild-worker');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_rebuild_claim;
        UPDATE p13_rebuild_chunks SET retrieval_text = 'tampered';
        SELECT pgcontext.rebuild_document_chunk_job((SELECT job_id FROM p13_rebuild_claim));
        CREATE TEMP TABLE p13_rebuild_claim_again AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'rebuild-worker-2');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_rebuild_claim_again;
        "#,
    )
    .expect("projection rebuild");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT retrieval_text FROM pgcontext.current_document_chunks(
                 'p13_rebuild','body',ARRAY['a']
             )"
        )
        .expect("rebuilt projection"),
        Some("canonical body".to_owned())
    );
}

#[pg_test]
fn automatic_chunking_outbox_retires_old_primary_keys_and_enqueues_new_keys() {
    Spi::run(
        r#"
        CREATE TABLE p13_key_change_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        SELECT pgcontext.create_collection('p13_key_change','public.p13_key_change_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_key_change_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_key_change_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_key_change','body','body','source_version',
            'public.p13_key_change_chunks','p13_key_change_profile'
        );
        SELECT pgcontext.install_document_chunk_trigger('p13_key_change','body');
        INSERT INTO p13_key_change_docs VALUES ('old','one',1);
        UPDATE p13_key_change_docs SET id = 'new' WHERE id = 'old';
        UPDATE p13_key_change_docs
           SET id = 'newer', body = 'two', source_version = 2 WHERE id = 'new';
        "#,
    )
    .expect("primary-key outbox lifecycle");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._document_chunk_generations
              WHERE source_key IN ('old','new') AND status = 'superseded'"
        )
        .expect("retired old keys"),
        Some(2)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*) FROM pgcontext._document_chunk_generations
              WHERE source_key = 'newer' AND source_version = 2 AND status = 'queued'"
        )
        .expect("new key queued"),
        Some(1)
    );
}

#[pg_test]
fn automatic_chunking_admits_key_arrays_and_source_text_before_rust_allocation() {
    Spi::run(
        r#"
        CREATE TABLE p13_datum_bound_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_datum_bound_docs VALUES ('a','small',1),('b','small',1);
        SELECT pgcontext.create_collection('p13_datum_bound','public.p13_datum_bound_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_datum_bound_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_datum_bound_profile','plain_text_v1',8,8,1,0,1024,false
        );
        SELECT pgcontext.register_document_source(
            'p13_datum_bound','body','body','source_version',
            'public.p13_datum_bound_chunks','p13_datum_bound_profile'
        );
        "#,
    )
    .expect("datum admission fixture");
    assert_sqlstate(
        "SELECT pgcontext.enqueue_document_chunking(
             'p13_datum_bound','body',ARRAY[pg_catalog.repeat('x',1025)]
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT pgcontext.enqueue_document_chunking(
             'p13_datum_bound','body',
             ARRAY(SELECT value::text FROM pg_catalog.generate_series(1,257) AS value)
         )",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
    Spi::run(
        r#"
        SELECT pgcontext.enqueue_document_chunking('p13_datum_bound','body',ARRAY['a','b']);
        UPDATE p13_datum_bound_docs SET body = pg_catalog.repeat('z',2048) WHERE id = 'a';
        "#,
    )
    .expect("enqueue bounded source rows");
    assert_sqlstate(
        "SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'datum-worker')",
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
    );
}

#[pg_test]
fn automatic_chunking_source_edits_supersede_stale_claims_and_do_not_starve() {
    Spi::run(
        r#"
        CREATE TABLE p13_stale_claim_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_stale_claim_docs VALUES ('a','stale',1),('b','current',1);
        SELECT pgcontext.create_collection('p13_stale_claim','public.p13_stale_claim_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_stale_claim_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_stale_claim_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_stale_claim','body','body','source_version',
            'public.p13_stale_claim_chunks','p13_stale_claim_profile'
        );
        SELECT pgcontext.enqueue_document_chunking('p13_stale_claim','body',ARRAY['a','b']);
        UPDATE p13_stale_claim_docs SET body='changed', source_version=2 WHERE id='a';
        CREATE TEMP TABLE p13_stale_claim_result AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'stale-worker');
        "#,
    )
    .expect("stale claim lifecycle");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT request->>'source_text' FROM p13_stale_claim_result"
        )
        .expect("current claim"),
        Some("current".to_owned())
    );
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT jobs.status
               FROM pgcontext._document_chunk_jobs AS jobs
               JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
              WHERE generations.source_key='a'"
        )
        .expect("stale terminal status"),
        Some("superseded".to_owned())
    );
}
