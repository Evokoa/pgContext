//! SPI parameterization contract tests.

use context_filter::{
    FieldRegistry, SqlParameter, SqlParameterBinding, SqlParameterType, parse_filter_json,
    render_sql_predicate,
};

#[test]
fn records_contiguous_bindings_for_column_predicates() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_column("price", "price_cents")?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "tenant_id", "match": "acme"},
            {"key": "price", "range": {"gte": 10, "lt": 20}}
          ]
        }
        "#,
    )?;
    let resolved = registry.resolve_filter(&filter)?;

    let predicate = render_sql_predicate(&resolved);

    assert_eq!(
        predicate.bindings(),
        vec![
            SqlParameterBinding {
                index: 1,
                parameter_type: SqlParameterType::InferFromSql,
                value: SqlParameter::JsonValue("acme".into()),
            },
            SqlParameterBinding {
                index: 2,
                parameter_type: SqlParameterType::InferFromSql,
                value: SqlParameter::RangeBound(10.into()),
            },
            SqlParameterBinding {
                index: 3,
                parameter_type: SqlParameterType::InferFromSql,
                value: SqlParameter::RangeBound(20.into()),
            },
        ]
    );
    Ok(())
}

#[test]
fn records_explicit_spi_types_for_jsonb_path_predicates() -> Result<(), Box<dyn std::error::Error>>
{
    let registry = FieldRegistry::builder()
        .register_jsonb_path("metadata.topic", "metadata", ["topic"])?
        .build();
    let filter = parse_filter_json(r#"{"must":[{"key":"metadata.topic","match":"billing"}]}"#)?;
    let resolved = registry.resolve_filter(&filter)?;

    let predicate = render_sql_predicate(&resolved);

    assert_eq!(predicate.sql, "((metadata #> $1::text[]) = $2::jsonb)");
    assert_eq!(
        predicate.bindings(),
        vec![
            SqlParameterBinding {
                index: 1,
                parameter_type: SqlParameterType::TextArray,
                value: SqlParameter::JsonbPath(vec!["topic".to_owned()]),
            },
            SqlParameterBinding {
                index: 2,
                parameter_type: SqlParameterType::Jsonb,
                value: SqlParameter::JsonValue("billing".into()),
            },
        ]
    );
    Ok(())
}
