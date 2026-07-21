//! Property tests for filter rendering safety invariants.

use core::fmt;

use context_filter::{
    FieldRegistry, SqlParameter, SqlParameterBinding, SqlParameterType, parse_filter_json,
    render_sql_predicate,
};
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        .. ProptestConfig::default()
    })]

    #[test]
    fn jsonb_path_and_value_text_never_render_as_sql(
        path in prop::collection::vec(generated_text("path_value_"), 1..=4),
        value in generated_text("match_value_"),
    ) {
        let registry = must(
            FieldRegistry::builder()
                .register_jsonb_path("metadata.field", "metadata", path.clone())
                .map(|builder| builder.build())
        )?;
        let filter_json = json!({
            "must": [
                {"key": "metadata.field", "match": value.clone()}
            ]
        })
        .to_string();
        let filter = must(parse_filter_json(&filter_json))?;
        let resolved = must(registry.resolve_filter(&filter))?;

        let predicate = render_sql_predicate(&resolved);

        prop_assert!(!predicate.sql.contains(&value));
        for segment in &path {
            prop_assert!(!predicate.sql.contains(segment));
        }
        prop_assert_eq!(
            predicate.bindings(),
            vec![
                SqlParameterBinding {
                    index: 1,
                    parameter_type: SqlParameterType::TextArray,
                    value: SqlParameter::JsonbPath(path),
                },
                SqlParameterBinding {
                    index: 2,
                    parameter_type: SqlParameterType::Jsonb,
                    value: SqlParameter::JsonValue(value.into()),
                },
            ]
        );
    }

    #[test]
    fn generated_scalar_filters_round_trip_to_contiguous_placeholders(
        tenant in generated_text("tenant_value_"),
        status in generated_text("status_value_"),
        lower in -1_000_i64..1_000,
        width in 1_i64..1_000,
    ) {
        let upper = lower + width;
        let registry = must(
            FieldRegistry::builder()
                .register_column("tenant_id", "tenant_id")
                .and_then(|builder| builder.register_column("status", "status"))
                .and_then(|builder| builder.register_column("score", "score"))
                .map(|builder| builder.build())
        )?;
        let filter_json = json!({
            "must": [
                {"key": "tenant_id", "match": tenant.clone()},
                {"key": "score", "range": {"gte": lower, "lt": upper}}
            ],
            "should": [
                {"key": "status", "match": {"any": [status.clone(), "fallback"]}}
            ]
        })
        .to_string();
        let filter = must(parse_filter_json(&filter_json))?;
        let resolved = must(registry.resolve_filter(&filter))?;

        let predicate = render_sql_predicate(&resolved);
        let bindings = predicate.bindings();

        prop_assert_eq!(bindings.len(), predicate.parameters.len());
        prop_assert_eq!(bindings.len(), predicate.parameter_types.len());
        for binding in bindings {
            let placeholder = format!("${}", binding.index);
            prop_assert!(predicate.sql.contains(&placeholder));
            prop_assert_eq!(binding.parameter_type, SqlParameterType::InferFromSql);
        }
        prop_assert!(!predicate.sql.contains(&tenant));
        prop_assert!(!predicate.sql.contains(&status));
        prop_assert!(!predicate.sql.contains("fallback"));
    }
}

fn generated_text(prefix: &'static str) -> impl Strategy<Value = String> {
    prop::collection::vec(
        any::<char>().prop_filter("no NUL bytes", |value| *value != '\0'),
        0..16,
    )
    .prop_map(move |suffix| {
        let suffix = suffix.into_iter().collect::<String>();
        format!("{prefix}{suffix}")
    })
}

fn must<T, E: fmt::Display>(result: Result<T, E>) -> Result<T, TestCaseError> {
    result.map_err(|error| TestCaseError::fail(error.to_string()))
}
