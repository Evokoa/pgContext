#[pg_test]
fn artifact_segment_sql_round_trips_hnsw_payload_metadata() {
    let rows = artifact_segment_rows(
        "SELECT kind, payload_bytes, checksum
           FROM pgcontext.validate_artifact_segment(
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )",
    );

    assert_eq!(rows.len(), 1);
    let (kind, payload_bytes, checksum) = &rows[0];
    assert_eq!(kind, "hnsw_graph");
    assert_eq!(*payload_bytes, 7);
    assert_ne!(*checksum, 0);
}
#[pg_test]
fn artifact_segment_sql_accepts_empty_payloads() {
    let rows = artifact_segment_rows(
        "SELECT kind, payload_bytes, checksum
           FROM pgcontext.validate_artifact_segment(
                pgcontext.encode_artifact_segment('hnsw_graph', decode('', 'hex'))
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "hnsw_graph");
    assert_eq!(rows[0].1, 0);
}

#[pg_test]
fn artifact_file_publish_failpoints_never_expose_a_partial_catalog_generation() {
    let build_job_id = completed_artifact_build_job("m10_artifact_publish_failpoints", "mmap", "view-a");
    for failpoint in [
        "before_output_write",
        "before_file_fsync",
        "before_rename",
        "before_directory_fsync",
        "before_catalog_activate",
        "before_retire",
    ] {
        Spi::run(&format!(
            "SELECT pgcontext.test_set_artifact_publish_failpoint('{failpoint}')"
        ))
        .expect("artifact failpoint should be configured");
        shared_assert_sql_failure(
            &format!(
                "SELECT * FROM pgcontext.publish_artifact_segment_file(
                     {build_job_id},
                     pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
                 )"
            ),
            "XX000",
            &format!("injected artifact publication failpoint: {failpoint}"),
            failpoint,
        );
        assert_eq!(count_artifact_manifests("m10_artifact_publish_failpoints"), 0);
    }
    Spi::run("SELECT pgcontext.test_set_artifact_publish_failpoint(NULL)")
        .expect("artifact failpoint should be cleared");
    let rows = artifact_file_rows(&format!(
        "SELECT artifact_id, collection_name, build_job_id, artifact_kind, artifact_name,
                target_name, segment_kind, format_version, payload_bytes, checksum,
                relative_path, lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle_state, "file_materialized");
}

#[pg_test]
fn artifact_segment_sql_rejects_unknown_kind_with_sqlstate() {
    shared_assert_sql_failure(
        "SELECT pgcontext.encode_artifact_segment('future_kind', decode('00', 'hex'))",
        "22023",
        "unsupported segment kind: future_kind",
        "unknown segment kind",
    );
}

#[pg_test]
fn artifact_segment_sql_rejects_truncated_header_with_sqlstate() {
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.validate_artifact_segment(decode('50474354534547', 'hex'))",
        "XX001",
        "truncated segment header: 7 < 40",
        "truncated segment header",
    );
}

#[pg_test]
fn artifact_segment_sql_rejects_checksum_mismatch_with_sqlstate() {
    shared_assert_sql_failure(
        "WITH encoded AS (
             SELECT pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex')) AS segment
         )
         SELECT * FROM pgcontext.validate_artifact_segment((
             SELECT set_byte(
                 segment,
                 length(segment) - 1,
                 get_byte(segment, length(segment) - 1) # 1
             )
             FROM encoded
         ))",
        "XX001",
        "segment checksum mismatch",
        "checksum mismatch",
    );
}

#[pg_test]
fn artifact_segment_publish_records_completed_build_metadata() {
    let build_job_id = completed_artifact_build_job("m10_artifact_publish", "segment", "seg-a");
    let rows = artifact_manifest_rows(&format!(
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
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].collection_name, "m10_artifact_publish");
    assert_eq!(rows[0].build_job_id, build_job_id);
    assert_eq!(rows[0].artifact_kind, "segment");
    assert_eq!(rows[0].artifact_name, "seg-a");
    assert_eq!(rows[0].target_name, "public.m10_artifact_publish");
    assert_eq!(rows[0].segment_kind, "hnsw_graph");
    assert_eq!(rows[0].format_version, 1);
    assert_eq!(rows[0].payload_bytes, 7);
    assert_ne!(rows[0].checksum, 0);
    assert_eq!(rows[0].lifecycle_state, "validated");

    let listed = artifact_manifest_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_publish')",
    );
    assert_eq!(listed, rows);
}

#[pg_test]
fn artifact_segment_publish_supersedes_existing_target_with_a_new_generation() {
    let first_job = completed_artifact_build_job("m10_artifact_replace", "mmap", "view-a");
    let first_rows = artifact_manifest_rows(&format!(
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
                {first_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let second_job = completed_artifact_build_job("m10_artifact_replace", "mmap", "view-a");
    let rows = artifact_manifest_rows(&format!(
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
                {second_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));

    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(rows[0].build_job_id, second_job);
    assert_eq!(rows[0].payload_bytes, 2);
    assert_eq!(rows[0].lifecycle_state, "validated");
    // Metadata-only publication appends a new catalog generation; earlier
    // generations stay listed until retire/cleanup reclaims them.
    assert_eq!(count_artifact_manifests("m10_artifact_replace"), 2);

    let listed = artifact_manifest_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_replace')",
    );
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], first_rows[0]);
    assert_eq!(listed[1], rows[0]);
}

#[pg_test]
fn artifact_segment_publish_rejects_corruption_without_manifest() {
    let build_job_id = completed_artifact_build_job("m10_artifact_corrupt", "segment", "seg-a");
    shared_assert_sql_failure(
        &format!(
            "WITH encoded AS (
                 SELECT pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex')) AS segment
             )
             SELECT * FROM pgcontext.publish_artifact_segment({build_job_id}, (
                 SELECT set_byte(
                     segment,
                     length(segment) - 1,
                     get_byte(segment, length(segment) - 1) # 1
                 )
                 FROM encoded
             ))"
        ),
        "XX001",
        "segment checksum mismatch",
        "publish checksum mismatch",
    );

    assert_eq!(count_artifact_manifests("m10_artifact_corrupt"), 0);
}

#[pg_test]
fn artifact_segment_publish_rejects_non_terminal_and_unsupported_jobs() {
    create_artifact_collection("m10_artifact_bad_status");
    let running = start_artifact_build_job("m10_artifact_bad_status", "segment", "running", 2);
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {running},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        &format!("cannot publish artifact for build job {running} in status running"),
        "publish running job",
    );

    let failed = start_artifact_build_job("m10_artifact_bad_status", "segment", "failed", 2);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({failed}, 1, 'failed', 'synthetic failure')"
    ))
    .expect("segment build job should be marked failed");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {failed},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        &format!("cannot publish artifact for build job {failed} in status failed"),
        "publish failed job",
    );

    let cancelled =
        start_artifact_build_job("m10_artifact_bad_status", "segment", "cancelled", 2);
    Spi::run(&format!("SELECT pgcontext.request_build_cancel({cancelled})"))
        .expect("segment build job cancel should be requested");
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({cancelled}, 0, 'cancelled', 'synthetic cancel')"
    ))
    .expect("segment build job should be marked cancelled");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {cancelled},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        &format!("cannot publish artifact for build job {cancelled} in status cancelled"),
        "publish cancelled job",
    );

    let abandoned =
        start_artifact_build_job("m10_artifact_bad_status", "segment", "abandoned", 2);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'abandoned',
                backend_pid = NULL,
                backend_identity = NULL,
                completed_at = pg_catalog.now()
          WHERE build_job_id = {abandoned}"
    ))
    .expect("segment build job should be marked abandoned");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {abandoned},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        &format!("cannot publish artifact for build job {abandoned} in status abandoned"),
        "publish abandoned job",
    );

    let index = start_artifact_build_job("m10_artifact_bad_status", "index", "idx", 1);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({index}, 1, 'completed')"
    ))
    .expect("index build job should be marked completed for unsupported publish test");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {index},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        "artifact publication supports only segment and mmap jobs for now: index",
        "publish unsupported job kind",
    );
    assert_eq!(count_artifact_manifests("m10_artifact_bad_status"), 0);
}

#[pg_test]
fn artifact_segment_file_publish_rejects_non_terminal_and_unsupported_jobs() {
    create_artifact_collection("m10_artifact_file_bad_status");
    let running =
        start_artifact_build_job("m10_artifact_file_bad_status", "mmap", "running", 2);
    assert_artifact_file_publish_status_failure(
        running,
        "running",
        "publish file running job",
    );

    let failed = start_artifact_build_job("m10_artifact_file_bad_status", "mmap", "failed", 2);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({failed}, 1, 'failed', 'synthetic failure')"
    ))
    .expect("mmap build job should be marked failed");
    assert_artifact_file_publish_status_failure(failed, "failed", "publish file failed job");

    let cancelled =
        start_artifact_build_job("m10_artifact_file_bad_status", "mmap", "cancelled", 2);
    Spi::run(&format!("SELECT pgcontext.request_build_cancel({cancelled})"))
        .expect("mmap build job cancel should be requested");
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({cancelled}, 0, 'cancelled', 'synthetic cancel')"
    ))
    .expect("mmap build job should be marked cancelled");
    assert_artifact_file_publish_status_failure(
        cancelled,
        "cancelled",
        "publish file cancelled job",
    );

    let abandoned =
        start_artifact_build_job("m10_artifact_file_bad_status", "mmap", "abandoned", 2);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'abandoned',
                backend_pid = NULL,
                backend_identity = NULL,
                completed_at = pg_catalog.now()
          WHERE build_job_id = {abandoned}"
    ))
    .expect("mmap build job should be marked abandoned");
    assert_artifact_file_publish_status_failure(
        abandoned,
        "abandoned",
        "publish file abandoned job",
    );

    let index = start_artifact_build_job("m10_artifact_file_bad_status", "index", "idx", 1);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({index}, 1, 'completed')"
    ))
    .expect("index build job should be marked completed for unsupported file publish test");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment_file(
                {index},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        "artifact publication supports only segment and mmap jobs for now: index",
        "publish file unsupported job kind",
    );
    assert!(
        !artifact_file_exists(&artifact_file_path_for_build_job(index)),
        "unsupported file publish must not materialize a generated file"
    );
    assert_eq!(count_artifact_manifests("m10_artifact_file_bad_status"), 0);
}

#[pg_test]
fn artifact_segment_publish_enforces_collection_owner_acl() {
    let build_job_id = completed_artifact_build_job("m10_artifact_acl", "segment", "seg-a");
    let owner_role = Spi::get_one::<String>("SELECT current_user::text")
        .expect("current_user query should succeed")
        .expect("current_user should not be null");
    sql_test_create_role("m10_artifact_acl_denied");
    sql_test_grant_api_access("m10_artifact_acl_denied");

    sql_test_set_session_user("m10_artifact_acl_denied");
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "42501",
        &format!("permission denied for collection m10_artifact_acl: owner is {owner_role}"),
        "publish artifact ACL",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn artifact_segment_file_publish_materializes_generated_path() {
    let build_job_id = completed_artifact_build_job("m10_artifact_file", "segment", "seg-a");
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
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));

    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.collection_name, "m10_artifact_file");
    assert_eq!(row.build_job_id, build_job_id);
    assert_eq!(row.artifact_kind, "segment");
    assert_eq!(row.segment_kind, "hnsw_graph");
    assert_eq!(row.format_version, 1);
    assert_eq!(row.payload_bytes, 7);
    assert_eq!(row.lifecycle_state, "file_materialized");
    let relative_path = row
        .relative_path
        .as_deref()
        .expect("file publication should return a relative path");
    assert!(relative_path.starts_with("pgcontext_artifacts/"));
    assert!(relative_path.ends_with("_segment_1.pgctxseg"));
    assert!(!relative_path.contains("seg-a"));
    assert!(!relative_path.contains(".."));
    assert!(!relative_path.starts_with('/'));

    let reloaded = artifact_segment_rows(&format!(
        "SELECT kind, payload_bytes, checksum
           FROM pgcontext.validate_artifact_segment(
                pg_catalog.pg_read_binary_file('{relative_path}')
           )"
    ));
    assert_eq!(reloaded, vec![("hnsw_graph".to_owned(), 7, row.checksum)]);

    let listed = artifact_file_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_file')",
    );
    assert_eq!(listed, rows);
}

#[pg_test]
fn artifact_segment_file_republish_retires_previous_generation_for_cleanup() {
    let first_job =
        completed_artifact_build_job("m10_artifact_file_replace", "mmap", "view-a");
    let first_rows = artifact_file_rows(&format!(
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
                {first_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let first_path = first_rows[0]
        .relative_path
        .clone()
        .expect("first publish should materialize a generated file");
    assert!(artifact_file_exists(&first_path));

    let second_job =
        completed_artifact_build_job("m10_artifact_file_replace", "mmap", "view-a");
    let second_rows = artifact_file_rows(&format!(
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
                {second_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    let second_path = second_rows[0]
        .relative_path
        .clone()
        .expect("second publish should materialize a generated file");

    assert_ne!(second_rows[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(second_rows[0].build_job_id, second_job);
    assert_eq!(second_rows[0].payload_bytes, 2);
    assert_eq!(second_rows[0].lifecycle_state, "file_materialized");
    assert_ne!(second_path, first_path);
    assert!(
        artifact_file_exists(&first_path),
        "republish retires the superseded generation and leaves its file for pin-aware cleanup"
    );
    assert!(artifact_file_exists(&second_path));
    assert_eq!(count_artifact_manifests("m10_artifact_file_replace"), 2);

    let listed = artifact_file_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_file_replace')",
    );
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(listed[0].lifecycle_state, "retired");
    assert_eq!(listed[0].relative_path.as_deref(), Some(first_path.as_str()));

    let cleanup_rows = artifact_cleanup_rows(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status,
                cleanup_action,
                relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_file_replace', false)",
    );
    assert_eq!(cleanup_rows.len(), 1);
    assert_eq!(cleanup_rows[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(cleanup_rows[0].status, "retired");
    assert_eq!(cleanup_rows[0].cleanup_action, "reconciled_retired");
    assert!(cleanup_rows[0].file_removed);
    assert!(!artifact_file_exists(&first_path));
    assert!(artifact_file_exists(&second_path));
}

#[pg_test]
fn artifact_segment_metadata_republish_preserves_previous_file_generation() {
    let first_job =
        completed_artifact_build_job("m10_artifact_file_to_metadata_replace", "mmap", "view-a");
    let first_rows = artifact_file_rows(&format!(
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
                {first_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let first_path = first_rows[0]
        .relative_path
        .clone()
        .expect("first publish should materialize a generated file");
    assert!(artifact_file_exists(&first_path));

    let second_job =
        completed_artifact_build_job("m10_artifact_file_to_metadata_replace", "mmap", "view-a");
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
                {second_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));

    assert_ne!(metadata_rows[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(metadata_rows[0].build_job_id, second_job);
    assert_eq!(metadata_rows[0].payload_bytes, 2);
    assert_eq!(metadata_rows[0].lifecycle_state, "validated");
    assert!(
        artifact_file_exists(&first_path),
        "metadata republish must not touch the previous generation's generated file"
    );
    assert_eq!(
        count_artifact_manifests("m10_artifact_file_to_metadata_replace"),
        2
    );

    let listed = artifact_file_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_file_to_metadata_replace')",
    );
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(listed[0].relative_path.as_deref(), Some(first_path.as_str()));
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
    assert_eq!(listed[1].artifact_id, metadata_rows[0].artifact_id);
    assert_eq!(listed[1].relative_path, None);
    assert_eq!(listed[1].lifecycle_state, "validated");

    let cleanup_rows = artifact_cleanup_rows(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status,
                cleanup_action,
                relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.cleanup_artifact_segments(
                'm10_artifact_file_to_metadata_replace',
                false
           )",
    );
    assert!(
        cleanup_rows.is_empty(),
        "the previous generation stays referenced, so cleanup must not reclaim its file"
    );
    assert!(artifact_file_exists(&first_path));
}

#[pg_test]
fn artifact_segment_metadata_republish_ignores_previous_path_state() {
    let first_job =
        completed_artifact_build_job("m10_artifact_metadata_escape_replace", "mmap", "view-a");
    let first_rows = artifact_file_rows(&format!(
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
                {first_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let first_path = first_rows[0]
        .relative_path
        .clone()
        .expect("first publish should materialize a generated file");
    assert!(artifact_file_exists(&first_path));

    let second_job =
        completed_artifact_build_job("m10_artifact_metadata_escape_replace", "mmap", "view-a");
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../postgresql.conf'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_metadata_escape_replace'",
    )
    .expect("artifact catalog path should be updated for metadata republish confinement test");

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
                {second_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    assert_eq!(metadata_rows.len(), 1);
    assert_eq!(metadata_rows[0].build_job_id, second_job);
    assert_eq!(metadata_rows[0].lifecycle_state, "validated");

    assert!(
        artifact_file_exists(&first_path),
        "metadata republish never touches previously generated files"
    );
    let rows = artifact_file_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_metadata_escape_replace')",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].build_job_id, first_job);
    assert_eq!(rows[0].relative_path, Some("../postgresql.conf".to_owned()));
    assert_eq!(rows[0].lifecycle_state, "file_materialized");
    assert_eq!(rows[1].artifact_id, metadata_rows[0].artifact_id);
    assert_eq!(rows[1].relative_path, None);
}

#[pg_test]
fn artifact_segment_file_republish_confines_new_path_and_cleanup_rejects_escaped_previous_path() {
    let first_job =
        completed_artifact_build_job("m10_artifact_file_escape_replace", "mmap", "view-a");
    let first_rows = artifact_file_rows(&format!(
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
                {first_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let second_job =
        completed_artifact_build_job("m10_artifact_file_escape_replace", "mmap", "view-a");
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../postgresql.conf'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_file_escape_replace'",
    )
    .expect("artifact catalog path should be updated for republish confinement test");

    let second_rows = artifact_file_rows(&format!(
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
                {second_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
           )"
    ));
    let second_path = second_rows[0]
        .relative_path
        .clone()
        .expect("republish should materialize a generated file");
    assert!(second_path.starts_with("pgcontext_artifacts/"));
    assert!(!second_path.contains(".."));
    assert!(artifact_file_exists(&second_path));

    let rows = artifact_file_rows(
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
           FROM pgcontext.artifact_segments('m10_artifact_file_escape_replace')",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].artifact_id, first_rows[0].artifact_id);
    assert_eq!(rows[0].lifecycle_state, "retired");
    assert_eq!(rows[0].relative_path, Some("../postgresql.conf".to_owned()));
    assert_eq!(rows[1].lifecycle_state, "file_materialized");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.cleanup_artifact_segments(
             'm10_artifact_file_escape_replace',
             false
         )",
        "22023",
        "artifact relative path is outside pgcontext_artifacts",
        "cleanup must fail closed on the escaped previous catalog path",
    );
    assert!(artifact_file_exists(&second_path));
}

#[pg_test]
fn artifact_segment_file_publish_rejects_corruption_without_manifest() {
    let build_job_id = completed_artifact_build_job("m10_artifact_file_corrupt", "mmap", "view-a");
    shared_assert_sql_failure(
        &format!(
            "WITH encoded AS (
                 SELECT pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex')) AS segment
             )
             SELECT * FROM pgcontext.publish_artifact_segment_file({build_job_id}, (
                 SELECT set_byte(
                     segment,
                     length(segment) - 1,
                     get_byte(segment, length(segment) - 1) # 1
                 )
                 FROM encoded
             ))"
        ),
        "XX001",
        "segment checksum mismatch",
        "file publish checksum mismatch",
    );

    assert_eq!(count_artifact_manifests("m10_artifact_file_corrupt"), 0);
}

#[pg_test]
fn artifact_segment_file_publish_does_not_use_artifact_name_as_path() {
    let build_job_id = completed_artifact_build_job("m10_artifact_safe_path", "segment", "../evil");
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
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
           )"
    ));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_name, "../evil");
    let relative_path = rows[0]
        .relative_path
        .as_deref()
        .expect("file publication should return a relative path");
    assert!(!relative_path.contains(".."));
    assert!(!relative_path.contains("evil"));
    assert!(relative_path.ends_with("_segment_1.pgctxseg"));
}

#[pg_test]
fn artifact_segment_memory_reports_mmap_budget_diagnostics() {
    let segment_job = completed_artifact_build_job("m10_artifact_memory", "segment", "seg-a");
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));

    let mmap_job = completed_artifact_build_job("m10_artifact_memory", "mmap", "view-a");
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));

    let rows = artifact_memory_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                payload_bytes,
                header_bytes,
                mapped_bytes,
                file_materialized
           FROM pgcontext.artifact_segment_memory('m10_artifact_memory')",
    );

    assert_eq!(
        rows,
        vec![
            ArtifactMemoryRow {
                artifact_kind: "segment".to_owned(),
                artifact_name: "seg-a".to_owned(),
                target_name: "public.m10_artifact_memory".to_owned(),
                lifecycle_state: "validated".to_owned(),
                payload_bytes: 7,
                header_bytes: 40,
                mapped_bytes: 47,
                file_materialized: false,
            },
            ArtifactMemoryRow {
                artifact_kind: "mmap".to_owned(),
                artifact_name: "view-a".to_owned(),
                target_name: "public.m10_artifact_memory".to_owned(),
                lifecycle_state: "file_materialized".to_owned(),
                payload_bytes: 1,
                header_bytes: 40,
                mapped_bytes: 41,
                file_materialized: true,
            },
        ]
    );
}

#[pg_test]
fn artifact_segment_memory_returns_empty_for_collection_without_artifacts() {
    create_artifact_collection("m10_artifact_memory_empty");

    let rows = artifact_memory_rows(
        "SELECT artifact_kind,
                artifact_name,
                target_name,
                lifecycle_state,
                payload_bytes,
                header_bytes,
                mapped_bytes,
                file_materialized
           FROM pgcontext.artifact_segment_memory('m10_artifact_memory_empty')",
    );

    assert!(rows.is_empty());
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactManifestRow {
    artifact_id: i64,
    collection_name: String,
    build_job_id: i64,
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    segment_kind: String,
    format_version: i32,
    payload_bytes: i64,
    checksum: i64,
    lifecycle_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactFileRow {
    artifact_id: i64,
    collection_name: String,
    build_job_id: i64,
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    segment_kind: String,
    format_version: i32,
    payload_bytes: i64,
    checksum: i64,
    relative_path: Option<String>,
    lifecycle_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactMemoryRow {
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    lifecycle_state: String,
    payload_bytes: i64,
    header_bytes: i64,
    mapped_bytes: i64,
    file_materialized: bool,
}
