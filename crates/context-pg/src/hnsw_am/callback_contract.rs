//! Executable inventory of HNSW PostgreSQL FFI callback boundaries.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswCallbackClass {
    Handler,
    AccessMethod,
    BuildVisitor,
    LifecycleEventTrigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswCallbackRetention {
    None,
    PostgresAllocatedResult,
    RustScanStateUntilEndOrContextReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswCallbackContract {
    pub(super) callback: &'static str,
    pub(super) safe_inner: &'static str,
    pub(super) class: HnswCallbackClass,
    pub(super) borrowed_inputs: &'static str,
    pub(super) retention: HnswCallbackRetention,
}

pub(super) const HNSW_CALLBACK_CONTRACTS: [HnswCallbackContract; 17] = [
    HnswCallbackContract {
        callback: "pgcontext_hnsw_mapped_sql_drop",
        safe_inner: "mapped_hnsw_sql_drop_safe",
        class: HnswCallbackClass::LifecycleEventTrigger,
        borrowed_inputs: "event-trigger FunctionCallInfo is PostgreSQL-owned for the call",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_handler",
        safe_inner: "hnsw_handler_safe",
        class: HnswCallbackClass::Handler,
        borrowed_inputs: "FunctionCallInfo is PostgreSQL-owned for the call",
        retention: HnswCallbackRetention::PostgresAllocatedResult,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_build",
        safe_inner: "hnsw_build_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "heap relation, index relation, and IndexInfo are live for the call",
        retention: HnswCallbackRetention::PostgresAllocatedResult,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_build_empty",
        safe_inner: "hnsw_build_empty_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "index relation is live for the call",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_insert",
        safe_inner: "hnsw_insert_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "relation, datum/null arrays, heap TID, and IndexInfo are call-bounded",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_insert_cleanup",
        safe_inner: "hnsw_insert_cleanup_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "relation and IndexInfo are live for the call",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_bulk_delete",
        safe_inner: "hnsw_bulk_delete_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "vacuum info, optional stats, callback, and callback state are call-bounded",
        retention: HnswCallbackRetention::PostgresAllocatedResult,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_vacuum_cleanup",
        safe_inner: "hnsw_vacuum_cleanup_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "vacuum info and optional stats are call-bounded",
        retention: HnswCallbackRetention::PostgresAllocatedResult,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_cost_estimate",
        safe_inner: "hnsw_cost_estimate_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "planner/path inputs are borrowed and five output pointers are writable",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_options",
        safe_inner: "hnsw_options_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "reloptions Datum belongs to PostgreSQL for the call",
        retention: HnswCallbackRetention::PostgresAllocatedResult,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_validate",
        safe_inner: "hnsw_validate_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "opclass OID is a copied scalar",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_begin_scan",
        safe_inner: "hnsw_begin_scan_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "index relation is live and key counts are nonnegative",
        retention: HnswCallbackRetention::RustScanStateUntilEndOrContextReset,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_rescan",
        safe_inner: "hnsw_rescan_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "scan and optional key arrays match their counts",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_get_tuple",
        safe_inner: "hnsw_get_tuple_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "scan descriptor and its opaque state remain live",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_get_bitmap",
        safe_inner: "hnsw_get_bitmap_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "scan descriptor and TIDBitmap are live for the call",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_end_scan",
        safe_inner: "hnsw_end_scan_safe",
        class: HnswCallbackClass::AccessMethod,
        borrowed_inputs: "scan descriptor owns at most one Rust opaque state",
        retention: HnswCallbackRetention::None,
    },
    HnswCallbackContract {
        callback: "pgcontext_hnsw_build_callback",
        safe_inner: "hnsw_build_callback_safe",
        class: HnswCallbackClass::BuildVisitor,
        borrowed_inputs: "relation, tuple arrays/TID, and build state are valid for the visitor call",
        retention: HnswCallbackRetention::None,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn callback_inventory_is_unique_complete_and_safe_inner_paired() {
        let callbacks = HNSW_CALLBACK_CONTRACTS
            .iter()
            .map(|contract| contract.callback)
            .collect::<BTreeSet<_>>();
        let inners = HNSW_CALLBACK_CONTRACTS
            .iter()
            .map(|contract| contract.safe_inner)
            .collect::<BTreeSet<_>>();

        assert_eq!(callbacks.len(), HNSW_CALLBACK_CONTRACTS.len());
        assert_eq!(inners.len(), HNSW_CALLBACK_CONTRACTS.len());
        assert!(HNSW_CALLBACK_CONTRACTS.iter().all(|contract| {
            contract.safe_inner.ends_with("_safe") && !contract.borrowed_inputs.is_empty()
        }));
        assert_eq!(
            HNSW_CALLBACK_CONTRACTS
                .iter()
                .filter(|contract| contract.class == HnswCallbackClass::AccessMethod)
                .count(),
            14
        );
    }
}
