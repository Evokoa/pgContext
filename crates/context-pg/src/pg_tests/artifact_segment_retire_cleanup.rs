#[pg_test]
fn artifact_segment_retire_removes_generated_file_and_marks_manifest_retired() {
    let build_job_id = completed_artifact_build_job("m10_artifact_retire_file", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;
    let relative_path = file_rows[0]
        .relative_path
        .clone()
        .expect("file row should include a relative path");
    let absolute_path = artifact_absolute_test_path(&relative_path);
    assert!(absolute_path.exists());

    let rows = artifact_retire_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                previous_relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));

    assert_eq!(
        rows,
        vec![ArtifactRetireRow {
            artifact_id,
            collection_name: "m10_artifact_retire_file".to_owned(),
            artifact_kind: "mmap".to_owned(),
            artifact_name: "view-a".to_owned(),
            target_name: "public.m10_artifact_retire_file".to_owned(),
            previous_relative_path: Some(relative_path.clone()),
            file_removed: false,
            lifecycle_state: "retired".to_owned(),
        }]
    );
    assert!(absolute_path.exists());

    let cleanup = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_retire_file', false)",
    );
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].status, "retired");
    assert_eq!(cleanup[0].cleanup_action, "reconciled_retired");
    assert!(cleanup[0].file_removed);
    assert!(!absolute_path.exists());

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
           FROM pgcontext.artifact_segments('m10_artifact_retire_file')",
    );
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].lifecycle_state, "retired");
    assert_eq!(listed[0].relative_path, None);

    let repeated = artifact_retire_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                previous_relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));
    assert_eq!(repeated.len(), 1);
    assert_eq!(repeated[0].previous_relative_path, None);
    assert!(!repeated[0].file_removed);
    assert_eq!(repeated[0].lifecycle_state, "retired");

    let diagnostics = artifact_diagnostic_rows(
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
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_retire_file')",
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].lifecycle_state, "retired");
    assert_eq!(diagnostics[0].status, "metadata_only");
    assert_eq!(
        diagnostics[0].repair_advice,
        "artifact manifest is retired; no materialized file cleanup is pending"
    );
    assert!(!diagnostics[0].cleanup_eligible);
}

#[pg_test]
fn artifact_segment_retire_tolerates_already_missing_files() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_retire_missing", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;
    let relative_path = file_rows[0]
        .relative_path
        .as_deref()
        .expect("file row should include a relative path");
    remove_artifact_file(relative_path);

    let rows = artifact_retire_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                previous_relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle_state, "retired");
    assert!(!rows[0].file_removed);
}

#[pg_test]
fn artifact_segment_cleanup_reconciles_abandoned_reader_pins_before_unlinking() {
    let build_job_id = completed_artifact_build_job("m10_artifact_retire_stale_pin", "mmap", "view-a");
    let file_rows = artifact_file_rows(&format!(
        "SELECT artifact_id, collection_name, build_job_id, artifact_kind, artifact_name,
                target_name, segment_kind, format_version, payload_bytes, checksum,
                relative_path, lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let artifact_id = file_rows[0].artifact_id;
    let relative_path = file_rows[0]
        .relative_path
        .clone()
        .expect("materialized artifact should have a path");
    let absolute_path = artifact_absolute_test_path(&relative_path);
    Spi::run(&format!(
        "INSERT INTO pgcontext._artifact_reader_pins
             (artifact_id, backend_pid, backend_identity, pin_count)
         VALUES ({artifact_id}, -1, 'retired-backend', 1)"
    ))
    .expect("stale reader pin should be inserted");

    artifact_retire_rows(&format!(
        "SELECT artifact_id, collection_name, artifact_kind, artifact_name, target_name,
                previous_relative_path, file_removed, lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));
    assert!(absolute_path.exists());

    let cleanup = artifact_cleanup_rows(
        "SELECT artifact_id, collection_name, artifact_kind, artifact_name, target_name,
                status, cleanup_action, relative_path, file_removed, lifecycle_state
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_retire_stale_pin', false)",
    );
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].status, "retired");
    assert_eq!(cleanup[0].cleanup_action, "reconciled_retired");
    assert!(cleanup[0].file_removed);
    assert!(!absolute_path.exists());
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*)::bigint FROM pgcontext._artifact_reader_pins"
        )
        .expect("reader pin count should be queryable"),
        Some(0)
    );
}

#[pg_test]
fn artifact_segment_cleanup_waits_for_a_live_reader_pin() {
    let build_job_id = completed_artifact_build_job("m10_artifact_retire_live_pin", "mmap", "view-a");
    let file_rows = artifact_file_rows(&format!(
        "SELECT artifact_id, collection_name, build_job_id, artifact_kind, artifact_name,
                target_name, segment_kind, format_version, payload_bytes, checksum,
                relative_path, lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let artifact_id = file_rows[0].artifact_id;
    let relative_path = file_rows[0]
        .relative_path
        .clone()
        .expect("materialized artifact should have a path");
    let absolute_path = artifact_absolute_test_path(&relative_path);
    Spi::run(&format!(
        "INSERT INTO pgcontext._artifact_reader_pins
             (artifact_id, backend_pid, backend_identity, pin_count)
         SELECT {artifact_id}, activity.pid, activity.backend_start::text, 1
           FROM pg_catalog.pg_stat_activity AS activity
          WHERE activity.pid = pg_catalog.pg_backend_pid()"
    ))
    .expect("live reader pin should be inserted");
    artifact_retire_rows(&format!(
        "SELECT artifact_id, collection_name, artifact_kind, artifact_name, target_name,
                previous_relative_path, file_removed, lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));

    let deferred = artifact_cleanup_rows(
        "SELECT artifact_id, collection_name, artifact_kind, artifact_name, target_name,
                status, cleanup_action, relative_path, file_removed, lifecycle_state
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_retire_live_pin', false)",
    );
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].status, "retired");
    assert!(!deferred[0].file_removed);
    assert!(absolute_path.exists());

    Spi::run(&format!(
        "DELETE FROM pgcontext._artifact_reader_pins WHERE artifact_id = {artifact_id}"
    ))
    .expect("live reader pin should be released");
    let reclaimed = artifact_cleanup_rows(
        "SELECT artifact_id, collection_name, artifact_kind, artifact_name, target_name,
                status, cleanup_action, relative_path, file_removed, lifecycle_state
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_retire_live_pin', false)",
    );
    assert_eq!(reclaimed.len(), 1);
    assert!(reclaimed[0].file_removed);
    assert!(!absolute_path.exists());
}

#[pg_test]
fn artifact_segment_cleanup_dry_run_does_not_mutate_missing_artifacts() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_cleanup_dry_run", "mmap", "view-a");
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
    let relative_path = file_rows[0]
        .relative_path
        .as_deref()
        .expect("file row should include a relative path");
    remove_artifact_file(relative_path);

    let rows = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_dry_run', true)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "artifact_missing");
    assert_eq!(rows[0].cleanup_action, "would_retire");
    assert_eq!(rows[0].relative_path.as_deref(), Some(relative_path));
    assert!(!rows[0].file_removed);
    assert_eq!(rows[0].lifecycle_state, "file_materialized");

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
           FROM pgcontext.artifact_segments('m10_artifact_cleanup_dry_run')",
    );
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
    assert_eq!(listed[0].relative_path.as_deref(), Some(relative_path));
}

#[pg_test]
fn artifact_segment_cleanup_retires_missing_artifacts() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_cleanup_missing", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;
    let relative_path = file_rows[0]
        .relative_path
        .as_deref()
        .expect("file row should include a relative path");
    remove_artifact_file(relative_path);

    let rows = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_missing', false)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_id, artifact_id);
    assert_eq!(rows[0].status, "artifact_missing");
    assert_eq!(rows[0].cleanup_action, "retired");
    assert_eq!(rows[0].relative_path.as_deref(), Some(relative_path));
    assert!(!rows[0].file_removed);
    assert_eq!(rows[0].lifecycle_state, "retired");

    let diagnostics = artifact_diagnostic_rows(
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
           FROM pgcontext.artifact_segment_diagnostics('m10_artifact_cleanup_missing')",
    );
    assert_eq!(diagnostics[0].lifecycle_state, "retired");
    assert_eq!(diagnostics[0].status, "metadata_only");
    assert!(!diagnostics[0].cleanup_eligible);
}

#[pg_test]
fn artifact_segment_cleanup_removes_corrupt_files_but_skips_ready_files() {
    let corrupt_job =
        completed_artifact_build_job("m10_artifact_cleanup_corrupt", "mmap", "corrupt");
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));
    let ready_job = completed_artifact_build_job("m10_artifact_cleanup_corrupt", "mmap", "ready");
    let ready_rows = artifact_file_rows(&format!(
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
                {ready_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('02', 'hex'))
           )"
    ));
    let corrupt_path = corrupt_rows[0]
        .relative_path
        .as_deref()
        .expect("corrupt row should include a relative path");
    let ready_path = ready_rows[0]
        .relative_path
        .as_deref()
        .expect("ready row should include a relative path");
    flip_last_artifact_file_byte(corrupt_path);

    let rows = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_corrupt', false)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_name, "corrupt");
    assert_eq!(rows[0].status, "checksum_mismatch");
    assert_eq!(rows[0].cleanup_action, "retired");
    assert!(
        !rows[0].file_removed,
        "the first cleanup pass only retires; the file waits for pin-aware reclamation"
    );
    assert_eq!(rows[0].lifecycle_state, "retired");
    assert!(artifact_file_exists(corrupt_path));
    assert!(artifact_file_exists(ready_path));

    let reclaimed = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_corrupt', false)",
    );
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].artifact_name, "corrupt");
    assert_eq!(reclaimed[0].status, "retired");
    assert_eq!(reclaimed[0].cleanup_action, "reconciled_retired");
    assert!(reclaimed[0].file_removed);
    assert!(!artifact_file_exists(corrupt_path));
    assert!(artifact_file_exists(ready_path));
}

#[pg_test]
fn artifact_segment_cleanup_alias_does_not_remove_live_manifest_file() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_cleanup_alias_target", "mmap", "view-a");
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
    let relative_path = file_rows[0]
        .relative_path
        .as_deref()
        .expect("file row should include a relative path");
    Spi::run(
        "SELECT pgcontext.create_collection_alias(
             'm10_artifact_cleanup_alias_live',
             'm10_artifact_cleanup_alias_target'
         )",
    )
    .expect("artifact cleanup alias should be created");

    let rows = artifact_cleanup_rows(
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
                'm10_artifact_cleanup_alias_live',
                false
           )",
    );

    assert!(rows.is_empty());
    assert!(
        artifact_file_exists(relative_path),
        "cleanup through an alias must not classify the target collection's live manifest file as orphaned"
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
           FROM pgcontext.artifact_segments('m10_artifact_cleanup_alias_target')",
    );
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].relative_path.as_deref(), Some(relative_path));
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
}

#[pg_test]
fn artifact_segment_cleanup_dry_run_reports_orphan_files_without_manifest() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_cleanup_orphan_dry", "mmap", "view-a");
    let relative_path = artifact_file_path_for_build_job(build_job_id);
    write_orphan_artifact_file(&relative_path, "01");

    let rows = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_orphan_dry', true)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_id, 0);
    assert_eq!(rows[0].artifact_kind, "orphaned_file");
    assert_eq!(
        rows[0].artifact_name,
        format!("{build_job_id}_mmap.pgctxseg")
    );
    assert_eq!(rows[0].target_name, "");
    assert_eq!(rows[0].status, "orphaned_file");
    assert_eq!(rows[0].cleanup_action, "would_remove_file");
    assert_eq!(
        rows[0].relative_path.as_deref(),
        Some(relative_path.as_str())
    );
    assert!(!rows[0].file_removed);
    assert_eq!(rows[0].lifecycle_state, "orphaned_file");
    assert!(artifact_file_exists(&relative_path));
    assert_eq!(
        count_artifact_manifests("m10_artifact_cleanup_orphan_dry"),
        0
    );
}

#[pg_test]
fn artifact_segment_cleanup_removes_orphan_files_without_manifest() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_cleanup_orphan", "mmap", "view-a");
    let relative_path = artifact_file_path_for_build_job(build_job_id);
    write_orphan_artifact_file(&relative_path, "7061796c6f6164");

    let rows = artifact_cleanup_rows(
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
           FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_orphan', false)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_id, 0);
    assert_eq!(rows[0].status, "orphaned_file");
    assert_eq!(rows[0].cleanup_action, "removed_file");
    assert_eq!(
        rows[0].relative_path.as_deref(),
        Some(relative_path.as_str())
    );
    assert!(rows[0].file_removed);
    assert_eq!(rows[0].lifecycle_state, "orphaned_file");
    assert!(!artifact_file_exists(&relative_path));
    assert_eq!(count_artifact_manifests("m10_artifact_cleanup_orphan"), 0);
}

#[cfg(unix)]
#[pg_test]
fn artifact_segment_cleanup_does_not_follow_or_remove_orphan_symlinks() {
    create_artifact_collection("m10_artifact_cleanup_orphan_symlink");
    let collection_id = artifact_collection_id("m10_artifact_cleanup_orphan_symlink");
    let root_path = artifact_absolute_test_path(&format!("pgcontext_artifacts/{collection_id}"));
    let relative_path = format!("pgcontext_artifacts/{collection_id}/symlink.pgctxseg");
    let symlink_path = artifact_absolute_test_path(&relative_path);
    let target_path =
        std::env::temp_dir().join(format!("pgcontext-orphan-symlink-target-{collection_id}"));
    let non_segment_path = root_path.join("not-a-segment.txt");
    let directory_path = root_path.join("nested");
    if symlink_path.exists() {
        std::fs::remove_file(&symlink_path).expect("stale symlink should be removable");
    }
    if target_path.exists() {
        std::fs::remove_file(&target_path).expect("stale symlink target should be removable");
    }
    if non_segment_path.exists() {
        std::fs::remove_file(&non_segment_path)
            .expect("stale non-segment artifact file should be removable");
    }
    if directory_path.exists() {
        std::fs::remove_dir_all(&directory_path)
            .expect("stale artifact directory should be removable");
    }
    std::fs::create_dir_all(&root_path).expect("artifact symlink directory should be created");
    std::fs::write(&target_path, b"outside artifact root")
        .expect("symlink target should be writable");
    std::fs::write(&non_segment_path, b"not a segment")
        .expect("non-segment artifact file should be writable");
    std::fs::create_dir(&directory_path).expect("artifact child directory should be created");
    std::os::unix::fs::symlink(&target_path, &symlink_path)
        .expect("artifact symlink should be created");

    let rows = artifact_cleanup_rows(
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
                'm10_artifact_cleanup_orphan_symlink',
                false
           )",
    );

    assert!(rows.is_empty());
    assert!(std::fs::symlink_metadata(&symlink_path)
        .expect("artifact symlink should still exist")
        .file_type()
        .is_symlink());
    assert!(target_path.exists());
    assert!(non_segment_path.exists());
    assert!(directory_path.is_dir());
    std::fs::remove_file(&symlink_path).expect("artifact symlink should be removable");
    std::fs::remove_file(&target_path).expect("symlink target should be removable");
    std::fs::remove_file(&non_segment_path).expect("non-segment file should be removable");
    std::fs::remove_dir(&directory_path).expect("artifact child directory should be removable");
}

#[pg_test]
fn artifact_segment_cleanup_rejects_catalog_path_escape() {
    let build_job_id = completed_artifact_build_job("m10_artifact_cleanup_path", "mmap", "view-a");
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
            AND collections.collection_name = 'm10_artifact_cleanup_path'",
    )
    .expect("artifact catalog path should be updated for cleanup confinement test");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_path', false)",
        "22023",
        "artifact relative path is outside pgcontext_artifacts",
        "cleanup escaped artifact path",
    );
}

#[pg_test]
fn artifact_segment_cleanup_path_escape_does_not_partially_delete_prior_files() {
    let corrupt_job =
        completed_artifact_build_job("m10_artifact_cleanup_partial", "mmap", "corrupt");
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
                pgcontext.encode_artifact_segment('hnsw_graph', decode('7061796c6f6164', 'hex'))
           )"
    ));
    let escaped_job =
        completed_artifact_build_job("m10_artifact_cleanup_partial", "mmap", "escaped");
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
                {escaped_job},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('01', 'hex'))
           )"
    ));
    let corrupt_path = corrupt_rows[0]
        .relative_path
        .as_deref()
        .expect("corrupt row should include a relative path");
    flip_last_artifact_file_byte(corrupt_path);
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../escape.pgctxseg'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_cleanup_partial'
            AND artifacts.artifact_name = 'escaped'",
    )
    .expect("artifact catalog path should be updated for partial cleanup test");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.cleanup_artifact_segments('m10_artifact_cleanup_partial', false)",
        "22023",
        "artifact relative path is outside pgcontext_artifacts",
        "cleanup escaped artifact path after candidate",
    );

    assert!(
        artifact_file_exists(corrupt_path),
        "cleanup must not delete earlier files before rejecting a later escaped path"
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
           FROM pgcontext.artifact_segments('m10_artifact_cleanup_partial')",
    );
    assert_eq!(listed[0].artifact_name, "corrupt");
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
    assert_eq!(listed[0].relative_path.as_deref(), Some(corrupt_path));
}

#[pg_test]
fn artifact_segment_retire_rejects_catalog_path_escape() {
    let build_job_id = completed_artifact_build_job("m10_artifact_retire_path", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;
    Spi::run(
        "UPDATE pgcontext._artifact_segments AS artifacts
            SET relative_path = '../escape.pgctxseg'
           FROM pgcontext._collections AS collections
          WHERE artifacts.collection_id = collections.collection_id
            AND collections.collection_name = 'm10_artifact_retire_path'",
    )
    .expect("artifact catalog path should be updated for retire confinement test");

    shared_assert_sql_failure(
        &format!("SELECT * FROM pgcontext.retire_artifact_segment({artifact_id})"),
        "22023",
        "artifact relative path is outside pgcontext_artifacts",
        "retire escaped artifact path",
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
           FROM pgcontext.artifact_segments('m10_artifact_retire_path')",
    );
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
    assert_eq!(
        listed[0].relative_path,
        Some("../escape.pgctxseg".to_owned())
    );
}

#[pg_test]
fn artifact_segment_retire_enforces_collection_visibility() {
    let build_job_id = completed_artifact_build_job("m10_artifact_retire_acl", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;

    sql_test_create_role("m10_artifact_retire_denied");
    sql_test_grant_api_access("m10_artifact_retire_denied");
    sql_test_set_session_user("m10_artifact_retire_denied");
    shared_assert_sql_failure(
        &format!("SELECT * FROM pgcontext.retire_artifact_segment({artifact_id})"),
        "42704",
        &format!("artifact segment does not exist: {artifact_id}"),
        "retire artifact ACL",
    );
    sql_test_reset_session_user();

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
           FROM pgcontext.artifact_segments('m10_artifact_retire_acl')",
    );
    assert_eq!(listed[0].lifecycle_state, "file_materialized");
    assert!(listed[0].relative_path.is_some());
}

#[pg_test]
fn artifact_segment_retire_ignores_hostile_search_path() {
    let build_job_id =
        completed_artifact_build_job("m10_artifact_retire_search_path", "mmap", "view-a");
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
    let artifact_id = file_rows[0].artifact_id;
    Spi::run("CREATE SCHEMA m10_artifact_retire_shadow").expect("shadow schema should be created");
    Spi::run(
        "CREATE TABLE m10_artifact_retire_shadow._artifact_segments (
             artifact_id bigint,
             lifecycle_state text
         )",
    )
    .expect("shadow artifact table should be created");
    Spi::run(
        "SET LOCAL search_path =
             m10_artifact_retire_shadow, public, pgcontext, pg_catalog",
    )
    .expect("hostile search_path should be set");

    let rows = artifact_retire_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                previous_relative_path,
                file_removed,
                lifecycle_state
           FROM pgcontext.retire_artifact_segment({artifact_id})"
    ));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].artifact_kind, "mmap");
    assert_eq!(rows[0].lifecycle_state, "retired");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT pg_catalog.count(*)::bigint FROM m10_artifact_retire_shadow._artifact_segments"
        )
        .expect("shadow count query should succeed")
        .expect("shadow count should not be null"),
        0
    );
}
