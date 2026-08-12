#[pg_test]
fn automatic_chunking_registration_requires_table_and_exact_unique_arbiters() {
    Spi::run(
        r#"
        CREATE TABLE p13_contract_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_contract_docs VALUES ('a','body',1);
        SELECT pgcontext.create_collection('p13_contract','public.p13_contract_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_contract_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_contract_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        ALTER TABLE p13_contract_chunks DROP CONSTRAINT p13_contract_chunks_pkey;
        ALTER TABLE p13_contract_chunks
          DROP CONSTRAINT p13_contract_chunks_generation_id_ordinal_key;
        "#,
    )
    .expect("projection contract fixture");
    assert_sqlstate(
        "SELECT pgcontext.register_document_source(
             'p13_contract','body','body','source_version',
             'public.p13_contract_chunks','p13_contract_profile'
         )",
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
    );
}

#[pg_test]
fn automatic_chunking_registration_rejects_undeclared_projection_columns() {
    Spi::run(
        r#"
        CREATE TABLE p13_extra_column_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        SELECT pgcontext.create_collection(
            'p13_extra_column','public.p13_extra_column_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_extra_column_chunks'
        );
        ALTER TABLE p13_extra_column_chunks
            ADD COLUMN unbounded_extra jsonb DEFAULT 'null'::jsonb;
        SELECT pgcontext.register_chunking_profile(
            'p13_extra_column_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        "#,
    )
    .expect("extra projection column fixture");
    assert_sqlstate(
        "SELECT pgcontext.register_document_source(
             'p13_extra_column','body','body','source_version',
             'public.p13_extra_column_chunks','p13_extra_column_profile'
         )",
        PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
    );
}

#[pg_test]
fn automatic_chunking_accepts_noncollatable_bigint_source_keys() {
    Spi::run(
        r#"
        CREATE TABLE p13_bigint_key_docs (
            id bigint PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_bigint_key_docs VALUES (1,'bigint key body',1);
        SELECT pgcontext.create_collection(
            'p13_bigint_key','public.p13_bigint_key_docs'
        );
        SELECT pgcontext.create_document_chunk_projection(
            'public.p13_bigint_key_chunks'
        );
        SELECT pgcontext.register_chunking_profile(
            'p13_bigint_key_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_bigint_key','body','body','source_version',
            'public.p13_bigint_key_chunks','p13_bigint_key_profile'
        );
        "#,
    )
    .expect("bigint source-key fixture");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.enqueue_document_chunking(
                 'p13_bigint_key','body',ARRAY['1']
             )"
        )
        .expect("noncollatable bigint key enqueue"),
        Some(1),
    );
}

#[pg_test]
fn automatic_chunking_rebinds_logically_restored_relation_identities() {
    Spi::run(
        r#"
        CREATE TABLE p13_restore_docs (
            id text PRIMARY KEY, body text NOT NULL, source_version bigint NOT NULL
        );
        INSERT INTO p13_restore_docs VALUES ('a','restored body',1);
        SELECT pgcontext.create_collection('p13_restore','public.p13_restore_docs');
        SELECT pgcontext.create_document_chunk_projection('public.p13_restore_chunks');
        SELECT pgcontext.register_chunking_profile(
            'p13_restore_profile','plain_text_v1',8,8,1,0,8388608,false
        );
        SELECT pgcontext.register_document_source(
            'p13_restore','body','body','source_version',
            'public.p13_restore_chunks','p13_restore_profile'
        );
        UPDATE pgcontext._semantic_rerank_sources SET source_table_oid = 0
         WHERE source_name = 'body';
        UPDATE pgcontext._document_sources
           SET projection_table_oid = 0,
               registration_system_identifier = -1,
               registration_database_oid = 0
         WHERE source_name = 'body';
        "#,
    )
    .expect("logical restore fixture");
    let revision_before = Spi::get_one::<i64>(
        "SELECT registration_revision FROM pgcontext._document_sources
          WHERE source_name = 'body'",
    )
    .expect("stored revision")
    .expect("registered source");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pgcontext.enqueue_document_chunking(
                 'p13_restore','body',ARRAY['a']
             )"
        )
        .expect("restored registration enqueue"),
        Some(1),
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT registration_revision FROM pgcontext._document_sources
              WHERE source_name = 'body'"
        )
        .expect("refreshed revision"),
        Some(revision_before),
    );
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT sources.registration_system_identifier = control.system_identifier
                    AND sources.registration_database_oid = database.oid
               FROM pgcontext._document_sources AS sources
               CROSS JOIN pg_catalog.pg_control_system() AS control
               JOIN pg_catalog.pg_database AS database
                 ON database.datname = pg_catalog.current_database()
              WHERE sources.source_name = 'body'"
        )
        .expect("refreshed restore identity"),
        Some(true),
    );
}
