//! Filter JSON parser behavior tests.

use context_filter::{
    Condition, Filter, FilterError, MatchValue, PayloadValue, Predicate, RangeBound,
    parse_filter_json,
};

#[test]
fn parses_qdrant_style_boolean_filter() -> Result<(), Box<dyn std::error::Error>> {
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "tenant_id", "match": "acme"},
            {"key": "price", "range": {"gte": 10, "lt": 20}}
          ],
          "should": [
            {"key": "metadata.topic", "match": {"value": "billing"}}
          ],
          "must_not": [
            {"key": "archived", "match": true}
          ]
        }
        "#,
    )?;

    assert_eq!(
        filter,
        Filter {
            must: vec![
                Condition::Field {
                    key: "tenant_id".parse()?,
                    predicate: Predicate::Match(MatchValue::Value(PayloadValue::String(
                        "acme".to_owned()
                    ))),
                },
                Condition::Field {
                    key: "price".parse()?,
                    predicate: Predicate::Range {
                        gt: None,
                        gte: Some(RangeBound::Integer(10)),
                        lt: Some(RangeBound::Integer(20)),
                        lte: None,
                    },
                },
            ],
            should: vec![Condition::Field {
                key: "metadata.topic".parse()?,
                predicate: Predicate::Match(MatchValue::Value(PayloadValue::String(
                    "billing".to_owned()
                ))),
            }],
            must_not: vec![Condition::Field {
                key: "archived".parse()?,
                predicate: Predicate::Match(MatchValue::Value(PayloadValue::Bool(true))),
            }],
        }
    );
    Ok(())
}

#[test]
fn parses_match_any_and_empty_predicates() -> Result<(), Box<dyn std::error::Error>> {
    let filter = parse_filter_json(
        r#"
        {
          "must": [
            {"key": "status", "match": {"any": ["new", "open"]}},
            {"key": "deleted_at", "is_null": true},
            {"key": "tags", "is_empty": false}
          ]
        }
        "#,
    )?;

    assert_eq!(filter.must.len(), 3);
    assert!(matches!(
        &filter.must[0],
        Condition::Field {
            predicate: Predicate::Match(MatchValue::Any(values)),
            ..
        } if values.len() == 2
    ));
    assert!(matches!(
        filter.must[1],
        Condition::Field {
            predicate: Predicate::IsNull(true),
            ..
        }
    ));
    assert!(matches!(
        filter.must[2],
        Condition::Field {
            predicate: Predicate::IsEmpty(false),
            ..
        }
    ));
    Ok(())
}

#[test]
fn rejects_unknown_filter_and_condition_fields() {
    assert!(matches!(
        parse_filter_json(r#"{"must":[],"extra":[]}"#),
        Err(FilterError::UnknownField { field }) if field == "extra"
    ));
    assert!(matches!(
        parse_filter_json(r#"{"must":[{"key":"tenant_id","match":"acme","extra":true}]}"#),
        Err(FilterError::UnknownField { field }) if field == "extra"
    ));
}

#[test]
fn rejects_empty_filters_and_invalid_predicate_shapes() {
    assert!(matches!(
        parse_filter_json("{}"),
        Err(FilterError::EmptyFilter)
    ));
    assert!(matches!(
        parse_filter_json(r#"{"must":[{"key":"tenant_id"}]}"#),
        Err(FilterError::MissingPredicate { key }) if key.as_str() == "tenant_id"
    ));
    assert!(matches!(
        parse_filter_json(r#"{"must":[{"key":"tenant_id","match":{}}]}"#),
        Err(FilterError::InvalidPredicate { .. })
    ));
}

#[test]
fn enforces_filter_depth_and_node_budgets() {
    let too_deep = format!(
        r#"{{
          "must": [{}]
        }}"#,
        nested_filter(0, context_core::policy::MAX_FILTER_DEPTH + 1)
    );
    assert!(matches!(
        parse_filter_json(&too_deep),
        Err(FilterError::BudgetExceeded { budget, .. })
            if budget == "filter depth"
    ));

    let mut conditions = Vec::new();
    for index in 0..=context_core::policy::MAX_FILTER_NODES {
        conditions.push(format!(r#"{{"key":"field_{index}","match":{index}}}"#));
    }
    let too_many_nodes = format!(r#"{{"must":[{}]}}"#, conditions.join(","));
    assert!(matches!(
        parse_filter_json(&too_many_nodes),
        Err(FilterError::BudgetExceeded { budget, .. })
            if budget == "filter nodes"
    ));
}

fn nested_filter(depth: usize, max_depth: usize) -> String {
    if depth == max_depth {
        return r#"{"key":"tenant_id","match":"acme"}"#.to_owned();
    }
    format!(r#"{{"must":[{}]}}"#, nested_filter(depth + 1, max_depth))
}
