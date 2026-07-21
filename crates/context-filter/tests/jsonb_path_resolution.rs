//! JSONB filter path resolution behavior tests.

use context_core::SqlIdentifier;
use context_filter::{
    FieldRegistry, FilterError, JsonbPath, ResolvedCondition, ResolvedField, parse_filter_json,
};

#[test]
fn resolves_filter_keys_to_registered_jsonb_paths() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_jsonb_path("metadata.topic", "metadata", ["topic"])?
        .register_jsonb_path("metadata.billing.state", "metadata", ["billing", "state"])?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "metadata.topic", "match": "billing"},
            {"key": "metadata.billing.state", "match": "open"}
          ]
        }
        "#,
    )?;

    let resolved = registry.resolve_filter(&filter)?;

    assert_eq!(
        resolved.must[0].field(),
        Some(&ResolvedField::JsonbPath {
            key: "metadata.topic".parse()?,
            column: SqlIdentifier::new("metadata")?,
            path: JsonbPath::new(["topic"])?,
        })
    );
    assert_eq!(
        resolved.must[1].field(),
        Some(&ResolvedField::JsonbPath {
            key: "metadata.billing.state".parse()?,
            column: SqlIdentifier::new("metadata")?,
            path: JsonbPath::new(["billing", "state"])?,
        })
    );
    Ok(())
}

#[test]
fn resolves_jsonb_paths_inside_nested_filters() -> Result<(), Box<dyn std::error::Error>> {
    let registry = FieldRegistry::builder()
        .register_column("tenant_id", "tenant_id")?
        .register_jsonb_path("metadata.topic", "metadata", ["topic"])?
        .build();
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {
              "should": [
                {"key": "tenant_id", "match": "acme"},
                {"key": "metadata.topic", "match": "billing"}
              ]
            }
          ]
        }
        "#,
    )?;

    let resolved = registry.resolve_filter(&filter)?;

    assert!(matches!(
        &resolved.must[0],
        ResolvedCondition::Nested(nested)
            if matches!(nested.should[1].field(), Some(ResolvedField::JsonbPath { .. }))
    ));
    Ok(())
}

#[test]
fn rejects_invalid_jsonb_path_registrations() -> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(
        JsonbPath::new([""]),
        Err(FilterError::InvalidJsonbPath { .. })
    ));

    let too_deep = (0..=context_core::policy::MAX_FILTER_PATH_DEPTH)
        .map(|index| format!("segment_{index}"))
        .collect::<Vec<_>>();
    assert!(matches!(
        JsonbPath::new(too_deep),
        Err(FilterError::BudgetExceeded { budget, .. })
            if budget == "JSONB path depth"
    ));

    let result = FieldRegistry::builder()
        .register_jsonb_path("metadata.topic", "metadata", ["topic"])?
        .register_column("metadata.topic", "metadata_topic");
    assert!(matches!(
        result,
        Err(FilterError::DuplicatePayloadField { key }) if key.as_str() == "metadata.topic"
    ));

    Ok(())
}
