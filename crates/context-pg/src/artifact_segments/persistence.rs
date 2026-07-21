fn resolve_publishable_build_job(build_job_id: i64) -> PublishableBuildJob {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT jobs.build_job_id,
                    jobs.collection_id,
                    collections.collection_name,
                    collections.owner_role::regrole::text,
                    pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER'),
                    jobs.artifact_kind,
                    jobs.artifact_name,
                    jobs.target_name,
                    jobs.config_revision,
                    jobs.status
               FROM pgcontext._build_jobs AS jobs
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE jobs.build_job_id = $1",
            Some(1),
            &[build_job_id.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("build job does not exist: {build_job_id}"),
            );
        }
        let row = rows.first();
        let collection_name = required_column(row.get::<String>(3)?, "collection_name");
        let owner_role = required_column(row.get::<String>(4)?, "owner_role");
        let owns_collection = required_column(row.get::<bool>(5)?, "owns_collection");
        if !owns_collection {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                format!(
                    "permission denied for collection {collection_name}: owner is {owner_role}"
                ),
            );
        }

        let artifact_kind =
            artifact_kind_from_catalog(required_column(row.get::<String>(6)?, "artifact_kind"));
        if !matches!(artifact_kind, ArtifactKind::Segment | ArtifactKind::Mmap) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "artifact publication supports only segment and mmap jobs for now: {}",
                    artifact_kind.as_sql()
                ),
            );
        }

        let status = required_column(row.get::<String>(10)?, "status");
        if status != "completed" {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!("cannot publish artifact for build job {build_job_id} in status {status}"),
            );
        }

        Ok::<_, spi::Error>(PublishableBuildJob {
            build_job_id: required_column(row.get::<i64>(1)?, "build_job_id"),
            collection_id: required_column(row.get::<i64>(2)?, "collection_id"),
            collection_name,
            artifact_kind,
            artifact_name: required_column(row.get::<String>(7)?, "artifact_name"),
            target_name: required_column(row.get::<String>(8)?, "target_name"),
            config_revision: row.get::<i64>(9)?,
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact build job lookup failed: {error}"),
        )
    })
}

fn insert_artifact_segment(
    job: &PublishableBuildJob,
    generation: i64,
    metadata: &ValidatedSegmentMetadata,
    relative_path: Option<&str>,
) -> i64 {
    Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._artifact_segments (
             collection_id,
             build_job_id,
             artifact_kind,
             artifact_name,
             target_name,
             generation,
             segment_kind,
             format_version,
             payload_bytes,
             checksum,
             relative_path,
             config_revision,
             lifecycle_state
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
             CASE
                 WHEN $11 IS NULL THEN 'validated'
                 WHEN $12 IS NOT DISTINCT FROM pgcontext.current_vector_config_revision($1)
                    THEN 'file_materialized'
                 ELSE 'rebuild_required'
             END
         )
         RETURNING artifact_id",
        &[
            job.collection_id.into(),
            job.build_job_id.into(),
            job.artifact_kind.as_sql().into(),
            job.artifact_name.as_str().into(),
            job.target_name.as_str().into(),
            generation.into(),
            metadata.segment_kind.as_sql().into(),
            metadata.format_version.into(),
            metadata.payload_bytes.into(),
            metadata.checksum.into(),
            relative_path.into(),
            job.config_revision.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment publish failed: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "artifact segment publish did not return an artifact id",
        )
    })
}

fn lock_artifact_publish_target(job: &PublishableBuildJob) {
    let lock_key = artifact_publish_lock_key(
        job.collection_id,
        job.artifact_kind.as_sql(),
        &job.artifact_name,
        &job.target_name,
    );
    lock_artifact_target_by_key(&lock_key);
}

fn lock_artifact_segment_target(row: &ArtifactSegmentRow) {
    let lock_key = artifact_publish_lock_key(
        row.collection_id,
        row.artifact_kind.as_sql(),
        &row.artifact_name,
        &row.target_name,
    );
    lock_artifact_target_by_key(&lock_key);
}

/// Takes the exclusive form of the per-target advisory lock whose shared
/// form readers acquire before pinning; file cleanup uses it to exclude new
/// readers of one artifact without locking the pins table.
fn lock_artifact_segment_target_exclusive(row: &ArtifactSegmentRow) {
    let lock_key = artifact_publish_lock_key(
        row.collection_id,
        row.artifact_kind.as_sql(),
        &row.artifact_name,
        &row.target_name,
    );
    lock_artifact_target_by_key(&lock_key);
}

fn lock_artifact_segment_target_shared(row: &ArtifactSegmentRow) {
    let lock_key = artifact_publish_lock_key(
        row.collection_id,
        row.artifact_kind.as_sql(),
        &row.artifact_name,
        &row.target_name,
    );
    Spi::get_one_with_args::<()>(
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtextextended($1, 0))",
        &[lock_key.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment reader lock failed: {error}"),
        )
    });
}

fn lock_artifact_target_by_key(lock_key: &str) {
    Spi::get_one_with_args::<()>(
        "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended($1, 0))",
        &[lock_key.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment publish lock failed: {error}"),
        )
    });
}

fn artifact_publish_lock_key(
    collection_id: i64,
    artifact_kind: &str,
    artifact_name: &str,
    target_name: &str,
) -> String {
    format!(
        "artifact-segment-publish:{}:{}:{}:{}:{}:{}:{}",
        collection_id,
        artifact_kind.len(),
        artifact_kind,
        artifact_name.len(),
        artifact_name,
        target_name.len(),
        target_name,
    )
}

fn next_artifact_generation(job: &PublishableBuildJob) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT COALESCE(MAX(generation), 0) + 1
           FROM pgcontext._artifact_segments
          WHERE collection_id = $1
            AND artifact_kind = $2
            AND artifact_name = $3
            AND target_name = $4",
        &[
            job.collection_id.into(),
            job.artifact_kind.as_sql().into(),
            job.artifact_name.as_str().into(),
            job.target_name.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact generation lookup failed: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "artifact generation lookup returned no row",
        )
    })
}

fn retire_superseded_artifact_generations(job: &PublishableBuildJob, active_artifact_id: i64) {
    Spi::run_with_args(
        "UPDATE pgcontext._artifact_segments
            SET lifecycle_state = 'retired', updated_at = pg_catalog.now()
          WHERE collection_id = $1
            AND artifact_kind = $2
            AND artifact_name = $3
            AND target_name = $4
            AND artifact_id <> $5
            AND lifecycle_state <> 'retired'",
        &[
            job.collection_id.into(),
            job.artifact_kind.as_sql().into(),
            job.artifact_name.as_str().into(),
            job.target_name.as_str().into(),
            active_artifact_id.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact generation retirement failed: {error}"),
        )
    });
}

fn resolve_visible_artifact_segment(artifact_id: i64) -> ArtifactSegmentRow {
    let rows = select_artifact_segment_rows(
        "WHERE artifacts.artifact_id = $1",
        &[artifact_id.into()],
        "artifact segment lookup failed",
    );
    rows.into_iter().next().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("artifact segment does not exist: {artifact_id}"),
        )
    })
}

fn retire_artifact_segment_row(artifact_id: i64) -> ArtifactSegmentRow {
    let retired_id = Spi::get_one_with_args::<i64>(
        "UPDATE pgcontext._artifact_segments
            SET lifecycle_state = 'retired',
                updated_at = pg_catalog.now()
          WHERE artifact_id = $1
      RETURNING artifact_id",
        &[artifact_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment retire failed: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "artifact segment retire did not return an artifact id",
        )
    });
    resolve_visible_artifact_segment(retired_id)
}

fn retire_artifact_segment_row_if_current(
    expected: &ArtifactSegmentRow,
) -> Option<ArtifactSegmentRow> {
    let Some(relative_path) = expected.relative_path.as_deref() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "artifact segment cleanup candidate has no relative path",
        );
    };
    let retired_id = Spi::get_one_with_args::<i64>(
        "UPDATE pgcontext._artifact_segments
            SET lifecycle_state = 'retired',
                updated_at = pg_catalog.now()
          WHERE artifact_id = $1
            AND build_job_id = $2
            AND segment_kind = $3
            AND format_version = $4
            AND payload_bytes = $5
            AND checksum = $6
            AND relative_path = $7
            AND lifecycle_state = $8
      RETURNING artifact_id",
        &[
            expected.artifact_id.into(),
            expected.build_job_id.into(),
            expected.segment_kind.as_sql().into(),
            expected.format_version.into(),
            expected.payload_bytes.into(),
            expected.checksum.into(),
            relative_path.into(),
            expected.lifecycle_state.as_sql().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment cleanup retire failed: {error}"),
        )
    });
    retired_id.map(resolve_visible_artifact_segment)
}

fn select_artifact_segments(collection: &str) -> Vec<ArtifactSegmentRow> {
    select_artifact_segment_rows(
        "WHERE artifacts.collection_name = $1",
        &[collection.into()],
        "artifact segment list failed",
    )
}

fn select_artifact_segments_by_collection_id(collection_id: i64) -> Vec<ArtifactSegmentRow> {
    select_artifact_segment_rows(
        "WHERE artifacts.collection_id = $1",
        &[collection_id.into()],
        "artifact segment list failed",
    )
}

fn resolve_visible_artifact_collection(collection: &str) -> Option<ArtifactCollection> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collection_id,
                    collection_name
               FROM pgcontext._collection_acl
              WHERE collection_name = $1
                AND pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')",
            Some(1),
            &[collection.into()],
        )?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = rows.first();
        Ok::<_, spi::Error>(Some(ArtifactCollection {
            collection_id: required_column(row.get::<i64>(1)?, "collection_id"),
            collection_name: required_column(row.get::<String>(2)?, "collection_name"),
        }))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact collection lookup failed: {error}"),
        )
    })
}

fn select_artifact_segment_rows(
    predicate: &str,
    args: &[pgrx::datum::DatumWithOid<'_>],
    context: &'static str,
) -> Vec<ArtifactSegmentRow> {
    Spi::connect(|client| {
        let sql = format!(
            "SELECT artifacts.artifact_id,
                    artifacts.collection_id,
                    artifacts.collection_name,
                    artifacts.build_job_id,
                    artifacts.artifact_kind,
                    artifacts.artifact_name,
                    artifacts.target_name,
                    artifacts.generation,
                    artifacts.segment_kind,
                    artifacts.format_version,
                    artifacts.payload_bytes,
                    artifacts.checksum,
                    artifacts.config_revision,
                    artifacts.relative_path,
                    artifacts.lifecycle_state
               FROM pgcontext._visible_artifact_segments AS artifacts
              {predicate}
              ORDER BY artifacts.artifact_id"
        );
        let rows = client.select(&sql, None, args)?;
        let mut output = Vec::new();
        for row in rows {
            output.push(artifact_segment_from_spi_row(&row)?);
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("{context}: {error}"),
        )
    })
}

fn artifact_segment_from_spi_row(
    row: &spi::SpiHeapTupleData<'_>,
) -> Result<ArtifactSegmentRow, spi::Error> {
    Ok(ArtifactSegmentRow {
        artifact_id: required_column(row.get::<i64>(1)?, "artifact_id"),
        collection_id: required_column(row.get::<i64>(2)?, "collection_id"),
        collection_name: required_column(row.get::<String>(3)?, "collection_name"),
        build_job_id: required_column(row.get::<i64>(4)?, "build_job_id"),
        artifact_kind: artifact_kind_from_catalog(required_column(
            row.get::<String>(5)?,
            "artifact_kind",
        )),
        artifact_name: required_column(row.get::<String>(6)?, "artifact_name"),
        target_name: required_column(row.get::<String>(7)?, "target_name"),
        generation: required_column(row.get::<i64>(8)?, "generation"),
        segment_kind: ArtifactSegmentKind::from_catalog(required_column(
            row.get::<String>(9)?,
            "segment_kind",
        )),
        format_version: required_column(row.get::<i32>(10)?, "format_version"),
        payload_bytes: required_column(row.get::<i64>(11)?, "payload_bytes"),
        checksum: required_column(row.get::<i64>(12)?, "checksum"),
        config_revision: row.get::<i64>(13)?,
        relative_path: row.get::<String>(14)?,
        lifecycle_state: artifact_lifecycle_state_from_catalog(required_column(
            row.get::<String>(15)?,
            "lifecycle_state",
        )),
    })
}

fn required_column<T>(value: Option<T>, column: &'static str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact segment query returned null {column}"),
        )
    })
}

fn raise_segment_error(error: SegmentError) -> ! {
    let sqlstate = match error {
        SegmentError::PayloadLengthOverflow { .. } | SegmentError::PayloadTooLarge { .. } => {
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        }
        _ => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
    };
    raise_sql_error(sqlstate, error.to_string())
}

fn raise_hnsw_graph_payload_error(error: HnswGraphPayloadError) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
}
