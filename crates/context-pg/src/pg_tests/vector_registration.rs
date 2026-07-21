#[pg_test]
fn create_collection_can_record_source_table() {
    create_source_table("m2_source_table_info", "embedding vector");

    let rows = collection_rows(
        "SELECT collection_id, collection_name, owner_name, table_schema, table_name
           FROM pgcontext.create_collection('m2_source_table_info', 'public.m2_source_table_info')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "m2_source_table_info");
    assert_eq!(rows[0].3.as_deref(), Some("public"));
    assert_eq!(rows[0].4.as_deref(), Some("m2_source_table_info"));
}

#[pg_test]
fn register_vector_records_checked_vector_column_metadata() {
    create_source_table("m2_vector_docs", "embedding vector");
    Spi::run("SELECT pgcontext.create_collection('m2_vector_docs', 'public.m2_vector_docs')")
        .expect("table-backed collection should be created");

    let rows = vector_registration_rows(
        "SELECT collection_name, vector_name, table_schema, table_name, vector_column, dimensions, metric
           FROM pgcontext.register_vector('m2_vector_docs', 'embedding', 'embedding', 1536, 'cosine')",
    );

    assert_eq!(
        rows,
        vec![(
            "m2_vector_docs".to_owned(),
            "embedding".to_owned(),
            "public".to_owned(),
            "m2_vector_docs".to_owned(),
            "embedding".to_owned(),
            1536,
            "cosine".to_owned(),
        )]
    );
}

#[pg_test]
fn attach_hnsw_index_binds_matching_registered_vector() {
    create_source_table("m2_hnsw_binding", "embedding vector");
    Spi::run("SELECT pgcontext.create_collection('m2_hnsw_binding', 'public.m2_hnsw_binding')")
        .expect("collection should be created");
    Spi::run("SELECT pgcontext.register_vector('m2_hnsw_binding', 'embedding', 'embedding', 2, 'l2')")
        .expect("vector should be registered");
    Spi::run("CREATE INDEX m2_hnsw_binding_idx ON public.m2_hnsw_binding USING pgcontext_hnsw (embedding)")
        .expect("HNSW index should be created");
    Spi::run("SELECT pgcontext.attach_hnsw_index('m2_hnsw_binding', 'embedding', 'public.m2_hnsw_binding_idx')")
        .expect("matching HNSW index should attach");
    let bound = Spi::get_one::<bool>(
        "SELECT hnsw_index_oid = 'public.m2_hnsw_binding_idx'::regclass
           FROM pgcontext._collection_vectors
          WHERE collection_id = (SELECT collection_id FROM pgcontext._collections WHERE collection_name = 'm2_hnsw_binding')",
    )
    .expect("binding query should succeed");
    assert_eq!(bound, Some(true));
}

#[pg_test]
fn collection_vectors_reports_default_named_dense_metadata() {
    create_source_table("m2_vector_defaults", "embedding vector");
    Spi::run("SELECT pgcontext.create_collection('m2_vector_defaults', 'public.m2_vector_defaults')")
        .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm2_vector_defaults',
            'embedding',
            'embedding',
            384,
            'cosine'
        )",
    )
    .expect("vector should be registered");

    let rows = vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                hnsw_options::text,
                quantization_options::text,
                status
           FROM pgcontext.collection_vectors('m2_vector_defaults')",
    );

    assert_eq!(
        rows,
        vec![(
            "m2_vector_defaults".to_owned(),
            "embedding".to_owned(),
            "embedding".to_owned(),
            384,
            "cosine".to_owned(),
            "{}".to_owned(),
            "{}".to_owned(),
            "ready".to_owned(),
        )]
    );
}

#[pg_test]
fn configure_vector_records_per_vector_options_and_status() {
    create_source_table("m2_vector_options", "title_embedding vector, body_embedding vector");
    Spi::run("SELECT pgcontext.create_collection('m2_vector_options', 'public.m2_vector_options')")
        .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm2_vector_options',
            'title',
            'title_embedding',
            128,
            'l2'
        )",
    )
    .expect("title vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm2_vector_options',
            'body',
            'body_embedding',
            768,
            'cosine'
        )",
    )
    .expect("body vector should be registered");

    let configured = vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                hnsw_options::text,
                quantization_options::text,
                status
           FROM pgcontext.configure_vector(
                'm2_vector_options',
                'body',
                '{\"m\":16,\"ef_search\":64}'::jsonb,
                '{\"mode\":\"scalar\",\"levels\":256}'::jsonb,
                'building'
           )",
    );

    assert_eq!(
        configured,
        vec![(
            "m2_vector_options".to_owned(),
            "body".to_owned(),
            "body_embedding".to_owned(),
            768,
            "cosine".to_owned(),
            "{\"m\": 16, \"ef_search\": 64}".to_owned(),
            "{\"mode\": \"scalar\", \"levels\": 256}".to_owned(),
            "building".to_owned(),
        )]
    );

    let listed = vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                hnsw_options::text,
                quantization_options::text,
                status
           FROM pgcontext.collection_vectors('m2_vector_options')",
    );

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].1, "body");
    assert_eq!(listed[0].7, "building");
    assert_eq!(listed[1].1, "title");
    assert_eq!(listed[1].5, "{}");
    assert_eq!(listed[1].7, "ready");
}

#[pg_test]
fn vector_config_state_revision_lifecycle() {
    create_source_table("m2_vector_config_revision", "embedding vector");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm2_vector_config_revision',
            'public.m2_vector_config_revision'
        )",
    )
    .expect("collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm2_vector_config_revision', 'embedding', 'embedding', 2, 'l2'
        )",
    )
    .expect("vector should be registered");

    let first_job = start_artifact_build_job(
        "m2_vector_config_revision",
        "mmap",
        "first-generation",
        0,
    );
    Spi::run(&format!("SELECT pgcontext.run_build_job({first_job}, 1)"))
        .expect("first build job should complete");
    Spi::run(&format!(
        "SELECT * FROM pgcontext.publish_artifact_segment_file(
            {first_job},
            pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
        )"
    ))
    .expect("first generation should activate");

    Spi::run(
        "DO $$
         BEGIN
             BEGIN
                 PERFORM pgcontext.configure_vector(
                     'm2_vector_config_revision',
                     'embedding',
                     '{\"m\":8}'::jsonb,
                     '{\"mode\":\"none\"}'::jsonb,
                     'ready'
                 );
                 RAISE EXCEPTION 'rollback configuration revision';
             EXCEPTION WHEN raise_exception THEN
                 NULL;
             END;
         END $$",
    )
    .expect("aborted configuration update should roll back");

    let first_state = Spi::get_two::<String, i64>(
        "SELECT artifacts.lifecycle_state, collections.config_revision
           FROM pgcontext._artifact_segments AS artifacts
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE artifacts.build_job_id = (SELECT min(build_job_id) FROM pgcontext._build_jobs
                                          WHERE collection_id = collections.collection_id)",
    )
    .expect("rolled-back generation state lookup should succeed");
    assert_eq!(first_state, (Some("file_materialized".to_owned()), Some(1)));

    Spi::run(
        "SELECT pgcontext.configure_vector(
            'm2_vector_config_revision',
            'embedding',
            '{\"m\":16}'::jsonb,
            '{\"mode\":\"none\"}'::jsonb,
            'ready'
        )",
    )
    .expect("configuration revision should be recorded");

    let state_and_revision = Spi::get_two::<String, i64>(
        "SELECT artifacts.lifecycle_state, collections.config_revision
           FROM pgcontext._artifact_segments AS artifacts
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE artifacts.build_job_id = (SELECT min(build_job_id) FROM pgcontext._build_jobs
                                          WHERE collection_id = collections.collection_id)",
    )
    .expect("stale generation lookup should succeed");
    assert_eq!(state_and_revision, (Some("rebuild_required".to_owned()), Some(2)));

    let replacement_job = start_artifact_build_job(
        "m2_vector_config_revision",
        "mmap",
        "replacement-generation",
        0,
    );
    Spi::run(&format!("SELECT pgcontext.run_build_job({replacement_job}, 1)"))
        .expect("replacement build job should complete");
    Spi::run(&format!(
        "SELECT * FROM pgcontext.publish_artifact_segment_file(
            {replacement_job},
            pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
        )"
    ))
    .expect("replacement generation should activate");

    let replacement_state = Spi::get_two::<String, i64>(&format!(
        "SELECT artifacts.lifecycle_state, artifacts.config_revision
           FROM pgcontext._artifact_segments AS artifacts
          WHERE artifacts.build_job_id = {replacement_job}"
    ))
    .expect("replacement generation state lookup should succeed");
    assert_eq!(replacement_state, (Some("file_materialized".to_owned()), Some(2)));
}

#[pg_test]
fn configure_vector_rejects_invalid_metadata_with_sqlstates() {
    create_source_table("m2_vector_bad_options", "embedding vector");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_vector_bad_options', 'public.m2_vector_bad_options')",
    )
    .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm2_vector_bad_options',
            'embedding',
            'embedding',
            16,
            'l2'
        )",
    )
    .expect("vector should be registered");

    let cases = [
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'missing',
                '{}'::jsonb,
                '{}'::jsonb,
                'ready'
            )",
            "42704",
            "vector registration does not exist for collection m2_vector_bad_options: missing",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '[]'::jsonb,
                '{}'::jsonb,
                'ready'
            )",
            "22023",
            "hnsw_options must be a JSON object",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '{}'::jsonb,
                '[]'::jsonb,
                'ready'
            )",
            "22023",
            "quantization_options must be a JSON object",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '{}'::jsonb,
                '{\"metadata_version\":999}'::jsonb,
                'ready'
            )",
            "22023",
            "unsupported quantization_options metadata_version: 999",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '{}'::jsonb,
                '{\"mode\":\"future\"}'::jsonb,
                'ready'
            )",
            "22023",
            "unsupported quantization_options mode: future",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '{}'::jsonb,
                '{\"mode\":\"scalar\",\"levels\":1}'::jsonb,
                'ready'
            )",
            "22023",
            "quantization_options levels must be between 2 and 256: 1",
        ),
        (
            "SELECT pgcontext.configure_vector(
                'm2_vector_bad_options',
                'embedding',
                '{}'::jsonb,
                '{}'::jsonb,
                'paused'
            )",
            "22023",
            "unsupported vector status: paused",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        let message = message.replace('\'', "''");
        Spi::run(&format!(
            r#"
            DO $$
            DECLARE
                actual_sqlstate text;
            BEGIN
                BEGIN
                    PERFORM * FROM ({sql}) AS invalid_call;
                    RAISE EXCEPTION 'expected configure_vector failure';
                EXCEPTION WHEN OTHERS THEN
                    GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                    IF actual_sqlstate <> '{sqlstate}' THEN
                        RAISE EXCEPTION 'unexpected configure_vector SQLSTATE: %', actual_sqlstate;
                    END IF;
                    IF SQLERRM <> '{message}' THEN
                        RAISE EXCEPTION 'unexpected configure_vector error: %', SQLERRM;
                    END IF;
                END;
            END $$;
            "#
        ))
        .expect("invalid vector metadata should raise expected error");
    }
}

#[pg_test]
fn register_sparse_vector_records_checked_sparse_column_metadata() {
    create_source_table("m2_sparse_docs", "lexical sparsevec");
    Spi::run("SELECT pgcontext.create_collection('m2_sparse_docs', 'public.m2_sparse_docs')")
        .expect("table-backed collection should be created");

    let rows = sparse_vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                storage_options::text,
                index_options::text,
                status
           FROM pgcontext.register_sparse_vector(
                'm2_sparse_docs',
                'lexical',
                'lexical',
                4096,
                'inner_product'
           )",
    );

    assert_eq!(
        rows,
        vec![(
            "m2_sparse_docs".to_owned(),
            "lexical".to_owned(),
            "lexical".to_owned(),
            4096,
            "inner_product".to_owned(),
            "{}".to_owned(),
            "{}".to_owned(),
            "ready".to_owned(),
        )]
    );
}

#[pg_test]
fn configure_sparse_vector_records_storage_index_options_and_status() {
    create_source_table("m2_sparse_options", "title_sparse sparsevec, body_sparse sparsevec");
    Spi::run("SELECT pgcontext.create_collection('m2_sparse_options', 'public.m2_sparse_options')")
        .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_sparse_vector(
            'm2_sparse_options',
            'title',
            'title_sparse',
            2048,
            'l1'
        )",
    )
    .expect("title sparse vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_sparse_vector(
            'm2_sparse_options',
            'body',
            'body_sparse',
            8192,
            'inner_product'
        )",
    )
    .expect("body sparse vector should be registered");

    let configured = sparse_vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                storage_options::text,
                index_options::text,
                status
           FROM pgcontext.configure_sparse_vector(
                'm2_sparse_options',
                'body',
                '{\"format\":\"posting_lists\",\"source\":\"bm25\"}'::jsonb,
                '{\"strategy\":\"exact\",\"rerank\":true}'::jsonb,
                'building'
           )",
    );

    assert_eq!(
        configured,
        vec![(
            "m2_sparse_options".to_owned(),
            "body".to_owned(),
            "body_sparse".to_owned(),
            8192,
            "inner_product".to_owned(),
            "{\"format\": \"posting_lists\", \"source\": \"bm25\"}".to_owned(),
            "{\"rerank\": true, \"strategy\": \"exact\"}".to_owned(),
            "building".to_owned(),
        )]
    );

    let listed = sparse_vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                storage_options::text,
                index_options::text,
                status
           FROM pgcontext.collection_sparse_vectors('m2_sparse_options')",
    );

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].1, "body");
    assert_eq!(listed[0].7, "building");
    assert_eq!(listed[1].1, "title");
    assert_eq!(listed[1].5, "{}");
    assert_eq!(listed[1].7, "ready");
}

#[pg_test]
fn register_sparse_vector_rejects_invalid_inputs_with_sqlstates() {
    Spi::run("SELECT pgcontext.create_collection('m2_sparse_no_table')")
        .expect("tableless collection should be created");
    create_source_table("m2_sparse_wrong_type", "lexical text");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_sparse_wrong_type', 'public.m2_sparse_wrong_type')",
    )
    .expect("wrong-type collection should be created");
    create_source_table("m2_sparse_duplicate", "lexical sparsevec");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_sparse_duplicate', 'public.m2_sparse_duplicate')",
    )
    .expect("duplicate collection should be created");
    Spi::run(
        "SELECT pgcontext.register_sparse_vector(
            'm2_sparse_duplicate',
            'lexical',
            'lexical',
            128,
            'l2'
        )",
    )
    .expect("initial sparse vector registration should succeed");

    let cases = [
        (
            "SELECT pgcontext.register_sparse_vector(
                'm2_sparse_missing',
                'lexical',
                'lexical',
                128,
                'l2'
            )",
            "42704",
            "collection does not exist: m2_sparse_missing",
        ),
        (
            "SELECT pgcontext.register_sparse_vector(
                'm2_sparse_no_table',
                'lexical',
                'lexical',
                128,
                'l2'
            )",
            "22023",
            "collection has no source table: m2_sparse_no_table",
        ),
        (
            "SELECT pgcontext.register_sparse_vector(
                'm2_sparse_wrong_type',
                'lexical',
                'lexical',
                128,
                'l2'
            )",
            "42804",
            "sparse vector column must have type sparsevec: m2_sparse_wrong_type.lexical is text",
        ),
        (
            "SELECT pgcontext.register_sparse_vector(
                'm2_sparse_duplicate',
                'lexical',
                'lexical',
                128,
                'l2'
            )",
            "42710",
            "sparse vector registration already exists for collection m2_sparse_duplicate: lexical",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_sql_failure(sql, sqlstate, message, "register_sparse_vector");
    }
}

#[pg_test]
fn register_sparse_vector_accepts_cosine_metric() {
    create_source_table("m2_sparse_cosine", "lexical sparsevec");
    Spi::run("SELECT pgcontext.create_collection('m2_sparse_cosine', 'public.m2_sparse_cosine')")
        .expect("sparse cosine collection should be created");

    let rows = sparse_vector_metadata_rows(
        "SELECT collection_name,
                vector_name,
                vector_column,
                dimensions,
                metric,
                storage_options::text,
                index_options::text,
                status
           FROM pgcontext.register_sparse_vector(
                'm2_sparse_cosine',
                'lexical',
                'lexical',
                128,
                'cosine'
           )",
    );

    assert_eq!(
        rows,
        vec![(
            "m2_sparse_cosine".to_owned(),
            "lexical".to_owned(),
            "lexical".to_owned(),
            128,
            "cosine".to_owned(),
            "{}".to_owned(),
            "{}".to_owned(),
            "ready".to_owned(),
        )]
    );
}

#[pg_test]
fn configure_sparse_vector_rejects_invalid_metadata_with_sqlstates() {
    create_source_table("m2_sparse_bad_options", "lexical sparsevec");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_sparse_bad_options', 'public.m2_sparse_bad_options')",
    )
    .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_sparse_vector(
            'm2_sparse_bad_options',
            'lexical',
            'lexical',
            16,
            'l2'
        )",
    )
    .expect("sparse vector should be registered");

    let cases = [
        (
            "SELECT pgcontext.configure_sparse_vector(
                'm2_sparse_bad_options',
                'missing',
                '{}'::jsonb,
                '{}'::jsonb,
                'ready'
            )",
            "42704",
            "sparse vector registration does not exist for collection m2_sparse_bad_options: missing",
        ),
        (
            "SELECT pgcontext.configure_sparse_vector(
                'm2_sparse_bad_options',
                'lexical',
                '[]'::jsonb,
                '{}'::jsonb,
                'ready'
            )",
            "22023",
            "storage_options must be a JSON object",
        ),
        (
            "SELECT pgcontext.configure_sparse_vector(
                'm2_sparse_bad_options',
                'lexical',
                '{}'::jsonb,
                '[]'::jsonb,
                'ready'
            )",
            "22023",
            "index_options must be a JSON object",
        ),
        (
            "SELECT pgcontext.configure_sparse_vector(
                'm2_sparse_bad_options',
                'lexical',
                '{}'::jsonb,
                '{}'::jsonb,
                'paused'
            )",
            "22023",
            "unsupported vector status: paused",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_sql_failure(sql, sqlstate, message, "configure_sparse_vector");
    }
}

#[pg_test]
#[should_panic(expected = "source table does not exist: public.m2_missing_table")]
fn create_collection_rejects_missing_source_tables() {
    Spi::run("SELECT pgcontext.create_collection('m2_missing_table', 'public.m2_missing_table')")
        .expect("missing source table should fail");
}

#[pg_test]
#[should_panic(expected = "vector column does not exist on public.m2_missing_column: embedding")]
fn register_vector_rejects_missing_columns() {
    create_source_table("m2_missing_column", "body text");
    Spi::run("SELECT pgcontext.create_collection('m2_missing_column', 'public.m2_missing_column')")
        .expect("table-backed collection should be created");

    Spi::run("SELECT pgcontext.register_vector('m2_missing_column', 'embedding', 'embedding', 3, 'l2')")
        .expect("missing vector column should fail");
}

#[pg_test]
#[should_panic(expected = "vector column must have type vector: m2_wrong_column_type.embedding is text")]
fn register_vector_rejects_non_vector_columns() {
    create_source_table("m2_wrong_column_type", "embedding text");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_wrong_column_type', 'public.m2_wrong_column_type')",
    )
    .expect("table-backed collection should be created");

    Spi::run(
        "SELECT pgcontext.register_vector('m2_wrong_column_type', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("non-vector column should fail");
}

#[pg_test]
#[should_panic(expected = "vector registration already exists for collection m2_duplicate_vector: embedding")]
fn register_vector_rejects_duplicate_vector_names() {
    create_source_table("m2_duplicate_vector", "embedding vector");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_duplicate_vector', 'public.m2_duplicate_vector')",
    )
    .expect("table-backed collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector('m2_duplicate_vector', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("initial vector registration should succeed");

    Spi::run(
        "SELECT pgcontext.register_vector('m2_duplicate_vector', 'embedding', 'embedding', 3, 'l2')",
    )
    .expect("duplicate vector registration should fail");
}

#[pg_test]
#[should_panic(expected = "invalid vector dimensions: 0")]
fn register_vector_rejects_invalid_dimensions() {
    create_source_table("m2_invalid_dimensions", "embedding vector");
    Spi::run(
        "SELECT pgcontext.create_collection('m2_invalid_dimensions', 'public.m2_invalid_dimensions')",
    )
    .expect("table-backed collection should be created");

    Spi::run(
        "SELECT pgcontext.register_vector('m2_invalid_dimensions', 'embedding', 'embedding', 0, 'l2')",
    )
    .expect("invalid dimensions should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported distance metric: hamming")]
fn register_vector_rejects_unsupported_metrics() {
    create_source_table("m2_bad_metric", "embedding vector");
    Spi::run("SELECT pgcontext.create_collection('m2_bad_metric', 'public.m2_bad_metric')")
        .expect("table-backed collection should be created");

    Spi::run("SELECT pgcontext.register_vector('m2_bad_metric', 'embedding', 'embedding', 3, 'hamming')")
        .expect("unsupported metric should fail");
}

fn create_source_table(table_name: &str, vector_column_sql: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             {vector_column_sql}
         )"
    ))
    .expect("source table should be created");
}

fn vector_registration_rows(sql: &str) -> Vec<(String, String, String, String, String, i32, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?.expect("vector_name should not be null"),
                row.get::<String>(3)?.expect("table_schema should not be null"),
                row.get::<String>(4)?.expect("table_name should not be null"),
                row.get::<String>(5)?
                    .expect("vector_column should not be null"),
                row.get::<i32>(6)?.expect("dimensions should not be null"),
                row.get::<String>(7)?.expect("metric should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("vector registration rows query failed")
}

fn vector_metadata_rows(
    sql: &str,
) -> Vec<(String, String, String, i32, String, String, String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?.expect("vector_name should not be null"),
                row.get::<String>(3)?.expect("vector_column should not be null"),
                row.get::<i32>(4)?.expect("dimensions should not be null"),
                row.get::<String>(5)?.expect("metric should not be null"),
                row.get::<String>(6)?.expect("hnsw_options should not be null"),
                row.get::<String>(7)?
                    .expect("quantization_options should not be null"),
                row.get::<String>(8)?.expect("status should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("vector metadata rows query failed")
}

fn sparse_vector_metadata_rows(
    sql: &str,
) -> Vec<(String, String, String, i32, String, String, String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?.expect("vector_name should not be null"),
                row.get::<String>(3)?.expect("vector_column should not be null"),
                row.get::<i32>(4)?.expect("dimensions should not be null"),
                row.get::<String>(5)?.expect("metric should not be null"),
                row.get::<String>(6)?.expect("storage_options should not be null"),
                row.get::<String>(7)?.expect("index_options should not be null"),
                row.get::<String>(8)?.expect("status should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("sparse vector metadata rows query failed")
}

fn assert_sql_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                PERFORM * FROM ({sql}) AS invalid_call;
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %', actual_sqlstate;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .expect("invalid sparse vector metadata call should raise expected error");
}
