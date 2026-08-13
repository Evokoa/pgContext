//! Complete invoker-authoritative exact search over a registered source.

use context_core::{CollectionName, SearchLimit};

use crate::vector::Vector;

use super::{advisor::quote_identifier, *};

#[derive(Clone, Debug)]
struct ExactDenseBinding {
    collection_name: String,
    source_schema_name: String,
    source_table_name: String,
    source_key_column_name: String,
    binding_name: String,
    column_name: String,
    dimensions: usize,
    metric: String,
}

/// Executes a complete exact dense-vector query over current invoker-visible rows.
#[pg_extern(name = "exact_first_search")]
#[search_path(pg_catalog, pgcontext)]
pub fn exact_first_search(
    collection: String,
    binding: String,
    vector: Vector,
    limit: i32,
) -> TableIterator<'static, (name!(source_key, String), name!(score, f32))> {
    let collection = CollectionName::new(collection).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let limit = usize::try_from(limit)
        .ok()
        .and_then(|limit| SearchLimit::new(limit).ok())
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "exact-first search limit must be positive and bounded",
            )
        });
    TableIterator::new(search_rows(&collection, &binding, &vector, limit))
}

pub(crate) fn try_search_default(
    collection: &CollectionName,
    vector: &Vector,
    limit: SearchLimit,
) -> Option<Vec<(i64, String, f32)>> {
    let binding = load_dense_binding(collection, None)?;
    let rows = execute(&binding, vector, limit, true);
    Some(
        rows.into_iter()
            .map(|(source_key, point_id, score)| {
                let point_id = point_id.unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                        "the search(collection, vector, limit) exact-first fallback requires an integral source key; use exact_first_search for other key types",
                    )
                });
                (point_id, source_key, score)
            })
            .collect(),
    )
}

fn search_rows(
    collection: &CollectionName,
    binding_name: &str,
    vector: &Vector,
    limit: SearchLimit,
) -> Vec<(String, f32)> {
    let binding = load_dense_binding(collection, Some(binding_name)).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "registered exact-first dense binding does not exist",
        )
    });
    execute(&binding, vector, limit, false)
        .into_iter()
        .map(|(source_key, _point_id, score)| (source_key, score))
        .collect()
}

fn load_dense_binding(
    collection: &CollectionName,
    requested_binding: Option<&str>,
) -> Option<ExactDenseBinding> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registrations.source_schema_name,
                        registrations.source_table_name,
                        registrations.source_key_column_name,
                        columns.binding_name, columns.column_name,
                        columns.dimensions, columns.metric
                   FROM pgcontext._visible_exact_first_registrations AS registrations
                   JOIN pgcontext._visible_collections AS collections USING (collection_id)
                   JOIN pgcontext._visible_exact_first_columns AS columns
                     USING (exact_first_registration_id)
                  WHERE collections.collection_name = $1
                    AND columns.binding_kind = 'dense'
                    AND ($2::text IS NULL OR columns.binding_name = $2)
                  ORDER BY columns.binding_ordinal
                  LIMIT 2",
                None,
                &[collection.as_str().into(), nullable_text(requested_binding)],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first dense binding: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        if requested_binding.is_none() && rows.len() != 1 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "the exact-first search fallback requires exactly one dense binding",
            );
        }
        let row = rows.first();
        let dimensions = required(row.get::<i32>(6).unwrap_or(None), "binding_dimensions");
        Some(ExactDenseBinding {
            collection_name: collection.as_str().to_owned(),
            source_schema_name: required(row.get::<String>(1).unwrap_or(None), "source_schema"),
            source_table_name: required(row.get::<String>(2).unwrap_or(None), "source_table"),
            source_key_column_name: required(
                row.get::<String>(3).unwrap_or(None),
                "source_key_column",
            ),
            binding_name: required(row.get::<String>(4).unwrap_or(None), "binding_name"),
            column_name: required(row.get::<String>(5).unwrap_or(None), "column_name"),
            dimensions: usize::try_from(dimensions).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "exact-first dense dimensions are invalid",
                )
            }),
            metric: required(row.get::<String>(7).unwrap_or(None), "binding_metric"),
        })
    })
}

fn execute(
    binding: &ExactDenseBinding,
    vector: &Vector,
    limit: SearchLimit,
    include_point_id: bool,
) -> Vec<(String, Option<i64>, f32)> {
    require_current_registration(&binding.collection_name);
    if vector.dimension() != binding.dimensions {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            format!(
                "exact-first query dimension does not match binding {}",
                binding.binding_name
            ),
        );
    }
    let operator = match binding.metric.as_str() {
        "l2" => "<->",
        "inner_product" => "<#>",
        "cosine" => "<=>",
        "l1" => "<+>",
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first dense metric is not executable",
        ),
    };
    let table = format!(
        "{}.{}",
        quote_identifier(&binding.source_schema_name),
        quote_identifier(&binding.source_table_name)
    );
    let key = quote_identifier(&binding.source_key_column_name);
    let column = quote_identifier(&binding.column_name);
    let point_id = if include_point_id {
        "source.source_key::bigint".to_owned()
    } else {
        "NULL::bigint".to_owned()
    };
    let sql = format!(
        "WITH source AS MATERIALIZED (
             SELECT {key} AS source_key, {column} AS search_value
               FROM {table}
              WHERE {column} IS NOT NULL
         )
         SELECT source.source_key::text, {point_id},
                (source.search_value OPERATOR(pgcontext.{operator}) $1)::real AS score
           FROM source
          ORDER BY score ASC, source.source_key ASC
          LIMIT $2"
    );
    Spi::connect(|client| {
        let query = Vector::from_validated_values(vector.as_slice().to_vec());
        let rows = client
            .select(
                &sql,
                Some(i64::try_from(limit.get()).unwrap_or(i64::MAX)),
                &[
                    query.into(),
                    i64::try_from(limit.get()).unwrap_or(i64::MAX).into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to execute exact-first exact search: {error}"),
                )
            });
        rows.into_iter()
            .map(|row| {
                (
                    required(row.get::<String>(1).unwrap_or(None), "source_key"),
                    row.get::<i64>(2).unwrap_or(None),
                    required(row.get::<f32>(3).unwrap_or(None), "score"),
                )
            })
            .collect()
    })
}

fn require_current_registration(collection: &str) {
    let state = Spi::get_one_with_args::<String>(
        "SELECT readiness_state
           FROM pgcontext.exact_first_readiness($1)",
        &[collection.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to revalidate exact-first readiness: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "exact-first registration does not exist",
        )
    });
    if state == ExactFirstState::Stale.as_catalog() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first registration is stale",
        );
    }
}

fn nullable_text(value: Option<&str>) -> DatumWithOid<'_> {
    match value {
        Some(value) => value.into(),
        None => None::<String>.into(),
    }
}
