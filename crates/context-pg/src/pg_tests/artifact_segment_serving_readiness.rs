#[pg_test]
fn artifact_segment_serving_readiness_gates_mmap_files_by_budget() {
    let segment_job = completed_artifact_build_job("m10_artifact_serving", "segment", "seg-a");
    artifact_manifest_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment(
                {segment_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let mmap_job = completed_artifact_build_job("m10_artifact_serving", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {mmap_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));

    let rows = artifact_serving_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                serving_ready,
                mapped_bytes,
                max_mapped_bytes,
                detail
           FROM pgcontext.artifact_segment_serving_readiness('m10_artifact_serving', 42)",
    );

    assert_eq!(
        rows,
        vec![
            ArtifactServingRow {
                artifact_kind: "segment".to_owned(),
                artifact_name: "seg-a".to_owned(),
                target_name: "public.m10_artifact_serving".to_owned(),
                lifecycle_state: "validated".to_owned(),
                status: "not_mmap_artifact".to_owned(),
                serving_ready: false,
                mapped_bytes: 41,
                max_mapped_bytes: 42,
                detail: "only mmap artifacts can be serving-ready".to_owned(),
            },
            ArtifactServingRow {
                artifact_kind: "mmap".to_owned(),
                artifact_name: "view-a".to_owned(),
                target_name: "public.m10_artifact_serving".to_owned(),
                lifecycle_state: "file_materialized".to_owned(),
                status: "ready".to_owned(),
                serving_ready: true,
                mapped_bytes: 42,
                max_mapped_bytes: 42,
                detail: "artifact file matches catalog metadata and memory budget".to_owned(),
            },
        ]
    );

    let budget_rows = artifact_serving_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                serving_ready,
                mapped_bytes,
                max_mapped_bytes,
                detail
           FROM pgcontext.artifact_segment_serving_readiness('m10_artifact_serving', 41)
          WHERE artifact_kind = 'mmap'",
    );
    assert_eq!(budget_rows.len(), 1);
    assert_eq!(budget_rows[0].status, "memory_budget_exceeded");
    assert!(!budget_rows[0].serving_ready);
    assert_eq!(budget_rows[0].mapped_bytes, 42);
    assert_eq!(budget_rows[0].max_mapped_bytes, 41);
}

#[pg_test]
fn artifact_segment_mmap_payload_serves_only_ready_mmap_files() {
    let mmap_job =
        completed_artifact_build_job("m10_artifact_mmap_payload", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {mmap_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('010203', 'hex'))
           )"
    ));

    let rows = artifact_mmap_payload_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                mapped_bytes,
                payload
           FROM pgcontext.artifact_segment_mmap_payload(
                'm10_artifact_mmap_payload',
                'view-a',
                43
           )",
    );
    assert_eq!(
        rows,
        vec![ArtifactMmapPayloadRow {
            artifact_kind: "mmap".to_owned(),
            artifact_name: "view-a".to_owned(),
            target_name: "public.m10_artifact_mmap_payload".to_owned(),
            mapped_bytes: 43,
            payload: vec![1, 2, 3],
        }]
    );
}

#[pg_test]
fn artifact_segment_mmap_payload_prefers_mmap_when_names_collide() {
    let segment_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_collision", "segment", "shared");
    artifact_manifest_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment(
                {segment_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('ff', 'hex'))
           )"
    ));

    let mmap_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_collision", "mmap", "shared");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {mmap_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));

    let rows = artifact_mmap_payload_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                mapped_bytes,
                payload
           FROM pgcontext.artifact_segment_mmap_payload(
                'm10_artifact_mmap_payload_collision',
                'shared',
                42
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_kind, "mmap");
    assert_eq!(rows[0].artifact_name, "shared");
    assert_eq!(rows[0].payload, vec![1, 2]);
}

#[pg_test]
fn artifact_segment_mmap_payload_rejects_not_ready_artifacts() {
    let metadata_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_bad", "mmap", "metadata-only");
    artifact_manifest_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment(
                {metadata_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let mmap_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_bad", "mmap", "view-a");
    let rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {mmap_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    let relative_path = rows[0]
        .relative_path
        .as_ref()
        .expect("materialized artifact should record a path");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_bad',
             'view-a',
             41
        )",
        "55000",
        "mmap artifact is not serving-ready: memory_budget_exceeded (artifact mapped bytes exceed the serving memory budget)",
        "over-budget mmap artifacts are not served",
    );
    remove_artifact_file(relative_path);

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_bad',
             'metadata-only',
             100
        )",
        "55000",
        "serving-ready mmap artifact not found: m10_artifact_mmap_payload_bad/metadata-only",
        "metadata-only artifacts are not serving-ready",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_bad',
             'view-a',
             100
        )",
        "55000",
        "mmap artifact is not serving-ready: artifact_missing (artifact file is missing)",
        "missing files are not served",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_bad',
             'missing-artifact',
             100
        )",
        "55000",
        "serving-ready mmap artifact not found: m10_artifact_mmap_payload_bad/missing-artifact",
        "unknown artifact names fail closed",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_bad',
             'metadata-only',
             -1
         )",
        "22023",
        "max_mapped_bytes must be non-negative: -1",
        "negative serving budget",
    );
}

#[pg_test]
fn artifact_segment_mmap_payload_rejects_checksum_and_metadata_drift() {
    let checksum_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_drift", "mmap", "checksum");
    let checksum_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {checksum_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    flip_last_artifact_file_byte(
        checksum_rows[0]
            .relative_path
            .as_ref()
            .expect("checksum drift artifact should record a path"),
    );

    let metadata_job =
        completed_artifact_build_job("m10_artifact_mmap_payload_drift", "mmap", "metadata");
    let metadata_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {metadata_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    Spi::run(&format!(
        "UPDATE pgcontext._artifact_segments
            SET payload_bytes = payload_bytes + 1
          WHERE artifact_id = {}",
        metadata_rows[0].artifact_id
    ))
    .expect("test should simulate mmap payload metadata drift");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_drift',
             'checksum',
             100
        )",
        "XX001",
        "mmap artifact is not serving-ready: checksum_mismatch (segment checksum mismatch)",
        "checksum drift mmap payload is not served",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
             'm10_artifact_mmap_payload_drift',
             'metadata',
             100
        )",
        "XX001",
        "mmap artifact is not serving-ready: metadata_mismatch (artifact file metadata differs from catalog)",
        "metadata drift mmap payload is not served",
    );
}

#[pg_test]
fn artifact_segment_mmap_payload_enforces_collection_visibility() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_mmap_payload_acl", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    sql_test_create_role("m10_artifact_payload_denied");
    sql_test_grant_api_access("m10_artifact_payload_denied");
    with_sql_session_user("m10_artifact_payload_denied", || {
        shared_assert_sql_failure(
            "SELECT * FROM pgcontext.artifact_segment_mmap_payload(
                 'm10_artifact_mmap_payload_acl',
                 'view-a',
                 100
            )",
            "42501",
            "raw mmap artifact payload access is internal",
            "non-superuser cannot read raw artifact bytes",
        );
    });
}

#[pg_test]
fn artifact_segment_mmap_payload_ignores_hostile_search_path() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_mmap_payload_search_path", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    Spi::run("CREATE SCHEMA m10_artifact_mmap_payload_shadow")
        .expect("shadow schema should be created");
    Spi::run(
        "CREATE TABLE m10_artifact_mmap_payload_shadow._visible_artifact_segments (
             artifact_kind text,
             artifact_name text,
             payload bytea
         )",
    )
    .expect("shadow visible artifact table should be created");
    Spi::run(
        "CREATE FUNCTION m10_artifact_mmap_payload_shadow.load_segment_file(text)
         RETURNS bytea
         LANGUAGE sql
         AS $$ SELECT decode('ff', 'hex') $$",
    )
    .expect("shadow function should be created");

    Spi::run(
        "SET LOCAL search_path =
             m10_artifact_mmap_payload_shadow, public, pgcontext, pg_catalog",
    )
    .expect("hostile search_path should be set");
    let rows = artifact_mmap_payload_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                mapped_bytes,
                payload
           FROM pgcontext.artifact_segment_mmap_payload(
                'm10_artifact_mmap_payload_search_path',
                'view-a',
                100
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_kind, "mmap");
    assert_eq!(rows[0].artifact_name, "view-a");
    assert_eq!(rows[0].payload, vec![1]);
}

#[pg_test]
fn artifact_segment_serving_readiness_rejects_bad_inputs_and_files() {
    create_artifact_collection("m10_artifact_serving_bad");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.artifact_segment_serving_readiness('m10_artifact_serving_bad', -1)",
        "22023",
        "max_mapped_bytes must be non-negative: -1",
        "negative serving memory budget",
    );

    let metadata_job =
        completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "metadata-only");
    artifact_manifest_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment(
                {metadata_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let mmap_job = completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "view-a");
    let rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {mmap_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    let relative_path = rows[0]
        .relative_path
        .as_ref()
        .expect("materialized artifact should record a path");
    flip_last_artifact_file_byte(relative_path);

    let missing_job = completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "missing");
    let missing_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {missing_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    remove_artifact_file(
        missing_rows[0]
            .relative_path
            .as_ref()
            .expect("missing test artifact should record a path"),
    );

    let drift_job = completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "drift");
    let drift_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {drift_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    Spi::run(&format!(
        "UPDATE pgcontext._artifact_segments
            SET payload_bytes = payload_bytes + 1
          WHERE artifact_id = {}",
        drift_rows[0].artifact_id
    ))
    .expect("test should simulate catalog metadata drift");

    let escape_job = completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "escape");
    let escape_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {escape_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    Spi::run(&format!(
        "UPDATE pgcontext._artifact_segments
            SET relative_path = '../escape.pgctxseg'
          WHERE artifact_id = {}",
        escape_rows[0].artifact_id
    ))
    .expect("test should simulate path escape");

    let corrupt_job = completed_artifact_build_job("m10_artifact_serving_bad", "mmap", "corrupt");
    let corrupt_rows = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {corrupt_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    let corrupt_path = corrupt_rows[0]
        .relative_path
        .as_ref()
        .expect("corrupt test artifact should record a path");
    std::fs::write(artifact_absolute_test_path(corrupt_path), b"not a segment")
        .expect("test should overwrite artifact file with malformed bytes");

    let bad_rows = artifact_serving_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                serving_ready,
                mapped_bytes,
                max_mapped_bytes,
                detail
           FROM pgcontext.artifact_segment_serving_readiness('m10_artifact_serving_bad', 100)",
    );
    let statuses = bad_rows
        .iter()
        .map(|row| (row.artifact_name.as_str(), row.status.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            ("metadata-only", "not_file_materialized"),
            ("view-a", "checksum_mismatch"),
            ("missing", "artifact_missing"),
            ("drift", "metadata_mismatch"),
            ("escape", "path_rejected"),
            ("corrupt", "artifact_corrupt"),
        ]
    );
    assert!(bad_rows.iter().all(|row| !row.serving_ready));
}

#[pg_test]
fn artifact_segment_serving_readiness_enforces_collection_visibility() {
    let build_job_id = completed_artifact_build_job("m10_artifact_serving_acl", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    sql_test_create_role("m10_artifact_serving_denied");
    sql_test_grant_api_access("m10_artifact_serving_denied");
    let rows = with_sql_session_user("m10_artifact_serving_denied", || {
        artifact_serving_rows(
            "SELECT artifact_kind,
                    artifact_name,
                    target_name,
                    lifecycle_state,
                    status,
                    serving_ready,
                    mapped_bytes,
                    max_mapped_bytes,
                    detail
               FROM pgcontext.artifact_segment_serving_readiness('m10_artifact_serving_acl', 100)",
        )
    });

    assert!(rows.is_empty());
}

#[pg_test]
fn artifact_segment_serving_readiness_ignores_hostile_search_path() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_serving_search_path", "mmap", "view-a");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    Spi::run("CREATE SCHEMA m10_artifact_serving_shadow")
        .expect("shadow schema should be created");
    Spi::run(
        "CREATE TABLE m10_artifact_serving_shadow._visible_artifact_segments (
             artifact_kind text,
             status text
         )",
    )
    .expect("shadow visible artifact table should be created");
    Spi::run(
        "CREATE FUNCTION m10_artifact_serving_shadow.load_segment_file(text)
         RETURNS text
         LANGUAGE sql
         AS $$ SELECT 'shadow'::text $$",
    )
    .expect("shadow function should be created");

    Spi::run(
        "SET LOCAL search_path =
             m10_artifact_serving_shadow, public, pgcontext, pg_catalog",
    )
    .expect("hostile search_path should be set");
    let rows = artifact_serving_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                serving_ready,
                mapped_bytes,
                max_mapped_bytes,
                detail
           FROM pgcontext.artifact_segment_serving_readiness(
                'm10_artifact_serving_search_path',
                100
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_kind, "mmap");
    assert_eq!(rows[0].artifact_name, "view-a");
    assert_eq!(rows[0].status, "ready");
    assert!(rows[0].serving_ready);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactServingRow {
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    lifecycle_state: String,
    status: String,
    serving_ready: bool,
    mapped_bytes: i64,
    max_mapped_bytes: i64,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactMmapPayloadRow {
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    mapped_bytes: i64,
    payload: Vec<u8>,
}

fn artifact_serving_rows(sql: &str) -> Vec<ArtifactServingRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactServingRow {
                artifact_kind: row
                    .get::<String>(1)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(2)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(3)?
                    .expect("target_name should not be null"),
                lifecycle_state: row
                    .get::<String>(4)?
                    .expect("lifecycle_state should not be null"),
                status: row.get::<String>(5)?.expect("status should not be null"),
                serving_ready: row
                    .get::<bool>(6)?
                    .expect("serving_ready should not be null"),
                mapped_bytes: row
                    .get::<i64>(7)?
                    .expect("mapped_bytes should not be null"),
                max_mapped_bytes: row
                    .get::<i64>(8)?
                    .expect("max_mapped_bytes should not be null"),
                detail: row.get::<String>(9)?.expect("detail should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact serving SQL query should succeed")
}

fn artifact_mmap_payload_rows(sql: &str) -> Vec<ArtifactMmapPayloadRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactMmapPayloadRow {
                artifact_kind: row
                    .get::<String>(1)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(2)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(3)?
                    .expect("target_name should not be null"),
                mapped_bytes: row
                    .get::<i64>(4)?
                    .expect("mapped_bytes should not be null"),
                payload: row.get::<Vec<u8>>(5)?.expect("payload should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact mmap payload SQL query should succeed")
}

fn with_sql_session_user<T>(role_name: &str, action: impl FnOnce() -> T) -> T {
    sql_test_set_session_user(role_name);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    sql_test_reset_session_user();
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
