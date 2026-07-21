fn build_job_result(row: BuildJobRow) -> BuildJobResult {
    let status = effective_build_status(&row);
    (
        row.build_job_id,
        row.collection_name,
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        status,
        row.backend_pid,
        row.attempt,
        row.processed_units,
        row.total_units,
        row.cancel_requested,
        row.error_message,
    )
}

fn scan_source_point_batch(
    build_job_id: i64,
    after_point_id: i64,
    units_per_step: i64,
) -> (i64, i64) {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT points.point_id
               FROM pgcontext._collection_points AS points
               JOIN pgcontext._build_jobs AS jobs USING (collection_id)
              WHERE jobs.build_job_id = $1
                AND points.deleted_at IS NULL
                AND points.point_id > $2
              ORDER BY points.point_id
              LIMIT $3",
            None,
            &[
                build_job_id.into(),
                after_point_id.into(),
                units_per_step.into(),
            ],
        )?;
        let mut last_point_id = after_point_id;
        let mut count = 0_i64;
        for row in rows {
            last_point_id = required_column(row.get::<i64>(1)?, "point_id");
            count += 1;
        }
        Ok::<_, spi::Error>((last_point_id, count))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build source scan failed: {error}"),
        )
    })
}

fn replay_build_deltas(build_job_id: i64) {
    Spi::run_with_args(
        "DELETE FROM pgcontext._build_deltas
          WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build delta replay failed: {error}"),
        )
    });
}

fn effective_build_status(row: &BuildJobRow) -> BuildJobStatus {
    effective_build_status_from_parts(
        row.stored_status,
        row.backend_pid,
        row.backend_identity.as_deref(),
    )
}

fn effective_build_status_from_parts(
    status: BuildJobStatus,
    backend_pid: Option<i32>,
    backend_identity: Option<&str>,
) -> BuildJobStatus {
    match status {
        BuildJobStatus::Running | BuildJobStatus::CancelRequested => {
            let Some(backend_pid) = backend_pid else {
                return BuildJobStatus::Abandoned;
            };
            let Some(backend_identity) = backend_identity else {
                return BuildJobStatus::Abandoned;
            };
            if backend_is_active(backend_pid, backend_identity) {
                status
            } else {
                BuildJobStatus::Abandoned
            }
        }
        _ => status,
    }
}

fn resolve_owned_collection_id(collection: &str) -> i64 {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collection_id,
                    owner_role::regrole::text,
                    pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {collection}"),
            );
        }
        let row = rows.first();
        let collection_id = required_column(row.get::<i64>(1)?, "collection_id");
        let owner_role = required_column(row.get::<String>(2)?, "owner_role");
        let owns_collection = required_column(row.get::<bool>(3)?, "owns_collection");
        if !owns_collection {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                format!("permission denied for collection {collection}: owner is {owner_role}"),
            );
        }
        Ok::<_, spi::Error>(collection_id)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job collection lookup failed: {error}"),
        )
    })
}

fn insert_build_job(
    collection_id: i64,
    artifact_kind: ArtifactKind,
    artifact_name: &str,
    target_name: &str,
    total_units: i64,
) -> BuildJobRow {
    let artifact_kind_label = artifact_kind.as_sql();
    lock_build_job_target(
        collection_id,
        artifact_kind_label,
        artifact_name,
        target_name,
    );
    reject_duplicate_active_build_job(
        collection_id,
        artifact_kind_label,
        artifact_name,
        target_name,
    );
    let (backend_pid, backend_identity) = current_backend_identity();
    let sql = "INSERT INTO pgcontext._build_jobs (
                    collection_id,
                    artifact_kind,
                    artifact_name,
                    target_name,
                    status,
                    backend_pid,
                    backend_identity,
                    total_units,
                    config_revision
               )
               VALUES ($1, $2, $3, $4, 'running', $5, $6, $7,
                       pgcontext.current_vector_config_revision($1))
               RETURNING build_job_id";
    let build_job_id = Spi::get_one_with_args::<i64>(
        sql,
        &[
            collection_id.into(),
            artifact_kind_label.into(),
            artifact_name.into(),
            target_name.into(),
            backend_pid.into(),
            backend_identity.into(),
            total_units.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job insert failed: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "build job insert did not return a build job id",
        )
    });

    resolve_visible_build_job(build_job_id)
}

fn lock_build_job_target(
    collection_id: i64,
    artifact_kind: &str,
    artifact_name: &str,
    target_name: &str,
) {
    let lock_key = format!("{collection_id}:{artifact_kind}:{artifact_name}:{target_name}");
    Spi::run_with_args(
        "SELECT pg_catalog.pg_advisory_xact_lock(
                    pg_catalog.hashtextextended($1, 0)
                )",
        &[lock_key.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job target lock failed: {error}"),
        )
    });
}

fn reject_duplicate_active_build_job(
    collection_id: i64,
    artifact_kind: &str,
    artifact_name: &str,
    target_name: &str,
) {
    let existing = Spi::connect(|client| {
        let rows = client.select(
            "SELECT build_job_id,
                    status,
                    backend_pid,
                    backend_identity
               FROM pgcontext._build_jobs
              WHERE collection_id = $1
                AND artifact_kind = $2
                AND artifact_name = $3
                AND target_name = $4
                AND status IN ('planned', 'running', 'cancel_requested')
              ORDER BY build_job_id",
            None,
            &[
                collection_id.into(),
                artifact_kind.into(),
                artifact_name.into(),
                target_name.into(),
            ],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ActiveBuildJobCandidate {
                build_job_id: required_column(row.get::<i64>(1)?, "build_job_id"),
                stored_status: build_status_from_catalog(&required_column(
                    row.get::<String>(2)?,
                    "status",
                )),
                backend_pid: row.get::<i32>(3)?,
                backend_identity: row.get::<String>(4)?,
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job duplicate check failed: {error}"),
        )
    });
    for candidate in existing {
        match effective_build_status_from_parts(
            candidate.stored_status,
            candidate.backend_pid,
            candidate.backend_identity.as_deref(),
        ) {
            BuildJobStatus::Running | BuildJobStatus::CancelRequested | BuildJobStatus::Planned => {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                    format!(
                        "active build job already exists for target: {}",
                        candidate.build_job_id
                    ),
                );
            }
            BuildJobStatus::Abandoned => mark_build_job_abandoned(&candidate),
            BuildJobStatus::Cancelled | BuildJobStatus::Completed | BuildJobStatus::Failed => {}
        }
    }
}

fn select_build_jobs(collection_id: i64) -> Vec<BuildJobRow> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT jobs.build_job_id,
                    collections.collection_name,
                    jobs.artifact_kind,
                    jobs.artifact_name,
                    jobs.target_name,
                    jobs.status,
                    jobs.backend_pid,
                    jobs.backend_identity,
                    jobs.attempt,
                    jobs.total_units,
                    jobs.processed_units,
                    jobs.last_source_point_id,
                    jobs.cancel_requested,
                    jobs.error_message
               FROM pgcontext._visible_build_jobs AS jobs
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE jobs.collection_id = $1
              ORDER BY jobs.build_job_id",
            None,
            &[collection_id.into()],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push(build_job_from_spi_iter_row(&row)?);
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job list query failed: {error}"),
        )
    })
}

fn resolve_visible_build_job(build_job_id: i64) -> BuildJobRow {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT jobs.build_job_id,
                    collections.collection_name,
                    jobs.artifact_kind,
                    jobs.artifact_name,
                    jobs.target_name,
                    jobs.status,
                    jobs.backend_pid,
                    jobs.backend_identity,
                    jobs.attempt,
                    jobs.total_units,
                    jobs.processed_units,
                    jobs.last_source_point_id,
                    jobs.cancel_requested,
                    jobs.error_message,
                    pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER'),
                    collections.owner_role::regrole::text
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
        let owns_collection = required_column(row.get::<bool>(15)?, "owns_collection");
        if !owns_collection {
            let collection_name = required_column(row.get::<String>(2)?, "collection_name");
            let owner_role = required_column(row.get::<String>(16)?, "owner_role");
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                format!(
                    "permission denied for collection {collection_name}: owner is {owner_role}"
                ),
            );
        }
        build_job_from_spi_row(&row)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job lookup failed: {error}"),
        )
    })
}

fn build_job_from_spi_row(row: &spi::SpiTupleTable<'_>) -> Result<BuildJobRow, spi::Error> {
    Ok(BuildJobRow {
        build_job_id: required_column(row.get::<i64>(1)?, "build_job_id"),
        collection_name: required_column(row.get::<String>(2)?, "collection_name"),
        artifact_kind: artifact_kind_from_catalog(required_column(
            row.get::<String>(3)?,
            "artifact_kind",
        )),
        artifact_name: required_column(row.get::<String>(4)?, "artifact_name"),
        target_name: required_column(row.get::<String>(5)?, "target_name"),
        stored_status: build_status_from_catalog(&required_column(row.get::<String>(6)?, "status")),
        backend_pid: row.get::<i32>(7)?,
        backend_identity: row.get::<String>(8)?,
        attempt: required_column(row.get::<i32>(9)?, "attempt"),
        total_units: required_column(row.get::<i64>(10)?, "total_units"),
        processed_units: required_column(row.get::<i64>(11)?, "processed_units"),
        last_source_point_id: required_column(row.get::<i64>(12)?, "last_source_point_id"),
        cancel_requested: required_column(row.get::<bool>(13)?, "cancel_requested"),
        error_message: row.get::<String>(14)?,
    })
}

fn build_job_from_spi_iter_row(row: &spi::SpiHeapTupleData<'_>) -> Result<BuildJobRow, spi::Error> {
    Ok(BuildJobRow {
        build_job_id: required_column(row.get::<i64>(1)?, "build_job_id"),
        collection_name: required_column(row.get::<String>(2)?, "collection_name"),
        artifact_kind: artifact_kind_from_catalog(required_column(
            row.get::<String>(3)?,
            "artifact_kind",
        )),
        artifact_name: required_column(row.get::<String>(4)?, "artifact_name"),
        target_name: required_column(row.get::<String>(5)?, "target_name"),
        stored_status: build_status_from_catalog(&required_column(row.get::<String>(6)?, "status")),
        backend_pid: row.get::<i32>(7)?,
        backend_identity: row.get::<String>(8)?,
        attempt: required_column(row.get::<i32>(9)?, "attempt"),
        total_units: required_column(row.get::<i64>(10)?, "total_units"),
        processed_units: required_column(row.get::<i64>(11)?, "processed_units"),
        last_source_point_id: required_column(row.get::<i64>(12)?, "last_source_point_id"),
        cancel_requested: required_column(row.get::<bool>(13)?, "cancel_requested"),
        error_message: row.get::<String>(14)?,
    })
}

fn ensure_current_backend_can_update(row: &BuildJobRow) {
    match effective_build_status(row) {
        BuildJobStatus::Running | BuildJobStatus::CancelRequested => {}
        status => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "cannot update build job {} in status {status:?}",
                row.build_job_id
            ),
        ),
    }

    let (backend_pid, backend_identity) = current_backend_identity();
    if row.backend_pid != Some(backend_pid)
        || row.backend_identity.as_deref() != Some(&backend_identity)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("build job {} is owned by another backend", row.build_job_id),
        );
    }
}

fn status_catalog_value(status: BuildJobStatus) -> &'static str {
    match status {
        BuildJobStatus::Planned => "planned",
        BuildJobStatus::Running => "running",
        BuildJobStatus::CancelRequested => "cancel_requested",
        BuildJobStatus::Cancelled => "cancelled",
        BuildJobStatus::Completed => "completed",
        BuildJobStatus::Failed => "failed",
        BuildJobStatus::Abandoned => "abandoned",
    }
}

fn update_build_job_row(
    build_job_id: i64,
    processed_units: i64,
    status: BuildJobStatus,
    error_message: Option<String>,
    last_source_point_id: Option<i64>,
) -> BuildJobRow {
    let terminal = matches!(
        status,
        BuildJobStatus::Completed | BuildJobStatus::Failed | BuildJobStatus::Cancelled
    );
    let (backend_pid, backend_identity) = if terminal {
        (None, None)
    } else {
        let (pid, identity) = current_backend_identity();
        (Some(pid), Some(identity))
    };
    let status_value = status_catalog_value(status);
    Spi::run_with_args(
        "UPDATE pgcontext._build_jobs
            SET processed_units = $1,
                status = $2,
                last_source_point_id = COALESCE($3, last_source_point_id),
                backend_pid = $4,
                backend_identity = $5,
                cancel_requested = CASE WHEN $2 = 'cancelled' THEN true ELSE cancel_requested END,
                error_message = $6,
                updated_at = pg_catalog.now(),
                completed_at = CASE
                    WHEN $2 IN ('completed', 'failed', 'cancelled') THEN pg_catalog.now()
                    ELSE NULL
                END
          WHERE build_job_id = $7",
        &[
            processed_units.into(),
            status_value.into(),
            last_source_point_id.into(),
            backend_pid.into(),
            backend_identity.into(),
            error_message.into(),
            build_job_id.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job update failed: {error}"),
        )
    });
    resolve_visible_build_job(build_job_id)
}

fn request_build_cancel_row(build_job_id: i64) -> BuildJobRow {
    Spi::run_with_args(
        "UPDATE pgcontext._build_jobs
            SET status = 'cancel_requested',
                cancel_requested = true,
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build cancel request failed: {error}"),
        )
    });
    resolve_visible_build_job(build_job_id)
}

fn mark_build_job_abandoned(candidate: &ActiveBuildJobCandidate) {
    build_job_failpoint(4, "before_backend_loss_recovery");
    let updated = Spi::get_one_with_args::<i64>(
        "UPDATE pgcontext._build_jobs
            SET status = 'abandoned',
                backend_pid = NULL,
                backend_identity = NULL,
                updated_at = pg_catalog.now(),
                completed_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND status = $2
            AND backend_pid IS NOT DISTINCT FROM $3
            AND backend_identity IS NOT DISTINCT FROM $4
      RETURNING build_job_id",
        &[
            candidate.build_job_id.into(),
            status_catalog_value(candidate.stored_status).into(),
            candidate.backend_pid.into(),
            candidate.backend_identity.clone().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job abandonment recording failed: {error}"),
        )
    });
    if updated.is_none() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "active build job changed while starting replacement: {}",
                candidate.build_job_id
            ),
        );
    }
}

fn retry_build_job_row(build_job_id: i64) -> BuildJobRow {
    let (backend_pid, backend_identity) = current_backend_identity();
    Spi::run_with_args(
        "UPDATE pgcontext._build_jobs
            SET status = 'running',
                backend_pid = $1,
                backend_identity = $2,
                attempt = attempt + 1,
                cancel_requested = false,
                error_message = NULL,
                updated_at = pg_catalog.now(),
                completed_at = NULL
          WHERE build_job_id = $3",
        &[
            backend_pid.into(),
            backend_identity.into(),
            build_job_id.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job retry failed: {error}"),
        )
    });
    resolve_visible_build_job(build_job_id)
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build job catalog column was unexpectedly null: {column_name}"),
        ),
    }
}
