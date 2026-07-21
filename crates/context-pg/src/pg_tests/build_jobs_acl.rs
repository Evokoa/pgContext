#[pg_test]
fn build_jobs_enforce_collection_owner_acl() {
    build_job_create_role("m10_build_acl_owner");
    build_job_create_role("m10_build_acl_member");
    build_job_create_role("m10_build_acl_denied");
    build_job_grant_api_access("m10_build_acl_owner");
    build_job_grant_api_access("m10_build_acl_member");
    build_job_grant_api_access("m10_build_acl_denied");
    Spi::run("GRANT m10_build_acl_owner TO m10_build_acl_member")
        .expect("member should receive owner role membership");

    let owner_job = with_build_job_session_user("m10_build_acl_owner", || {
        Spi::run("SELECT pgcontext.create_collection('m10_build_acl')")
            .expect("owner should create collection");
        build_job_row(
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
               FROM pgcontext.start_build_job('m10_build_acl', 'index', 'idx', 'public.m10_build_acl', 4)",
        )
    });

    with_build_job_session_user("m10_build_acl_denied", || {
        assert_build_job_sql_failure(
            "SELECT * FROM pgcontext.build_jobs('m10_build_acl')",
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job list",
        );
        assert_build_job_sql_failure(
            "SELECT * FROM pgcontext.start_build_job('m10_build_acl', 'index', 'idx2', 'public.m10_build_acl', 4)",
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job start",
        );
        assert_build_job_sql_failure(
            &format!("SELECT * FROM pgcontext.request_build_cancel({})", owner_job.0),
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job cancel",
        );
        assert_build_job_sql_failure(
            &format!("SELECT * FROM pgcontext.retry_build_job({})", owner_job.0),
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job retry",
        );
        assert_build_job_sql_failure(
            &format!(
                "SELECT * FROM pgcontext.update_build_job({}, 1, 'running')",
                owner_job.0
            ),
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job update",
        );
        assert_build_job_sql_failure(
            &format!("SELECT * FROM pgcontext.run_build_job({}, 1)", owner_job.0),
            "42501",
            "permission denied for collection m10_build_acl: owner is m10_build_acl_owner",
            "non-owner build job run",
        );
    });

    with_build_job_session_user("m10_build_acl_member", || {
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
               FROM pgcontext.build_jobs('m10_build_acl')",
        );
        assert_eq!(listed.0, owner_job.0);
        let updated = build_job_row(&format!(
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
               FROM pgcontext.update_build_job({}, 1, 'running')",
            owner_job.0
        ));
        assert_eq!(updated.8, 1);
    });
}

fn with_build_job_session_user<T>(role_name: &str, action: impl FnOnce() -> T) -> T {
    build_job_set_session_user(role_name);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    build_job_reset_session_user();
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
