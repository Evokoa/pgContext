//! SQL-facing collection alias catalog functions.

use context_core::CollectionName;
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};

#[derive(Debug, Clone)]
struct AliasTarget {
    collection_id: i64,
    collection_name: String,
    owner_role: pg_sys::Oid,
}

#[derive(Debug, Clone)]
struct ExistingAlias {
    owner_role: pg_sys::Oid,
}

/// Creates or retargets a collection alias.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn create_collection_alias(
    alias_name: String,
    target_collection_name: String,
) -> TableIterator<'static, (name!(alias_name, String), name!(collection_name, String))> {
    let alias_name = collection_name_from_sql(alias_name);
    let target_collection_name = collection_name_from_sql(target_collection_name);
    reject_existing_collection_name(&alias_name);
    let target = resolve_alias_target(&target_collection_name);
    require_collection_owner(target.owner_role, &target_collection_name);
    if let Some(existing) = find_alias(&alias_name) {
        require_collection_owner(existing.owner_role, &alias_name);
    }

    Spi::run_with_args(
        "INSERT INTO pgcontext._collection_aliases (alias_name, collection_id)
         VALUES ($1, $2)
         ON CONFLICT (alias_name)
         DO UPDATE
            SET collection_id = EXCLUDED.collection_id,
                updated_at = pg_catalog.now()",
        &[alias_name.as_str().into(), target.collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to create collection alias: {error}"),
        )
    });

    TableIterator::once((alias_name.into_string(), target.collection_name))
}

/// Lists collection aliases visible to the session user.
#[pg_extern(stable, security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn collection_aliases()
-> TableIterator<'static, (name!(alias_name, String), name!(collection_name, String))> {
    let rows = Spi::connect(|client| {
        let rows = match client.select(
            "SELECT aliases.alias_name,
                    collections.collection_name
               FROM pgcontext._collection_aliases AS aliases
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
              ORDER BY aliases.alias_name",
            None,
            &[],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to list collection aliases: {error}"),
            ),
        };
        let mut output = Vec::new();
        for row in rows {
            output.push((
                match row.get::<String>(1) {
                    Ok(Some(value)) => value,
                    Ok(None) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "collection alias column is null: alias_name",
                    ),
                    Err(error) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read collection alias column alias_name: {error}"),
                    ),
                },
                match row.get::<String>(2) {
                    Ok(Some(value)) => value,
                    Ok(None) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "collection alias column is null: collection_name",
                    ),
                    Err(error) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read collection alias column collection_name: {error}"),
                    ),
                },
            ));
        }
        output
    });
    TableIterator::new(rows)
}

/// Drops a collection alias.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn drop_collection_alias(alias_name: String) -> bool {
    let alias_name = collection_name_from_sql(alias_name);
    let Some(existing) = find_alias(&alias_name) else {
        return false;
    };
    require_collection_owner(existing.owner_role, &alias_name);

    Spi::get_one_with_args::<bool>(
        "WITH deleted AS (
             DELETE FROM pgcontext._collection_aliases
              WHERE alias_name = $1
              RETURNING 1
         )
         SELECT EXISTS (SELECT 1 FROM deleted)",
        &[alias_name.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to drop collection alias: {error}"),
        )
    })
    .unwrap_or(false)
}

pub(crate) fn reject_existing_alias_name(collection_name: &CollectionName) {
    if find_alias(collection_name).is_some() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!(
                "collection alias already exists: {}",
                collection_name.as_str()
            ),
        );
    }
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn reject_existing_collection_name(alias_name: &CollectionName) {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._collections
              WHERE collection_name = $1
         )",
        &[alias_name.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection alias conflict: {error}"),
        )
    })
    .unwrap_or(false);
    if exists {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!(
                "collection already exists for alias name: {}",
                alias_name.as_str()
            ),
        );
    }
}

fn resolve_alias_target(collection_name: &CollectionName) -> AliasTarget {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id,
                    collection_name,
                    owner_role
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection alias target: {error}"),
            ),
        };
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }
        let row = rows.first();
        AliasTarget {
            collection_id: spi_required_column::<i64>(&row, 1, "collection_id"),
            collection_name: spi_required_column::<String>(&row, 2, "collection_name"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 3, "owner_role"),
        }
    })
}

fn find_alias(alias_name: &CollectionName) -> Option<ExistingAlias> {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collections.owner_role
               FROM pgcontext._collection_aliases AS aliases
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE aliases.alias_name = $1",
            Some(1),
            &[alias_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection alias: {error}"),
            ),
        };
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(ExistingAlias {
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 1, "owner_role"),
        })
    })
}

fn require_collection_owner(owner_role: pg_sys::Oid, collection_name: &CollectionName) {
    let session_user = session_user();
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')",
        &[session_user.as_str().into(), owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection alias owner: {error}"),
        )
    })
    .unwrap_or(false);

    if !is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for collection {}",
                collection_name.as_str()
            ),
        );
    }
}

fn session_user() -> String {
    match Spi::get_one::<String>("SELECT SESSION_USER::text") {
        Ok(Some(user)) => user,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "SESSION_USER returned null",
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read SESSION_USER: {error}"),
        ),
    }
}

fn spi_required_column<T>(
    row: &spi::SpiTupleTable<'_>,
    index: usize,
    column_name: &'static str,
) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("collection alias column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read collection alias column {column_name}: {error}"),
        ),
    }
}
