#[pg_test]
fn artifact_segment_diagnostics_report_ready_and_metadata_only_artifacts() {
    let segment_job =
        completed_artifact_build_job("m10_artifact_diagnostics_ready", "segment", "seg-a");
    let metadata_rows = artifact_manifest_rows(&format!(
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));

    let mmap_job = completed_artifact_build_job("m10_artifact_diagnostics_ready", "mmap", "view-a");
    let file_rows = artifact_file_rows(&format!(
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_ready')",
    );

    assert_eq!(
        rows,
        vec![
            ArtifactDiagnosticRow {
                artifact_kind: "segment".to_owned(),
                artifact_name: "seg-a".to_owned(),
                target_name: "public.m10_artifact_diagnostics_ready".to_owned(),
                lifecycle_state: "validated".to_owned(),
                status: "metadata_only".to_owned(),
                detail: "artifact has no materialized file".to_owned(),
                repair_advice: "no materialized file cleanup is pending".to_owned(),
                cleanup_eligible: false,
                relative_path: None,
                payload_bytes: 7,
                file_payload_bytes: None,
                checksum: metadata_rows[0].checksum,
                file_checksum: None,
            },
            ArtifactDiagnosticRow {
                artifact_kind: "mmap".to_owned(),
                artifact_name: "view-a".to_owned(),
                target_name: "public.m10_artifact_diagnostics_ready".to_owned(),
                lifecycle_state: "file_materialized".to_owned(),
                status: "ready".to_owned(),
                detail: "artifact file matches catalog metadata".to_owned(),
                repair_advice: "no repair needed".to_owned(),
                cleanup_eligible: true,
                relative_path: file_rows[0].relative_path.clone(),
                payload_bytes: 1,
                file_payload_bytes: Some(1),
                checksum: file_rows[0].checksum,
                file_checksum: Some(file_rows[0].checksum),
            },
        ]
    );
}

#[pg_test]
fn artifact_segment_diagnostics_report_missing_files_without_erroring() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_missing", "mmap", "view-a");
    let file_rows = artifact_file_rows(&format!(
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
    remove_artifact_file(
        file_rows[0]
            .relative_path
            .as_deref()
            .expect("file row should include a relative path"),
    );

    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_missing')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "artifact_missing");
    assert!(rows[0].detail.contains("artifact file is missing"));
    assert_eq!(
        rows[0].repair_advice,
        "retire the manifest or rebuild the artifact after investigating the missing file"
    );
    assert!(rows[0].cleanup_eligible);
    assert_eq!(rows[0].file_payload_bytes, None);
    assert_eq!(rows[0].file_checksum, None);
}

#[pg_test]
fn artifact_segment_diagnostics_report_checksum_mismatch() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_checksum", "mmap", "view-a");
    let file_rows = artifact_file_rows(&format!(
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));
    flip_last_artifact_file_byte(
        file_rows[0]
            .relative_path
            .as_deref()
            .expect("file row should include a relative path"),
    );

    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_checksum')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "checksum_mismatch");
    assert_eq!(rows[0].detail, "segment checksum mismatch");
    assert_eq!(
        rows[0].repair_advice,
        "retire the manifest and rebuild the artifact from source data"
    );
    assert!(rows[0].cleanup_eligible);
    assert_eq!(rows[0].file_payload_bytes, None);
    assert_eq!(rows[0].file_checksum, None);
}

#[pg_test]
fn artifact_segment_diagnostics_report_catalog_metadata_mismatch() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_metadata", "mmap", "view-a");
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
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET payload_bytes = payload_bytes + 1
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_diagnostics_metadata'",
    )
    .expect("artifact catalog metadata should be updated for drift test");

    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_metadata')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "metadata_mismatch");
    assert_eq!(
        rows[0].detail,
        "artifact file metadata differs from catalog"
    );
    assert_eq!(
        rows[0].repair_advice,
        "retire the manifest and rebuild the artifact from source data"
    );
    assert!(rows[0].cleanup_eligible);
    assert_eq!(rows[0].payload_bytes, 2);
    assert_eq!(rows[0].file_payload_bytes, Some(1));
}

#[pg_test]
fn artifact_segment_diagnostics_reject_catalog_path_escape() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_path", "mmap", "view-a");
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
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../escape.pgctxseg'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_diagnostics_path'",
    )
    .expect("artifact catalog path should be updated for confinement test");

    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_path')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "path_rejected");
    assert_eq!(
        rows[0].detail,
        "artifact relative path is outside pgcontext_artifacts"
    );
    assert_eq!(
        rows[0].repair_advice,
        "fix or remove the invalid catalog path before retiring the artifact"
    );
    assert!(!rows[0].cleanup_eligible);
    assert_eq!(rows[0].relative_path, Some("../escape.pgctxseg".to_owned()));
    assert_eq!(rows[0].file_payload_bytes, None);
    assert_eq!(rows[0].file_checksum, None);
}

#[pg_test]
fn artifact_segment_diagnostics_enforces_collection_visibility() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_acl", "mmap", "view-a");
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
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../escape.pgctxseg'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_diagnostics_acl'",
    )
    .expect("artifact catalog path should be updated for denied-role visibility test");

    sql_test_create_role("m10_artifact_diagnostics_denied");
    sql_test_grant_api_access("m10_artifact_diagnostics_denied");
    sql_test_set_session_user("m10_artifact_diagnostics_denied");
    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_diagnostics_acl')",
    );
    sql_test_reset_session_user();

    assert!(rows.is_empty());
}

#[pg_test]
fn artifact_segment_diagnostics_ignores_hostile_search_path() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_diagnostics_search_path", "mmap", "view-a");
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
    Spi::run("CREATE SCHEMA m10_artifact_diagnostics_shadow")
        .expect("shadow schema should be created");
    Spi::run(
        "CREATE TABLE m10_artifact_diagnostics_shadow._visible_artifact_segments (
             artifact_kind text,
             status text
         )",
    )
    .expect("shadow visible artifact table should be created");
    Spi::run(
        "CREATE FUNCTION m10_artifact_diagnostics_shadow.load_segment_file(text)
         RETURNS text
         LANGUAGE sql
         AS $$ SELECT 'shadow'::text $$",
    )
    .expect("shadow function should be created");

    Spi::run(
        "SET LOCAL search_path =
             m10_artifact_diagnostics_shadow, public, pgcontext, pg_catalog",
    )
    .expect("hostile search_path should be set");
    let rows = artifact_diagnostic_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                status,
                detail,
                repair_advice,
                cleanup_eligible,
                relative_path,
                payload_bytes,
                file_payload_bytes,
                checksum,
                file_checksum
           FROM pgcontext.artifact_segment_diagnostics(
                'm10_artifact_diagnostics_search_path'
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_kind, "mmap");
    assert_eq!(rows[0].status, "ready");
    assert_eq!(rows[0].repair_advice, "no repair needed");
    assert!(rows[0].cleanup_eligible);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactDiagnosticRow {
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    lifecycle_state: String,
    status: String,
    detail: String,
    repair_advice: String,
    cleanup_eligible: bool,
    relative_path: Option<String>,
    payload_bytes: i64,
    file_payload_bytes: Option<i64>,
    checksum: i64,
    file_checksum: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactRetireRow {
    artifact_id: i64,
    collection_name: String,
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    previous_relative_path: Option<String>,
    file_removed: bool,
    lifecycle_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactCleanupRow {
    artifact_id: i64,
    collection_name: String,
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    status: String,
    cleanup_action: String,
    relative_path: Option<String>,
    file_removed: bool,
    lifecycle_state: String,
}

fn artifact_diagnostic_rows(sql: &str) -> Vec<ArtifactDiagnosticRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactDiagnosticRow {
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
                detail: row.get::<String>(6)?.expect("detail should not be null"),
                repair_advice: row
                    .get::<String>(7)?
                    .expect("repair_advice should not be null"),
                cleanup_eligible: row
                    .get::<bool>(8)?
                    .expect("cleanup_eligible should not be null"),
                relative_path: row.get::<String>(9)?,
                payload_bytes: row
                    .get::<i64>(10)?
                    .expect("payload_bytes should not be null"),
                file_payload_bytes: row.get::<i64>(11)?,
                checksum: row.get::<i64>(12)?.expect("checksum should not be null"),
                file_checksum: row.get::<i64>(13)?,
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact diagnostic SQL query should succeed")
}

fn artifact_retire_rows(sql: &str) -> Vec<ArtifactRetireRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactRetireRow {
                artifact_id: row.get::<i64>(1)?.expect("artifact_id should not be null"),
                collection_name: row
                    .get::<String>(2)?
                    .expect("collection_name should not be null"),
                artifact_kind: row
                    .get::<String>(3)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(4)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(5)?
                    .expect("target_name should not be null"),
                previous_relative_path: row.get::<String>(6)?,
                file_removed: row
                    .get::<bool>(7)?
                    .expect("file_removed should not be null"),
                lifecycle_state: row
                    .get::<String>(8)?
                    .expect("lifecycle_state should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact retire SQL query should succeed")
}

fn artifact_cleanup_rows(sql: &str) -> Vec<ArtifactCleanupRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactCleanupRow {
                artifact_id: row.get::<i64>(1)?.expect("artifact_id should not be null"),
                collection_name: row
                    .get::<String>(2)?
                    .expect("collection_name should not be null"),
                artifact_kind: row
                    .get::<String>(3)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(4)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(5)?
                    .expect("target_name should not be null"),
                status: row.get::<String>(6)?.expect("status should not be null"),
                cleanup_action: row
                    .get::<String>(7)?
                    .expect("cleanup_action should not be null"),
                relative_path: row.get::<String>(8)?,
                file_removed: row
                    .get::<bool>(9)?
                    .expect("file_removed should not be null"),
                lifecycle_state: row
                    .get::<String>(10)?
                    .expect("lifecycle_state should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact cleanup SQL query should succeed")
}

fn artifact_absolute_test_path(relative_path: &str) -> std::path::PathBuf {
    let data_directory = Spi::get_one::<String>("SHOW data_directory")
        .expect("data_directory query should succeed")
        .expect("data_directory should not be null");
    std::path::PathBuf::from(data_directory).join(relative_path)
}

fn artifact_collection_id(collection_name: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT collection_id
           FROM pgcontext._collections
          WHERE collection_name = $1",
        &[collection_name.into()],
    )
    .expect("artifact collection id lookup should succeed")
    .expect("artifact collection id should exist")
}

fn write_orphan_artifact_file(relative_path: &str, payload_hex: &str) {
    let path = artifact_absolute_test_path(relative_path);
    std::fs::create_dir_all(
        path.parent()
            .expect("orphan artifact path should have a parent"),
    )
    .expect("orphan artifact directory should be created");
    let bytes = Spi::get_one::<Vec<u8>>(&format!(
        "SELECT pgcontext.encode_artifact_segment('hnsw_graph', decode('{payload_hex}', 'hex'))"
    ))
    .expect("orphan artifact segment should encode")
    .expect("orphan artifact segment bytes should not be null");
    std::fs::write(path, bytes).expect("orphan artifact file should be written");
}

fn remove_artifact_file(relative_path: &str) {
    std::fs::remove_file(artifact_absolute_test_path(relative_path))
        .expect("artifact file should be removed for missing-file test");
}

fn flip_last_artifact_file_byte(relative_path: &str) {
    let path = artifact_absolute_test_path(relative_path);
    let mut bytes = std::fs::read(&path).expect("artifact file should be readable");
    let last = bytes
        .last_mut()
        .expect("artifact file should contain at least one byte");
    *last ^= 1;
    std::fs::write(&path, bytes).expect("artifact file should be writable");
}
