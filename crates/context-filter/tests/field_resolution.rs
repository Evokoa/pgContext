//! Filter field resolution behavior tests.

use context_core::SqlIdentifier;
use context_filter::{
    Condition, FieldRegistry, FilterError, ResolvedCondition, ResolvedField, ResolvedPredicate,
    parse_filter_json,
};

#[test]
fn resolves_filter_keys_to_registered_columns() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_column("price", "price_cents")?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "tenant_id", "match": "acme"},
            {"key": "price", "range": {"gte": 10}}
          ]
        }
        "#,
    )?;

    let resolved = registry.resolve_filter(&filter)?;

    assert_eq!(resolved.must.len(), 2);
    assert_eq!(
        resolved.must[0].field(),
        Some(&ResolvedField::Column {
            key: "tenant_id".parse()?,
            column: SqlIdentifier::new("tenant_id")?,
        })
    );
    assert!(matches!(
        resolved.must[1],
        ResolvedCondition::Field {
            predicate: ResolvedPredicate::Range { .. },
            ..
        }
    ));
    Ok(())
}

#[test]
fn resolves_nested_filter_keys() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_column("status", "status")?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {
              "should": [
                {"key": "tenant_id", "match": "acme"},
                {"key": "status", "match": "open"}
              ]
            }
          ]
        }
        "#,
    )?;

    let resolved = registry.resolve_filter(&filter)?;

    assert!(matches!(
        &resolved.must[0],
        ResolvedCondition::Nested(nested) if nested.should.len() == 2
    ));
    Ok(())
}

#[test]
fn rejects_unknown_filter_keys() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .build();
    let filter = parse_filter_json(r#"{"must":[{"key":"unknown","match":"acme"}]}"#)?;

    assert!(matches!(
        registry.resolve_filter(&filter),
        Err(FilterError::UnknownPayloadField { key }) if key.as_str() == "unknown"
    ));
    Ok(())
}

#[test]
fn rejects_duplicate_registered_filter_keys() -> Result<(), Box<dyn std::error::Error>> {
    let result = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_column("tenant_id", "tenant_id_copy");

    assert!(matches!(
        result,
        Err(FilterError::DuplicatePayloadField { key }) if key.as_str() == "tenant_id"
    ));
    Ok(())
}

#[test]
fn unresolved_conditions_remain_unresolved_until_field_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let filter = parse_filter_json(r#"{"must":[{"key":"tenant_id","match":"acme"}]}"#)?;

    assert!(matches!(filter.must[0], Condition::Field { .. }));
    Ok(())
}
