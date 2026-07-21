#[pg_test]
fn build_jobs_track_backend_local_progress_and_completion() {
    create_optimization_collection("m10_build_progress");

    let started = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.start_build_job(
                'm10_build_progress',
                'index',
                'm10_build_progress_embedding_idx',
                'public.m10_build_progress',
                10
           )",
    );

    assert_eq!(started.1, "m10_build_progress");
    assert_eq!(started.2, "index");
    assert_eq!(started.5, "Running");
    assert!(started.6.is_some());
    assert_eq!(started.7, 1);
    assert_eq!(started.8, 0);
    assert_eq!(started.9, 10);
    assert!(!started.10);
    assert_eq!(started.11, None);

    let progressed = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.update_build_job({}, 4, 'running')",
        started.0
    ));

    assert_eq!(progressed.0, started.0);
    assert_eq!(progressed.5, "Running");
    assert_eq!(progressed.8, 4);
    assert_eq!(progressed.9, 10);

    let completed = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.update_build_job({}, 10, 'completed')",
        started.0
    ));

    assert_eq!(completed.5, "Completed");
    assert_eq!(completed.6, None);
    assert_eq!(completed.8, 10);

    let listed = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.build_jobs('m10_build_progress')",
    );
    assert_eq!(listed.0, started.0);
    assert_eq!(listed.5, "Completed");
}
#[pg_test]
fn build_jobs_reject_duplicate_active_target_until_terminal() {
    create_optimization_collection("m10_build_duplicate");
    let job = start_build_job_for_collection("m10_build_duplicate", "index", "idx", 5);

    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_duplicate', 'index', 'idx', 'public.m10_build_duplicate', 5)",
        "55000",
        &format!("active build job already exists for target: {}", job.0),
        "duplicate active build job",
    );

    let planned = start_build_job_for_collection("m10_build_duplicate", "segment", "planned", 5);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'planned',
                backend_pid = NULL,
                backend_identity = NULL
          WHERE build_job_id = {}",
        planned.0
    ))
    .expect("test should simulate a planned job");
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_duplicate', 'segment', 'planned', 'public.m10_build_duplicate', 5)",
        "55000",
        &format!("active build job already exists for target: {}", planned.0),
        "duplicate planned build job",
    );

    let cancel_requested =
        start_build_job_for_collection("m10_build_duplicate", "mmap", "cancel-requested", 5);
    Spi::run(&format!(
        "SELECT pgcontext.request_build_cancel({})",
        cancel_requested.0
    ))
    .expect("test should request cancellation");
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_duplicate', 'mmap', 'cancel-requested', 'public.m10_build_duplicate', 5)",
        "55000",
        &format!(
            "active build job already exists for target: {}",
            cancel_requested.0
        ),
        "duplicate cancel-requested build job",
    );

    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 3, 'failed', 'synthetic failure')",
        job.0
    ))
    .expect("job should fail terminally");

    let next = start_build_job_for_collection("m10_build_duplicate", "index", "idx", 5);
    assert_ne!(next.0, job.0);
    assert_eq!(next.5, "Running");
}

#[pg_test]
fn build_jobs_support_cooperative_cancel_and_retry() {
    create_optimization_collection("m10_build_cancel");
    let started = start_build_job_for_collection("m10_build_cancel", "segment", "seg-a", 8);

    let requested = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.request_build_cancel({})",
        started.0
    ));

    assert_eq!(requested.5, "CancelRequested");
    assert!(requested.10);

    let cancelled = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.update_build_job({}, 3, 'cancelled', 'operator cancelled')",
        started.0
    ));

    assert_eq!(cancelled.5, "Cancelled");
    assert_eq!(cancelled.6, None);
    assert_eq!(cancelled.8, 3);
    assert!(cancelled.10);
    assert_eq!(cancelled.11, Some("operator cancelled".to_owned()));

    let retried = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.retry_build_job({})",
        started.0
    ));

    assert_eq!(retried.5, "Running");
    assert!(retried.6.is_some());
    assert_eq!(retried.7, 2);
    assert_eq!(retried.8, 3);
    assert!(!retried.10);
    assert_eq!(retried.11, None);
}

#[pg_test]
fn build_job_retry_preserves_recorded_progress_for_resumable_jobs() {
    create_optimization_collection("m10_build_retry_progress");
    let started =
        start_build_job_for_collection("m10_build_retry_progress", "segment", "seg-a", 8);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 5, 'failed', 'transient failure')",
        started.0
    ))
    .expect("job should fail after recording progress");

    let retried = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.retry_build_job({})",
        started.0
    ));

    assert_eq!(retried.5, "Running");
    assert!(retried.6.is_some());
    assert_eq!(retried.7, 2);
    assert_eq!(retried.8, 5);
    assert_eq!(retried.9, 8);
    assert!(!retried.10);
    assert_eq!(retried.11, None);
}

#[pg_test]
fn build_jobs_reject_progress_regression_for_resumable_jobs() {
    create_optimization_collection("m10_build_progress_regression");
    seed_build_source_points("m10_build_progress_regression", 8);
    let started =
        start_build_job_for_collection("m10_build_progress_regression", "segment", "seg-a", 8);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 5, 'running')",
        started.0
    ))
    .expect("job should record forward progress");

    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.update_build_job({}, 4, 'running')",
            started.0
        ),
        "22023",
        "build job progress cannot go backwards: 4 < 5",
        "running progress regression",
    );
    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.update_build_job({}, 3, 'failed', 'late failure')",
            started.0
        ),
        "22023",
        "build job progress cannot go backwards: 3 < 5",
        "terminal progress regression",
    );
    assert_build_job_statement_failure(
        &format!(
            "UPDATE pgcontext._build_jobs
                SET processed_units = 2
              WHERE build_job_id = {}",
            started.0
        ),
        "22023",
        "build job progress cannot go backwards: 2 < 5",
        "catalog progress regression",
    );

    let unchanged = build_job_by_id(started.0);
    assert_eq!(unchanged.5, "running");
    assert!(unchanged.6.is_some());
    assert_eq!(unchanged.7, 1);
    assert_eq!(unchanged.8, 5);
    assert_eq!(unchanged.9, 8);
    assert!(!unchanged.10);
    assert_eq!(unchanged.11, None);

    let next_step = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(next_step.5, "Running");
    assert_eq!(next_step.8, 7);
}

#[pg_test]
fn build_jobs_reject_terminal_catalog_state_drift() {
    create_optimization_collection("m10_build_terminal_drift");
    let mut drifted_job_ids = Vec::new();
    for terminal_status in ["completed", "failed", "cancelled", "abandoned"] {
        let owned_backend = start_build_job_for_collection(
            "m10_build_terminal_drift",
            "segment",
            &format!("owned-{terminal_status}"),
            8,
        );
        assert_build_job_statement_failure(
            &format!(
                "UPDATE pgcontext._build_jobs
                    SET status = '{terminal_status}',
                        completed_at = pg_catalog.now()
                  WHERE build_job_id = {}",
                owned_backend.0
            ),
            "22023",
            &format!("terminal build job {terminal_status} cannot retain backend ownership"),
            &format!("{terminal_status} terminal backend ownership drift"),
        );
        drifted_job_ids.push(owned_backend.0);

        let missing_completed_at = start_build_job_for_collection(
            "m10_build_terminal_drift",
            "segment",
            &format!("missing-completed-at-{terminal_status}"),
            8,
        );
        assert_build_job_statement_failure(
            &format!(
                "UPDATE pgcontext._build_jobs
                    SET status = '{terminal_status}',
                        backend_pid = NULL,
                        backend_identity = NULL
                  WHERE build_job_id = {}",
                missing_completed_at.0
            ),
            "22023",
            &format!("terminal build job {terminal_status} must record completed_at"),
            &format!("{terminal_status} terminal missing completed_at drift"),
        );
        drifted_job_ids.push(missing_completed_at.0);
    }

    let incomplete_completed =
        start_build_job_for_collection("m10_build_terminal_drift", "mmap", "view-a", 8);
    assert_build_job_statement_failure(
        &format!(
            "UPDATE pgcontext._build_jobs
                SET status = 'completed',
                    backend_pid = NULL,
                    backend_identity = NULL,
                    completed_at = pg_catalog.now()
              WHERE build_job_id = {}",
            incomplete_completed.0
        ),
        "22023",
        "completed build job progress must equal total: 0 <> 8",
        "completed progress drift",
    );
    drifted_job_ids.push(incomplete_completed.0);

    for build_job_id in drifted_job_ids {
        let unchanged = build_job_by_id(build_job_id);
        assert_eq!(unchanged.5, "running");
        assert!(unchanged.6.is_some());
        assert_eq!(unchanged.7, 1);
        assert_eq!(unchanged.8, 0);
        assert_eq!(unchanged.9, 8);
        assert!(!unchanged.10);
        assert_eq!(unchanged.11, None);
    }
}

#[pg_test]
fn build_jobs_reject_active_catalog_ownership_drift() {
    create_optimization_collection("m10_build_active_drift");
    for active_status in ["running", "cancel_requested"] {
        let missing_backend_pid = start_build_job_for_collection(
            "m10_build_active_drift",
            "segment",
            &format!("missing-pid-{active_status}"),
            8,
        );
        assert_build_job_statement_failure(
            &format!(
                "UPDATE pgcontext._build_jobs
                    SET status = '{active_status}',
                        backend_pid = NULL
                  WHERE build_job_id = {}",
                missing_backend_pid.0
            ),
            "22023",
            &format!("active build job {active_status} must record backend_pid"),
            &format!("{active_status} active missing backend pid drift"),
        );

        let missing_backend_identity = start_build_job_for_collection(
            "m10_build_active_drift",
            "segment",
            &format!("missing-identity-{active_status}"),
            8,
        );
        assert_build_job_statement_failure(
            &format!(
                "UPDATE pgcontext._build_jobs
                    SET status = '{active_status}',
                        backend_identity = NULL
                  WHERE build_job_id = {}",
                missing_backend_identity.0
            ),
            "22023",
            &format!("active build job {active_status} must record backend_identity"),
            &format!("{active_status} active missing backend identity drift"),
        );

        let retained_completed_at = start_build_job_for_collection(
            "m10_build_active_drift",
            "segment",
            &format!("completed-at-{active_status}"),
            8,
        );
        assert_build_job_statement_failure(
            &format!(
                "UPDATE pgcontext._build_jobs
                    SET status = '{active_status}',
                        completed_at = pg_catalog.now()
                  WHERE build_job_id = {}",
                retained_completed_at.0
            ),
            "22023",
            &format!("active build job {active_status} cannot retain completed_at"),
            &format!("{active_status} active completed_at drift"),
        );

        for build_job_id in [
            missing_backend_pid.0,
            missing_backend_identity.0,
            retained_completed_at.0,
        ] {
            let unchanged = build_job_by_id(build_job_id);
            assert_eq!(unchanged.5, "running");
            assert!(unchanged.6.is_some());
            assert_eq!(unchanged.7, 1);
            assert_eq!(unchanged.8, 0);
            assert_eq!(unchanged.9, 8);
            assert!(!unchanged.10);
            assert_eq!(unchanged.11, None);
        }
    }
}

#[pg_test]
fn build_job_runner_advances_one_bounded_step_per_call() {
    create_optimization_collection("m10_build_runner_complete");
    seed_build_source_points("m10_build_runner_complete", 5);
    let started = start_build_job_for_collection("m10_build_runner_complete", "segment", "seg-a", 5);

    let first_step = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(first_step.0, started.0);
    assert_eq!(first_step.2, "segment");
    assert_eq!(first_step.5, "Running");
    assert!(first_step.6.is_some());
    assert_eq!(first_step.8, 2);
    assert_eq!(first_step.9, 5);
    assert_eq!(first_step.11, None);

    let second_step = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(second_step.5, "Running");
    assert!(second_step.6.is_some());
    assert_eq!(second_step.8, 4);
    assert_eq!(second_step.9, 5);

    let completed = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(completed.0, started.0);
    assert_eq!(completed.2, "segment");
    assert_eq!(completed.5, "Completed");
    assert_eq!(completed.6, None);
    assert_eq!(completed.8, 5);
    assert_eq!(completed.9, 5);
    assert_eq!(completed.11, None);
}

#[pg_test]
fn build_job_runner_honors_pre_requested_cancel_without_progress() {
    create_optimization_collection("m10_build_runner_cancel");
    let started = start_build_job_for_collection("m10_build_runner_cancel", "mmap", "view-a", 5);
    Spi::run(&format!("SELECT pgcontext.request_build_cancel({})", started.0))
        .expect("build cancel should be requested");

    let cancelled = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(cancelled.5, "Cancelled");
    assert_eq!(cancelled.6, None);
    assert_eq!(cancelled.8, 0);
    assert!(cancelled.10);
    assert_eq!(
        cancelled.11,
        Some("build job cancelled before runner step".to_owned())
    );
}

#[pg_test]
fn build_job_runner_preserves_existing_progress_and_saturates_at_total() {
    create_optimization_collection("m10_build_runner_progress");
    seed_build_source_points("m10_build_runner_progress", 5);
    let started = start_build_job_for_collection("m10_build_runner_progress", "segment", "seg-a", 5);
    let advanced = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 4)",
        started.0
    ));
    assert_eq!(advanced.5, "Running");
    assert_eq!(advanced.8, 4);

    let completed = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 9223372036854775807)",
        started.0
    ));

    assert_eq!(completed.5, "Completed");
    assert_eq!(completed.8, 5);
    assert_eq!(completed.9, 5);
}

#[pg_test]
fn build_job_runner_completes_zero_unit_artifact_jobs() {
    create_optimization_collection("m10_build_runner_empty");
    let started = start_build_job_for_collection("m10_build_runner_empty", "mmap", "empty-view", 0);

    let completed = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 1)",
        started.0
    ));

    assert_eq!(completed.0, started.0);
    assert_eq!(completed.2, "mmap");
    assert_eq!(completed.3, "empty-view");
    assert_eq!(completed.5, "Completed");
    assert_eq!(completed.6, None);
    assert_eq!(completed.8, 0);
    assert_eq!(completed.9, 0);
    assert!(!completed.10);
    assert_eq!(completed.11, None);
}

#[pg_test]
fn build_job_runner_honors_cancel_requested_flag_drift() {
    create_optimization_collection("m10_build_runner_flag_cancel");
    let started =
        start_build_job_for_collection("m10_build_runner_flag_cancel", "segment", "seg-a", 5);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET cancel_requested = true
          WHERE build_job_id = {}",
        started.0
    ))
    .expect("test should simulate cancel flag drift");

    let cancelled = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(cancelled.5, "Cancelled");
    assert_eq!(cancelled.8, 0);
    assert!(cancelled.10);
}

#[pg_test]
fn build_jobs_report_abandoned_backend_and_allow_retry() {
    create_optimization_collection("m10_build_abandoned");
    let started = start_build_job_for_collection("m10_build_abandoned", "mmap", "view-a", 5);

    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 3, 'running')",
        started.0
    ))
    .expect("build progress should be recorded before abandonment");
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET backend_pid = -1,
                backend_identity = 'closed-test-backend'
          WHERE build_job_id = {}",
        started.0
    ))
    .expect("test should simulate dead backend pid");

    let abandoned = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.build_jobs('m10_build_abandoned')",
    );

    assert_eq!(abandoned.5, "Abandoned");

    let retried = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.retry_build_job({})",
        started.0
    ));

    assert_eq!(retried.5, "Running");
    assert_eq!(retried.7, 2);
    assert_eq!(retried.8, 3);
    assert!(retried.6.is_some());
}

#[pg_test]
fn build_jobs_retry_abandoned_cancel_request_preserves_progress_and_claims_backend() {
    create_optimization_collection("m10_build_abandoned_cancel_retry");
    seed_build_source_points("m10_build_abandoned_cancel_retry", 9);
    let started = start_build_job_for_collection(
        "m10_build_abandoned_cancel_retry",
        "mmap",
        "view-a",
        9,
    );
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 4, 'running')",
        started.0
    ))
    .expect("build progress should be recorded before abandoned cancel request");
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'cancel_requested',
                cancel_requested = true,
                error_message = 'stale cancel/error state',
                backend_pid = -1,
                backend_identity = 'closed-test-backend'
          WHERE build_job_id = {}",
        started.0
    ))
    .expect("test should simulate dead backend after cancellation request");

    let abandoned = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.build_jobs('m10_build_abandoned_cancel_retry')",
    );

    assert_eq!(abandoned.5, "Abandoned");
    assert_eq!(abandoned.8, 4);
    assert!(abandoned.10);

    let retried = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.retry_build_job({})",
        started.0
    ));

    assert_eq!(retried.5, "Running");
    assert!(retried.6.is_some());
    assert_eq!(retried.7, 2);
    assert_eq!(retried.8, 4);
    assert_eq!(retried.9, 9);
    assert!(!retried.10);
    assert_eq!(retried.11, None);

    let next_step = build_job_row(&format!(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.run_build_job({}, 2)",
        started.0
    ));

    assert_eq!(next_step.5, "Running");
    assert_eq!(next_step.6, retried.6);
    assert_eq!(next_step.7, 2);
    assert_eq!(next_step.8, 6);
    assert_eq!(next_step.9, 9);
    assert!(!next_step.10);
    assert_eq!(next_step.11, None);
}

#[pg_test]
fn build_jobs_allow_replacement_after_abandoned_backend() {
    create_optimization_collection("m10_build_abandoned_replace");
    let abandoned = start_build_job_for_collection(
        "m10_build_abandoned_replace",
        "segment",
        "seg-a",
        7,
    );
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET backend_pid = -1,
                backend_identity = 'closed-test-backend'
          WHERE build_job_id = {}",
        abandoned.0
    ))
    .expect("test should simulate dead backend pid");

    let listed = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.build_jobs('m10_build_abandoned_replace')",
    );
    assert_eq!(listed.0, abandoned.0);
    assert_eq!(listed.5, "Abandoned");

    let replacement = start_build_job_for_collection(
        "m10_build_abandoned_replace",
        "segment",
        "seg-a",
        11,
    );

    assert_ne!(replacement.0, abandoned.0);
    assert_eq!(replacement.2, "segment");
    assert_eq!(replacement.3, "seg-a");
    assert_eq!(replacement.5, "Running");
    assert_eq!(replacement.7, 1);
    assert_eq!(replacement.8, 0);
    assert_eq!(replacement.9, 11);
    assert!(replacement.6.is_some());

    let persisted_abandoned = build_job_by_id(abandoned.0);
    assert_eq!(persisted_abandoned.5, "abandoned");
    assert_eq!(persisted_abandoned.6, None);
}

#[pg_test]
fn build_jobs_allow_replacement_after_abandoned_cancel_request() {
    create_optimization_collection("m10_build_abandoned_cancel_replace");
    let abandoned = start_build_job_for_collection(
        "m10_build_abandoned_cancel_replace",
        "mmap",
        "view-a",
        9,
    );
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'cancel_requested',
                cancel_requested = true,
                backend_pid = -1,
                backend_identity = 'closed-test-backend'
          WHERE build_job_id = {}",
        abandoned.0
    ))
    .expect("test should simulate dead backend with pending cancellation");

    let replacement = start_build_job_for_collection(
        "m10_build_abandoned_cancel_replace",
        "mmap",
        "view-a",
        4,
    );

    assert_ne!(replacement.0, abandoned.0);
    assert_eq!(replacement.2, "mmap");
    assert_eq!(replacement.3, "view-a");
    assert_eq!(replacement.5, "Running");
    assert_eq!(replacement.9, 4);
    assert!(!replacement.10);

    let persisted_abandoned = build_job_by_id(abandoned.0);
    assert_eq!(persisted_abandoned.5, "abandoned");
    assert_eq!(persisted_abandoned.6, None);
    assert!(persisted_abandoned.10);
}

#[pg_test]
fn build_jobs_reject_bad_inputs_with_sqlstates() {
    create_optimization_collection("m10_build_bad_inputs");
    let job = start_build_job_for_collection("m10_build_bad_inputs", "index", "idx", 5);

    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('missing', 'index', 'idx', 'public.missing', 5)",
        "42704",
        "collection does not exist: missing",
        "missing build collection",
    );
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_bad_inputs', 'future', 'idx', 'target', 5)",
        "22023",
        "unsupported pgContext artifact or projection target: future",
        "unsupported artifact kind",
    );
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_bad_inputs', 'index', '', 'target', 5)",
        "22023",
        "artifact_name must not be empty",
        "empty artifact name",
    );
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.start_build_job('m10_build_bad_inputs', 'index', 'idx', 'target', -1)",
        "22023",
        "total_units must be non-negative: -1",
        "negative total units",
    );
    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.update_build_job({}, 3, 'completed')",
            job.0
        ),
        "22023",
        "completed build job progress must equal total: 3 <> 5",
        "incomplete completed progress",
    );
    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.update_build_job({}, 6, 'running')",
            job.0
        ),
        "22023",
        "build job progress exceeds total: 6 > 5",
        "progress over total",
    );
    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.update_build_job({}, 1, 'paused')",
            job.0
        ),
        "22023",
        "unsupported build job status: paused",
        "bad build status",
    );
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.build_jobs('m10_build_missing_collection')",
        "42704",
        "collection does not exist: m10_build_missing_collection",
        "missing build job list collection",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 0)", job.0),
        "22023",
        "units_per_step must be positive: 0",
        "zero build runner step",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, -1)", job.0),
        "22023",
        "units_per_step must be positive: -1",
        "negative build runner step",
    );
    assert_build_job_sql_failure(
        "SELECT * FROM pgcontext.run_build_job(999999, 1)",
        "42704",
        "build job does not exist: 999999",
        "missing build runner job",
    );
    let unsupported = start_build_job_for_collection(
        "m10_build_bad_inputs",
        "index",
        "idx-unsupported-runner",
        5,
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", unsupported.0),
        "55000",
        "build runner supports only segment and mmap artifact kinds for now: index",
        "unsupported build runner kind",
    );
    let unchanged = build_job_by_id(unsupported.0);
    assert_eq!(unchanged.0, unsupported.0);
    assert_eq!(unchanged.5, "running");
    assert_eq!(unchanged.8, unsupported.8);
    assert_eq!(unchanged.9, unsupported.9);
    assert_eq!(unchanged.11, unsupported.11);

    let unsupported_sparse = start_build_job_for_collection(
        "m10_build_bad_inputs",
        "sparse_index",
        "sparse-idx-unsupported-runner",
        5,
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", unsupported_sparse.0),
        "55000",
        "build runner supports only segment and mmap artifact kinds for now: sparse_index",
        "unsupported sparse build runner kind",
    );
    let unchanged_sparse = build_job_by_id(unsupported_sparse.0);
    assert_eq!(unchanged_sparse.0, unsupported_sparse.0);
    assert_eq!(unchanged_sparse.5, "running");
    assert_eq!(unchanged_sparse.8, unsupported_sparse.8);
    assert_eq!(unchanged_sparse.9, unsupported_sparse.9);
    assert_eq!(unchanged_sparse.11, unsupported_sparse.11);
}

#[pg_test]
fn build_jobs_reject_terminal_cancel_retry_and_update_transitions() {
    create_optimization_collection("m10_build_terminal");
    let job = start_build_job_for_collection("m10_build_terminal", "segment", "idx", 5);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 5, 'completed')",
        job.0
    ))
    .expect("job should complete");

    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.update_build_job({}, 5, 'running')", job.0),
        "55000",
        &format!("cannot update build job {} in status Completed", job.0),
        "update completed build job",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.request_build_cancel({})", job.0),
        "55000",
        &format!("cannot cancel build job {} in status Completed", job.0),
        "cancel completed build job",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.retry_build_job({})", job.0),
        "55000",
        &format!("cannot retry build job {} in status Completed", job.0),
        "retry completed build job",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", job.0),
        "55000",
        &format!("cannot update build job {} in status Completed", job.0),
        "run completed build job",
    );

    let failed = start_build_job_for_collection("m10_build_terminal", "segment", "failed-seg", 5);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 3, 'failed', 'synthetic failure')",
        failed.0
    ))
    .expect("job should fail terminally");
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", failed.0),
        "55000",
        &format!("cannot update build job {} in status Failed", failed.0),
        "run failed build job",
    );

    let cancelled =
        start_build_job_for_collection("m10_build_terminal", "segment", "cancelled-seg", 5);
    Spi::run(&format!(
        "SELECT pgcontext.update_build_job({}, 1, 'cancelled', 'synthetic cancel')",
        cancelled.0
    ))
    .expect("job should cancel terminally");
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", cancelled.0),
        "55000",
        &format!("cannot update build job {} in status Cancelled", cancelled.0),
        "run cancelled build job",
    );
}

#[pg_test]
fn build_jobs_reject_retry_for_active_and_planned_jobs() {
    create_optimization_collection("m10_build_retry_nonterminal");

    let running =
        start_build_job_for_collection("m10_build_retry_nonterminal", "segment", "running", 5);
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.retry_build_job({})", running.0),
        "55000",
        &format!("cannot retry build job {} in status Running", running.0),
        "retry running build job",
    );

    let cancel_requested = start_build_job_for_collection(
        "m10_build_retry_nonterminal",
        "segment",
        "cancel-requested",
        5,
    );
    Spi::run(&format!(
        "SELECT pgcontext.request_build_cancel({})",
        cancel_requested.0
    ))
    .expect("build cancellation should be requested");
    assert_build_job_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.retry_build_job({})",
            cancel_requested.0
        ),
        "55000",
        &format!(
            "cannot retry build job {} in status CancelRequested",
            cancel_requested.0
        ),
        "retry cancel-requested build job",
    );

    let planned =
        start_build_job_for_collection("m10_build_retry_nonterminal", "segment", "planned", 5);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'planned',
                backend_pid = NULL,
                backend_identity = NULL
          WHERE build_job_id = {}",
        planned.0
    ))
    .expect("test should simulate a planned build job");
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.retry_build_job({})", planned.0),
        "55000",
        &format!("cannot retry build job {} in status Planned", planned.0),
        "retry planned build job",
    );
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.request_build_cancel({})", planned.0),
        "55000",
        &format!("cannot cancel build job {} in status Planned", planned.0),
        "cancel planned build job",
    );
}

#[pg_test]
fn build_job_runner_rejects_abandoned_and_planned_jobs_without_claiming() {
    create_optimization_collection("m10_build_runner_not_claimable");
    let abandoned =
        start_build_job_for_collection("m10_build_runner_not_claimable", "segment", "abandoned", 5);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET backend_pid = -1,
                backend_identity = 'closed-test-backend'
          WHERE build_job_id = {}",
        abandoned.0
    ))
    .expect("test should simulate abandoned runner job");

    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", abandoned.0),
        "55000",
        &format!("cannot update build job {} in status Abandoned", abandoned.0),
        "run abandoned build job",
    );
    let abandoned_listed = build_job_row(
        "SELECT build_job_id,
                collection_name,
                artifact_kind,
                artifact_name,
                target_name,
                status::text,
                backend_pid,
                attempt,
                processed_units,
                total_units,
                cancel_requested,
                error_message
           FROM pgcontext.build_jobs('m10_build_runner_not_claimable')
          WHERE artifact_name = 'abandoned'",
    );
    assert_eq!(abandoned_listed.5, "Abandoned");

    let planned =
        start_build_job_for_collection("m10_build_runner_not_claimable", "segment", "planned", 5);
    Spi::run(&format!(
        "UPDATE pgcontext._build_jobs
            SET status = 'planned',
                backend_pid = NULL,
                backend_identity = NULL
          WHERE build_job_id = {}",
        planned.0
    ))
    .expect("test should simulate planned job");

    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", planned.0),
        "55000",
        &format!("cannot update build job {} in status Planned", planned.0),
        "run planned build job",
    );
    let listed = build_job_by_id(planned.0);
    assert_eq!(listed.5, "planned");
    assert_eq!(listed.8, 0);
}

#[pg_test]
fn build_job_source_failpoint_preserves_the_uncommitted_checkpoint() {
    create_optimization_collection("m10_build_failpoint_source");
    let started = start_build_job_for_collection(
        "m10_build_failpoint_source",
        "mmap",
        "source-failpoint",
        2,
    );
    Spi::run("SELECT pgcontext.test_set_build_job_failpoint('before_source_read')")
        .expect("source failpoint should be configured");
    assert_build_job_sql_failure(
        &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", started.0),
        "XX000",
        "injected build job failpoint: before_source_read",
        "source read failpoint",
    );
    Spi::run("SELECT pgcontext.test_set_build_job_failpoint(NULL)")
        .expect("source failpoint should be cleared");
    let unchanged = build_job_by_id(started.0);
    assert_eq!(unchanged.5, "running");
    assert_eq!(unchanged.8, 0);
}
