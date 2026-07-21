//! SQL predicate rendering behavior tests.

use context_filter::{
    FieldRegistry, SqlParameter, SqlParameterType, SqlPredicatePlan, parse_filter_json,
    render_sql_predicate,
};

#[test]
fn renders_boolean_predicates_over_registered_columns() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_column("price", "price_cents")?
        .register_column("archived", "archived")?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "tenant_id", "match": "acme"},
            {"key": "price", "range": {"gte": 10, "lt": 20}}
          ],
          "must_not": [
            {"key": "archived", "match": true}
          ]
        }
        "#,
    )?;
    let resolved = registry.resolve_filter(&filter)?;

    let predicate = render_sql_predicate(&resolved);

    assert_eq!(
        predicate,
        SqlPredicatePlan {
            sql: "((tenant_id = $1) AND (price_cents >= $2 AND price_cents < $3) AND NOT (archived = $4))"
                .to_owned(),
            parameters: vec![
                SqlParameter::JsonValue("acme".into()),
                SqlParameter::RangeBound(10.into()),
                SqlParameter::RangeBound(20.into()),
                SqlParameter::JsonValue(true.into()),
            ],
            parameter_types: vec![
                SqlParameterType::InferFromSql,
                SqlParameterType::InferFromSql,
                SqlParameterType::InferFromSql,
                SqlParameterType::InferFromSql,
            ],
        }
    );
    Ok(())
}

#[test]
fn renders_should_groups_and_jsonb_paths() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("status", "status")?
        .register_jsonb_path("metadata.topic", "metadata", ["topic"])?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {
              "should": [
                {"key": "status", "match": {"any": ["open", "pending"]}},
                {"key": "metadata.topic", "match": "billing"}
              ]
            }
          ]
        }
        "#,
    )?;
    let resolved = registry.resolve_filter(&filter)?;

    let predicate = render_sql_predicate(&resolved);

    assert_eq!(
        predicate,
        SqlPredicatePlan {
            sql: "((status = ANY($1)) OR ((metadata #> $2::text[]) = $3::jsonb))".to_owned(),
            parameters: vec![
                SqlParameter::JsonArray(vec!["open".into(), "pending".into()]),
                SqlParameter::JsonbPath(vec!["topic".to_owned()]),
                SqlParameter::JsonValue("billing".into()),
            ],
            parameter_types: vec![
                SqlParameterType::InferFromSql,
                SqlParameterType::TextArray,
                SqlParameterType::Jsonb,
            ],
        }
    );
    Ok(())
}
