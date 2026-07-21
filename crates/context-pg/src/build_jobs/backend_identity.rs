use pgrx::{prelude::*, spi};

use crate::error::raise_sql_error;

pub(super) fn current_backend_identity() -> (i32, String) {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT activity.pid,
                    activity.backend_start::text
               FROM pg_catalog.pg_stat_activity AS activity
              WHERE activity.pid = pg_catalog.pg_backend_pid()",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            required_column(row.get::<i32>(1)?, "pid"),
            required_column(row.get::<String>(2)?, "backend_start"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("backend identity query failed: {error}"),
        )
    })
}

pub(super) fn backend_is_active(backend_pid: i32, backend_identity: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
            SELECT 1
              FROM pg_catalog.pg_stat_activity
             WHERE pid = $1
               AND backend_start::text = $2
         )",
        &[backend_pid.into(), backend_identity.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("backend activity query failed: {error}"),
        )
    })
    .unwrap_or(false)
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("backend identity column was unexpectedly null: {column_name}"),
        ),
    }
}
