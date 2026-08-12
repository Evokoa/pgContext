#[pg_test]
fn automatic_chunking_retains_bounded_profile_coverage_until_explicit_drain() {
    Spi::run(
        r#"
        CREATE TABLE p13_retained_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_retained_docs VALUES
            ('x','one two three four five six seven eight',1),
            ('y','nine ten eleven twelve thirteen fourteen fifteen sixteen',1),
            ('z','seventeen eighteen nineteen twenty twentyone twentytwo twentythree',1);
        SELECT pgcontext.create_collection('p13_retained','public.p13_retained_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_retained_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_a','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_b','plain_text_v1',4,4,1,0,8388608,false
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_c','plain_text_v1',2,2,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_retained','body','body','source_version',
            'public.p13_retained_chunks','p13_retained_a'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_retained','body',ARRAY['x','y','z']
        );
        CREATE TEMP TABLE p13_retained_a_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(3,60000,'retained-a');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_retained_a_claim;
        SELECT pgcontext.prepare_chunking_profile_alias(
            'p13_retained_a','p13_retained_b'
        );
        SELECT pgcontext.enqueue_document_chunking_profile(
            'p13_retained','body','p13_retained_b',ARRAY['x']
        );
        CREATE TEMP TABLE p13_retained_b_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'retained-b');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_retained_b_claim;
        SELECT pgcontext.promote_chunking_profile_alias(
            'p13_retained_a','p13_retained_b'
        );
        SELECT pgcontext.prepare_chunking_profile_alias(
            'p13_retained_a','p13_retained_c'
        );
        SELECT pgcontext.enqueue_document_chunking_profile(
            'p13_retained','body','p13_retained_c',ARRAY['y']
        );
        CREATE TEMP TABLE p13_retained_c_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(1,60000,'retained-c');
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_retained_c_claim;
        SELECT pgcontext.promote_chunking_profile_alias(
            'p13_retained_a','p13_retained_c'
        );
        "#,
    )
    .expect("retained profile coverage fixture");
    assert_eq!(
        Spi::get_one::<String>(
            "SELECT pg_catalog.string_agg(
                        chunks.source_key || ':' || profiles.profile_name, ','
                        ORDER BY chunks.source_key
                    )
               FROM pgcontext.current_document_chunks(
                        'p13_retained','body',ARRAY['x','y','z']
                    ) AS chunks
               JOIN pgcontext._visible_chunking_profiles AS profiles
                 USING (profile_revision)"
        )
        .expect("retained fallback precedence"),
        Some(
            "x:p13_retained_b,x:p13_retained_b,y:p13_retained_c,y:p13_retained_c,y:p13_retained_c,y:p13_retained_c,z:p13_retained_a"
                .to_owned(),
        ),
    );
    Spi::run("SELECT pgcontext.drain_chunking_profile_alias('p13_retained_a','p13_retained_b')")
        .expect("explicit retained-profile drain");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.bool_and(profiles.profile_name = 'p13_retained_a')
               FROM pgcontext.current_document_chunks(
                        'p13_retained','body',ARRAY['x']
                    ) AS chunks
               JOIN pgcontext._visible_chunking_profiles AS profiles
                 USING (profile_revision)"
        )
        .expect("next retained fallback after drain"),
        Some(true),
    );
}

#[pg_test]
fn automatic_chunking_full_retained_set_can_promote_an_existing_predecessor() {
    Spi::run(
        r#"
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_cap_0','plain_text_v1',8,8,1,0,8388608,false
        );
        DO $profiles$
        BEGIN
            FOR index IN 1..8 LOOP
                PERFORM pgcontext.register_chunking_profile(
                    'p13_retained_cap_' || index::text,
                    'plain_text_v1', 8, 8, 1, 0, 8388608, false
                );
                PERFORM pgcontext.prepare_chunking_profile_alias(
                    'p13_retained_cap_0', 'p13_retained_cap_' || index::text
                );
                PERFORM pgcontext.promote_chunking_profile_alias(
                    'p13_retained_cap_0', 'p13_retained_cap_' || index::text
                );
            END LOOP;
            PERFORM pgcontext.prepare_chunking_profile_alias(
                'p13_retained_cap_0', 'p13_retained_cap_0'
            );
            PERFORM pgcontext.promote_chunking_profile_alias(
                'p13_retained_cap_0', 'p13_retained_cap_0'
            );
        END
        $profiles$;
        "#,
    )
    .expect("promote an already-retained target at the exact retained cap");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext._chunking_profile_alias_retained AS retained
               JOIN pgcontext._chunking_profile_aliases AS aliases
                 USING (chunking_profile_alias_id)
              WHERE aliases.alias_name = 'p13_retained_cap_0'"
        )
        .expect("retained cap after predecessor promotion"),
        Some(8),
    );
}

#[pg_test]
fn automatic_chunking_retained_profile_preserves_its_larger_read_limit() {
    Spi::run(
        r#"
        CREATE TABLE p13_retained_limit_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_retained_limit_docs
        VALUES ('large', pg_catalog.repeat('x', 2 * 1024 * 1024), 1);
        SELECT pgcontext.create_collection(
            'p13_retained_limit', 'public.p13_retained_limit_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_retained_limit_chunks'
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_limit_a','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_retained_limit_b','plain_text_v1',8,8,1,0,1048576,false
        );
        SELECT pgcontext.register_document_source(
            'p13_retained_limit','body','body','source_version',
            'public.p13_retained_limit_chunks','p13_retained_limit_a'
        );
        SELECT pgcontext.enqueue_document_chunking(
            'p13_retained_limit','body',ARRAY['large']
        );
        CREATE TEMP TABLE p13_retained_limit_claim AS
        SELECT * FROM pgcontext.claim_document_chunk_jobs(
            1,60000,'retained-limit-a'
        );
        SELECT pgcontext.fake_process_document_chunk_job(job_id,lease_token)
          FROM p13_retained_limit_claim;
        SELECT pgcontext.prepare_chunking_profile_alias(
            'p13_retained_limit_a','p13_retained_limit_b'
        );
        SELECT pgcontext.promote_chunking_profile_alias(
            'p13_retained_limit_a','p13_retained_limit_b'
        );
        "#,
    )
    .expect("retained-profile read-limit fixture");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)
               FROM pgcontext.current_document_chunks(
                    'p13_retained_limit','body',ARRAY['large']
               )"
        )
        .expect("retained profile keeps its certified source-byte limit"),
        Some(1),
    );
}
