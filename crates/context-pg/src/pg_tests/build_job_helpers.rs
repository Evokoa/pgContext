type BuildJobTestRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<i32>,
    i32,
    i64,
    i64,
    bool,
    Option<String>,
);

fn start_build_job_for_collection(
    collection: &str,
    artifact_kind: &str,
    artifact_name: &str,
    total_units: i64,
) -> BuildJobTestRow {
    build_job_row(&format!(
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
                '{collection}',
                '{artifact_kind}',
                '{artifact_name}',
                'public.{collection}',
                {total_units}
           )"
    ))
}

fn seed_build_source_points(collection: &str, count: usize) {
    let source_keys = (1..=count)
        .map(|key| format!("'runner-src-{key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection}', ARRAY[{source_keys}])"
    ))
    .expect("build runner source points should be seeded");
}

fn build_job_row(sql: &str) -> BuildJobTestRow {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.expect("build_job_id should not be null"),
            row.get::<String>(2)?
                .expect("collection_name should not be null"),
            row.get::<String>(3)?
                .expect("artifact_kind should not be null"),
            row.get::<String>(4)?
                .expect("artifact_name should not be null"),
            row.get::<String>(5)?.expect("target_name should not be null"),
            row.get::<String>(6)?.expect("status should not be null"),
            row.get::<i32>(7)?,
            row.get::<i32>(8)?.expect("attempt should not be null"),
            row.get::<i64>(9)?
                .expect("processed_units should not be null"),
            row.get::<i64>(10)?.expect("total_units should not be null"),
            row.get::<bool>(11)?
                .expect("cancel_requested should not be null"),
            row.get::<String>(12)?,
        ))
    })
    .expect("build job row should be returned")
}

fn build_job_by_id(build_job_id: i64) -> BuildJobTestRow {
    build_job_row(&format!(
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
           FROM pgcontext._build_jobs AS jobs
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE jobs.build_job_id = {build_job_id}"
    ))
}

fn assert_build_job_sql_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
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
    .expect("invalid build job call should raise expected error");
}

fn assert_build_job_statement_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let sql = sql.replace('\'', "''");
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{sql}';
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
    .expect("invalid build job statement should raise expected error");
}

fn build_job_create_role(role_name: &str) {
    Spi::run(&format!("CREATE ROLE {role_name}")).expect("role should be created");
}

fn build_job_grant_api_access(role_name: &str) {
    Spi::run(&format!("GRANT USAGE ON SCHEMA public, pgcontext TO {role_name}"))
        .expect("role should receive schema usage");
    Spi::run(&format!(
        "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO {role_name}"
    ))
    .expect("role should receive function execute");
}

fn build_job_set_session_user(role_name: &str) {
    Spi::run(&format!("SET SESSION AUTHORIZATION {role_name}"))
        .expect("session authorization should change");
}

fn build_job_reset_session_user() {
    Spi::run("RESET SESSION AUTHORIZATION").expect("session authorization should reset");
}
