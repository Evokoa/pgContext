// File lifecycle and reader-pin cleanup helpers included by
// `artifact_segments.rs`. Keeping these operations in one fragment makes the
// lock-and-delete boundary reviewable without creating another public module.

fn artifact_relative_path(job: &PublishableBuildJob, generation: i64) -> String {
    format!(
        "pgcontext_artifacts/{}/{}_{}_{}.pgctxseg",
        job.collection_id,
        job.build_job_id,
        job.artifact_kind.as_sql(),
        generation
    )
}

fn artifact_collection_root(collection_id: i64) -> String {
    format!("pgcontext_artifacts/{collection_id}")
}

fn artifact_absolute_path(relative_path: &str) -> PathBuf {
    debug_assert!(!Path::new(relative_path).is_absolute());
    postgres_data_directory().join(relative_path)
}

fn remove_retired_artifact_file(relative_path: &str) -> bool {
    match fs::remove_file(artifact_absolute_path(relative_path)) {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

fn reclaim_retired_artifact_file(row: &ArtifactSegmentRow) -> bool {
    if row.lifecycle_state != ArtifactLifecycleState::Retired {
        return false;
    }
    let Some(relative_path) = row.relative_path.as_deref() else {
        return false;
    };
    // Excludes new readers of this artifact while the pin check and unlink
    // run: readers take the shared form of this same per-target advisory
    // lock before pinning, so a reader either pinned before this exclusive
    // acquisition (the count below sees it and cleanup defers) or waits
    // until this transaction ends. The table-wide
    // `LOCK TABLE _artifact_reader_pins IN SHARE MODE` this replaces
    // upgrade-deadlocked against any concurrent transaction that had
    // already inserted a pin row and later cleaned up.
    lock_artifact_segment_target_exclusive(row);
    reconcile_abandoned_artifact_reader_pins(row.artifact_id);
    if artifact_reader_pin_count(row.artifact_id) != 0 {
        return false;
    }
    match fs::remove_file(artifact_absolute_path(relative_path)) {
        Ok(()) => {
            clear_reclaimed_artifact_path(row.artifact_id, relative_path);
            true
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            clear_reclaimed_artifact_path(row.artifact_id, relative_path);
            false
        }
        Err(_) => false,
    }
}

fn reconcile_abandoned_artifact_reader_pins(artifact_id: i64) {
    Spi::run_with_args(
        "DELETE FROM pgcontext._artifact_reader_pins AS pins
          WHERE pins.artifact_id = $1
            AND NOT EXISTS (
                SELECT 1
                  FROM pg_catalog.pg_stat_activity AS activity
                 WHERE activity.pid = pins.backend_pid
                   AND activity.backend_start::text = pins.backend_identity
            )",
        &[artifact_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact reader pin reconciliation failed: {error}"),
        )
    });
}

fn artifact_reader_pin_count(artifact_id: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT COUNT(*) FROM pgcontext._artifact_reader_pins WHERE artifact_id = $1",
        &[artifact_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact reader pin count failed: {error}"),
        )
    })
    .unwrap_or(0)
}

fn clear_reclaimed_artifact_path(artifact_id: i64, relative_path: &str) {
    Spi::run_with_args(
        "UPDATE pgcontext._artifact_segments
            SET relative_path = NULL, updated_at = pg_catalog.now()
          WHERE artifact_id = $1
            AND lifecycle_state = 'retired'
            AND relative_path = $2",
        &[artifact_id.into(), relative_path.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact retirement reconciliation failed: {error}"),
        )
    });
}

fn cleanup_artifact_segment(
    row: ArtifactSegmentRow,
    dry_run: bool,
) -> Option<ArtifactSegmentCleanupResult> {
    lock_artifact_segment_target(&row);
    let current = resolve_visible_artifact_segment(row.artifact_id);
    if !artifact_cleanup_snapshot_matches(&row, &current) {
        return None;
    }

    let relative_path = current.relative_path.clone()?;
    if !artifact_relative_path_is_confined(&relative_path) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "artifact relative path is outside pgcontext_artifacts",
        );
    }
    if current.lifecycle_state == ArtifactLifecycleState::Retired {
        if dry_run {
            return Some(artifact_segment_cleanup_result(
                current,
                "retired",
                "would_reconcile_retired",
                Some(relative_path),
                false,
            ));
        }
        let file_removed = reclaim_retired_artifact_file(&current);
        return Some(artifact_segment_cleanup_result(
            resolve_visible_artifact_segment(current.artifact_id),
            "retired",
            "reconciled_retired",
            Some(relative_path),
            file_removed,
        ));
    }
    let status = cleanup_candidate_status(&current, &relative_path)?;
    if dry_run {
        return Some(artifact_segment_cleanup_result(
            current,
            status,
            "would_retire",
            Some(relative_path),
            false,
        ));
    }

    let retired = retire_artifact_segment_row_if_current(&current)?;
    Some(artifact_segment_cleanup_result(
        retired,
        status,
        "retired",
        Some(relative_path),
        false,
    ))
}

fn cleanup_orphan_artifact_files(
    collection: &ArtifactCollection,
    referenced_paths: &HashSet<String>,
    dry_run: bool,
) -> Vec<ArtifactSegmentCleanupResult> {
    let root_relative_path = artifact_collection_root(collection.collection_id);
    let root_path = artifact_absolute_path(&root_relative_path);
    let entries = match fs::read_dir(&root_path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!(
                    "failed to scan artifact directory {}: {error}",
                    root_path.display()
                ),
            );
        }
    };

    let mut output = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!(
                    "failed to read artifact directory {}: {error}",
                    root_path.display()
                ),
            )
        });
        let metadata = fs::symlink_metadata(entry.path()).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!(
                    "failed to inspect artifact path {}: {error}",
                    entry.path().display()
                ),
            )
        });
        if !metadata.file_type().is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.ends_with(".pgctxseg") {
            continue;
        }

        let relative_path = format!("{root_relative_path}/{file_name}");
        if referenced_paths.contains(&relative_path) {
            continue;
        }

        let file_removed = if dry_run {
            false
        } else {
            remove_retired_artifact_file(&relative_path)
        };
        output.push(orphan_artifact_cleanup_result(
            collection,
            file_name,
            relative_path,
            dry_run,
            file_removed,
        ));
    }
    output.sort_by(|left, right| left.7.cmp(&right.7));
    output
}

fn orphan_artifact_cleanup_result(
    collection: &ArtifactCollection,
    file_name: &str,
    relative_path: String,
    dry_run: bool,
    file_removed: bool,
) -> ArtifactSegmentCleanupResult {
    (
        0,
        collection.collection_name.clone(),
        "orphaned_file".to_owned(),
        file_name.to_owned(),
        String::new(),
        "orphaned_file".to_owned(),
        if dry_run {
            "would_remove_file"
        } else {
            "removed_file"
        }
        .to_owned(),
        Some(relative_path),
        file_removed,
        "orphaned_file".to_owned(),
    )
}

fn artifact_manifest_paths(rows: &[ArtifactSegmentRow]) -> HashSet<String> {
    rows.iter()
        .filter_map(|row| row.relative_path.clone())
        .collect()
}

fn prevalidate_artifact_cleanup_paths(rows: &[ArtifactSegmentRow]) {
    for row in rows {
        if let Some(relative_path) = row.relative_path.as_deref()
            && !artifact_relative_path_is_confined(relative_path)
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "artifact relative path is outside pgcontext_artifacts",
            );
        }
    }
}

fn artifact_cleanup_snapshot_matches(
    expected: &ArtifactSegmentRow,
    current: &ArtifactSegmentRow,
) -> bool {
    expected.artifact_id == current.artifact_id
        && expected.build_job_id == current.build_job_id
        && expected.segment_kind == current.segment_kind
        && expected.format_version == current.format_version
        && expected.payload_bytes == current.payload_bytes
        && expected.checksum == current.checksum
        && expected.relative_path == current.relative_path
        && expected.lifecycle_state == current.lifecycle_state
}

fn cleanup_candidate_status(row: &ArtifactSegmentRow, relative_path: &str) -> Option<&'static str> {
    let path = artifact_absolute_path(relative_path);
    match load_segment_file(&path) {
        Ok(segment) if loaded_segment_matches_manifest(row, &segment) => None,
        Ok(_) => Some("metadata_mismatch"),
        Err(SegmentFileError::Io {
            operation: "open",
            source,
            ..
        }) if source.kind() == ErrorKind::NotFound => Some("artifact_missing"),
        Err(SegmentFileError::Format(SegmentError::ChecksumMismatch)) => Some("checksum_mismatch"),
        Err(_) => Some("artifact_corrupt"),
    }
}

fn loaded_segment_matches_manifest(row: &ArtifactSegmentRow, segment: &SegmentBytes) -> bool {
    let file_kind = ArtifactSegmentKind::from(segment.header().kind());
    let file_format_version =
        i32::try_from(segment.header().version().as_u32()).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "segment format version exceeds PostgreSQL integer range",
            )
        });
    let file_payload_bytes = i64::try_from(segment.payload().len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "segment payload length exceeds PostgreSQL bigint range",
        )
    });
    let file_checksum = i64::from_ne_bytes(segment.header().checksum().to_ne_bytes());
    row.segment_kind == file_kind
        && row.format_version == file_format_version
        && row.payload_bytes == file_payload_bytes
        && row.checksum == file_checksum
}

fn artifact_relative_path_is_confined(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    if path.is_absolute() {
        return false;
    }
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "pgcontext_artifacts" => {}
        _ => return false,
    }
    components.all(|component| matches!(component, Component::Normal(_)))
}

fn postgres_data_directory() -> PathBuf {
    let data_directory = Spi::get_one::<String>("SHOW data_directory")
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to resolve PostgreSQL data_directory: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "PostgreSQL data_directory is not set",
            )
        });
    PathBuf::from(data_directory)
}

fn ensure_artifact_parent(destination: &Path) {
    let Some(parent) = destination.parent() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!(
                "artifact destination has no parent: {}",
                destination.display()
            ),
        );
    };
    fs::create_dir_all(parent).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!(
                "failed to create artifact directory {}: {error}",
                parent.display()
            ),
        )
    });
}

fn raise_segment_file_error(error: SegmentFileError) -> ! {
    match error {
        SegmentFileError::Format(error) => raise_segment_error(error),
        other => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment file publication failed: {other}"),
        ),
    }
}
