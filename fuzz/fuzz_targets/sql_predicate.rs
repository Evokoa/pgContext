#![no_main]

use context_filter::{FieldRegistry, parse_filter_json, render_sql_predicate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = core::str::from_utf8(data) {
        let Ok(filter) = parse_filter_json(input) else {
            return;
        };
        let Ok(registry) = FieldRegistry::builder()
            .register_column("tenant_id", "tenant_id")
            .and_then(|builder| builder.register_column("status", "status"))
            .and_then(|builder| builder.register_column("score", "score"))
            .and_then(|builder| {
                builder.register_jsonb_path("metadata.topic", "metadata", ["topic"])
            })
            .and_then(|builder| {
                builder.register_jsonb_path(
                    "metadata.billing.state",
                    "metadata",
                    ["billing", "state"],
                )
            })
            .map(|builder| builder.build())
        else {
            return;
        };
        let Ok(resolved) = registry.resolve_filter(&filter) else {
            return;
        };

        let predicate = render_sql_predicate(&resolved);
        assert_eq!(predicate.parameters.len(), predicate.parameter_types.len());
        for index in 1..=predicate.parameters.len() {
            assert!(predicate.sql.contains(&format!("${index}")));
        }
    }
});
