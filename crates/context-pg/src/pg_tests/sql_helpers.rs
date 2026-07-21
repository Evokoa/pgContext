fn sql_test_create_role(role_name: &str) {
    Spi::run(&format!("DROP ROLE IF EXISTS {role_name}")).expect("test role should be dropped");
    Spi::run(&format!("CREATE ROLE {role_name}")).expect("test role should be created");
}

fn sql_test_grant_api_access(role_name: &str) {
    Spi::run(&format!(
        "GRANT USAGE ON SCHEMA pgcontext TO {role_name};
         GRANT USAGE ON SCHEMA public TO {role_name};
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA pgcontext TO {role_name};
         GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO {role_name};
         GRANT USAGE ON ALL SEQUENCES IN SCHEMA pgcontext TO {role_name}"
    ))
    .expect("test role should receive pgContext API access");
}

fn sql_test_set_session_user(role_name: &str) {
    Spi::run(&format!("SET SESSION AUTHORIZATION {role_name}"))
        .expect("test session user should be set");
}

fn sql_test_reset_session_user() {
    Spi::run("RESET SESSION AUTHORIZATION").expect("test session user should be reset");
}

fn shared_assert_sql_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
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
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %, message: %',
                        actual_sqlstate,
                        SQLERRM;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .expect("invalid SQL should raise expected error");
}
