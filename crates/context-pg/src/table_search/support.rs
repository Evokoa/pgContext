//! Filter and facet support for table-backed search.

use context_filter::{
    FieldRegistry, Filter, FilterError, PayloadValue, RangeBound, SqlParameter, SqlParameterType,
    parse_filter_json, render_sql_predicate,
};
use pgrx::JsonB;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use serde_json::Value;

use crate::error::raise_sql_error;

#[derive(Debug, Clone)]
pub(crate) struct FilterField {
    pub(crate) filter_key: String,
    pub(crate) column_name: String,
    pub(crate) jsonb_path: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct FilterPredicatePlan {
    pub(crate) sql: String,
    pub(crate) parameters: Vec<FilterSqlParameter>,
}

#[derive(Debug, Clone)]
pub(crate) enum FilterSqlParameter {
    Text(String),
    Bool(bool),
    I64(i64),
    F64(f64),
    TextArray(Vec<String>),
    BoolArray(Vec<bool>),
    I64Array(Vec<i64>),
    F64Array(Vec<f64>),
    Jsonb(Value),
    JsonbArray(Vec<Value>),
}

#[derive(Debug, Clone)]
pub(super) struct FacetTarget {
    pub(super) column_name: String,
    pub(super) jsonb_path: Option<Vec<String>>,
}

pub(crate) fn load_filter_fields(collection_id: i64) -> Vec<FilterField> {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT filter_key,
                    column_name,
                    jsonb_path
               FROM pgcontext._visible_collection_payload_columns
              WHERE collection_id = $1
              ORDER BY filter_key",
            Some(i64::MAX),
            &[collection_id.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load filter columns: {error}"),
            ),
        };

        let mut fields = Vec::new();
        for row in rows {
            fields.push(FilterField {
                filter_key: row
                    .get::<String>(1)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read filter key: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "filter key is null",
                        )
                    }),
                column_name: row
                    .get::<String>(2)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read filter column: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "filter column is null",
                        )
                    }),
                jsonb_path: row.get::<Vec<String>>(3).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read filter JSONB path: {error}"),
                    )
                }),
            });
        }
        fields
    })
}

pub(super) fn resolve_facet_target(fields: &[FilterField], field: &str) -> FacetTarget {
    fields
        .iter()
        .find(|column| column.filter_key == field)
        .map(|field| FacetTarget {
            column_name: field.column_name.clone(),
            jsonb_path: field.jsonb_path.clone(),
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("unknown filter field: {field}"),
            )
        })
}

pub(super) fn resolve_filter_plan(
    fields: &[FilterField],
    filter: Option<&str>,
    placeholder_offset: usize,
) -> Option<FilterPredicatePlan> {
    let filter = filter?;
    let filter = match parse_filter_json(filter) {
        Ok(filter) => filter,
        Err(error) => raise_filter_error(error),
    };

    resolve_typed_filter_plan(fields, &filter, placeholder_offset)
        .unwrap_or_else(|error| raise_filter_error(error))
        .into()
}

pub(crate) fn resolve_typed_filter_plan(
    fields: &[FilterField],
    filter: &Filter,
    placeholder_offset: usize,
) -> Result<FilterPredicatePlan, FilterError> {
    let mut builder = FieldRegistry::builder();
    for field in fields {
        builder = match field.jsonb_path.as_ref() {
            Some(path) => builder.register_jsonb_path(
                &field.filter_key,
                field.column_name.clone(),
                path.clone(),
            ),
            None => builder.register_column(&field.filter_key, field.column_name.clone()),
        }?;
    }
    let registry = builder.build();
    let resolved = registry.resolve_filter(filter)?;
    let plan = render_sql_predicate(&resolved);
    Ok(FilterPredicatePlan {
        sql: shift_placeholders(&plan.sql, placeholder_offset),
        parameters: sql_parameters(plan.parameters, plan.parameter_types),
    })
}

pub(crate) fn push_filter_parameter_args<'a>(
    args: &mut Vec<DatumWithOid<'a>>,
    parameter_values: &'a [FilterSqlParameter],
) {
    for value in parameter_values {
        match value {
            FilterSqlParameter::Text(value) => args.push(value.as_str().into()),
            FilterSqlParameter::Bool(value) => args.push((*value).into()),
            FilterSqlParameter::I64(value) => args.push((*value).into()),
            FilterSqlParameter::F64(value) => args.push((*value).into()),
            FilterSqlParameter::TextArray(values) => args.push(values.clone().into()),
            FilterSqlParameter::BoolArray(values) => args.push(values.clone().into()),
            FilterSqlParameter::I64Array(values) => args.push(values.clone().into()),
            FilterSqlParameter::F64Array(values) => args.push(values.clone().into()),
            FilterSqlParameter::Jsonb(value) => args.push(JsonB(value.clone()).into()),
            FilterSqlParameter::JsonbArray(values) => {
                let values = values.iter().cloned().map(JsonB).collect::<Vec<_>>();
                args.push(values.into());
            }
        }
    }
}

pub(super) fn facet_expression(target: &FacetTarget) -> String {
    let column = super::quote_identifier(&target.column_name);
    match target.jsonb_path.as_ref() {
        Some(path) => format!("(source.{column} #>> {})", quoted_text_array(path)),
        None => format!("source.{column}::text"),
    }
}

fn raise_filter_error(error: FilterError) -> ! {
    let message = match error {
        FilterError::UnknownPayloadField { key } => format!("unknown filter field: {key}"),
        other => other.to_string(),
    };
    raise_sql_error(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message)
}

fn sql_parameters(
    parameters: Vec<SqlParameter>,
    parameter_types: Vec<SqlParameterType>,
) -> Vec<FilterSqlParameter> {
    parameters
        .into_iter()
        .zip(parameter_types)
        .map(|(parameter, parameter_type)| match parameter_type {
            SqlParameterType::InferFromSql => infer_sql_parameter(parameter),
            SqlParameterType::Jsonb => jsonb_sql_parameter(parameter),
            SqlParameterType::JsonbArray => jsonb_array_sql_parameter(parameter),
            SqlParameterType::TextArray => match parameter {
                SqlParameter::JsonbPath(path) => FilterSqlParameter::TextArray(path),
                _ => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "filter parameter cannot be bound as text[]",
                ),
            },
        })
        .collect()
}

fn infer_sql_parameter(parameter: SqlParameter) -> FilterSqlParameter {
    match parameter {
        SqlParameter::JsonValue(value) => infer_scalar_parameter(value),
        SqlParameter::RangeBound(value) => infer_range_parameter(value),
        SqlParameter::JsonArray(values) => infer_array_parameter(values),
        SqlParameter::JsonbPath(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "JSONB path cannot be bound as an inferred scalar parameter",
        ),
    }
}

fn infer_scalar_parameter(value: PayloadValue) -> FilterSqlParameter {
    match value {
        PayloadValue::Null => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "null match values require an is_null predicate",
        ),
        PayloadValue::Bool(value) => FilterSqlParameter::Bool(value),
        PayloadValue::Number(value) => number_sql_parameter(value),
        PayloadValue::String(value) => FilterSqlParameter::Text(value),
    }
}

fn infer_range_parameter(value: RangeBound) -> FilterSqlParameter {
    match value {
        RangeBound::Integer(value) => FilterSqlParameter::I64(value),
        RangeBound::Float(value) => number_sql_parameter(value),
        RangeBound::String(value) => FilterSqlParameter::Text(value),
    }
}

fn infer_array_parameter(values: Vec<PayloadValue>) -> FilterSqlParameter {
    let Some(first) = values.first() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "filter array values must not be empty",
        );
    };

    match first {
        PayloadValue::String(_) => FilterSqlParameter::TextArray(
            values
                .into_iter()
                .map(|value| match value {
                    PayloadValue::String(value) => value,
                    _ => raise_mixed_filter_array(),
                })
                .collect(),
        ),
        PayloadValue::Bool(_) => FilterSqlParameter::BoolArray(
            values
                .into_iter()
                .map(|value| match value {
                    PayloadValue::Bool(value) => value,
                    _ => raise_mixed_filter_array(),
                })
                .collect(),
        ),
        PayloadValue::Number(_) => {
            let numbers = values
                .into_iter()
                .map(|value| {
                    let PayloadValue::Number(value) = value else {
                        raise_mixed_filter_array();
                    };
                    value
                })
                .collect::<Vec<_>>();
            let integers = numbers
                .iter()
                .map(serde_json::Number::as_i64)
                .collect::<Option<Vec<_>>>();
            match integers {
                Some(integers) => FilterSqlParameter::I64Array(integers),
                None => {
                    FilterSqlParameter::F64Array(numbers.into_iter().map(number_as_f64).collect())
                }
            }
        }
        PayloadValue::Null => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "null match values require an is_null predicate",
        ),
    }
}

fn raise_mixed_filter_array() -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        "ordinary column array filters must contain one scalar type",
    )
}

fn number_sql_parameter(value: serde_json::Number) -> FilterSqlParameter {
    match value.as_i64() {
        Some(value) => FilterSqlParameter::I64(value),
        None => FilterSqlParameter::F64(number_as_f64(value)),
    }
}

fn number_as_f64(value: serde_json::Number) -> f64 {
    lossless_json_number_as_f64(&value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "filter number {value} cannot be represented exactly as f64; use an integer-only array or a value within +/-2^53"
            ),
        )
    })
}

const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

fn lossless_json_number_as_f64(value: &serde_json::Number) -> Option<f64> {
    if value
        .as_i64()
        .is_some_and(|value| value.unsigned_abs() > MAX_EXACT_F64_INTEGER)
        || value
            .as_u64()
            .is_some_and(|value| value > MAX_EXACT_F64_INTEGER)
    {
        return None;
    }
    value.as_f64()
}

fn jsonb_sql_parameter(parameter: SqlParameter) -> FilterSqlParameter {
    match parameter {
        SqlParameter::JsonValue(value) => FilterSqlParameter::Jsonb(payload_value_json(value)),
        SqlParameter::RangeBound(value) => FilterSqlParameter::Jsonb(range_bound_json(value)),
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "filter parameter cannot be bound as jsonb",
        ),
    }
}

fn jsonb_array_sql_parameter(parameter: SqlParameter) -> FilterSqlParameter {
    match parameter {
        SqlParameter::JsonArray(values) => {
            FilterSqlParameter::JsonbArray(values.into_iter().map(payload_value_json).collect())
        }
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "filter parameter cannot be bound as jsonb[]",
        ),
    }
}

fn payload_value_json(value: PayloadValue) -> Value {
    match value {
        PayloadValue::Null => Value::Null,
        PayloadValue::Bool(value) => Value::Bool(value),
        PayloadValue::Number(value) => Value::Number(value),
        PayloadValue::String(value) => Value::String(value),
    }
}

fn range_bound_json(value: RangeBound) -> Value {
    match value {
        RangeBound::Integer(value) => Value::Number(value.into()),
        RangeBound::Float(value) => Value::Number(value),
        RangeBound::String(value) => Value::String(value),
    }
}

fn shift_placeholders(sql: &str, offset: usize) -> String {
    let mut shifted = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '$' {
            shifted.push(character);
            continue;
        }

        let mut digits = String::new();
        while let Some(digit) = chars.next_if(|character| character.is_ascii_digit()) {
            digits.push(digit);
        }
        let Some(index) = digits.parse::<usize>().ok().filter(|index| *index > 0) else {
            shifted.push('$');
            shifted.push_str(&digits);
            continue;
        };
        let Some(index) = index.checked_add(offset) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "filter SQL parameter index exceeds addressable range",
            );
        };
        shifted.push('$');
        shifted.push_str(&index.to_string());
    }
    shifted
}

fn quoted_text_array(values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| super::quote_literal(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("ARRAY[{values}]::text[]")
}

#[cfg(test)]
mod tests {
    use serde_json::Number;

    use super::{MAX_EXACT_F64_INTEGER, lossless_json_number_as_f64, shift_placeholders};

    #[test]
    fn shifts_each_parameter_once_without_rewriting_new_digits() {
        assert_eq!(
            shift_placeholders("source.a = $1 AND source.b = $2 AND source.c = $10", 4),
            "source.a = $5 AND source.b = $6 AND source.c = $14"
        );
    }

    #[test]
    fn accepts_only_exact_integer_values_for_float_binding() {
        let boundary = Number::from(MAX_EXACT_F64_INTEGER);
        assert_eq!(
            lossless_json_number_as_f64(&boundary),
            Some(9_007_199_254_740_992.0)
        );

        let positive_overflow = Number::from(MAX_EXACT_F64_INTEGER + 1);
        assert_eq!(lossless_json_number_as_f64(&positive_overflow), None);

        let negative_overflow = Number::from(-9_007_199_254_740_993_i64);
        assert_eq!(lossless_json_number_as_f64(&negative_overflow), None);
    }

    #[test]
    fn accepts_fractional_json_numbers_for_float_binding() {
        let value = Number::from_f64(1.5);
        assert_eq!(
            value.as_ref().and_then(lossless_json_number_as_f64),
            Some(1.5)
        );
    }
}
