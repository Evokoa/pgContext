//! Executable inventory of IVFFlat PostgreSQL FFI callback boundaries.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IvfflatCallbackClass {
    Handler,
    AccessMethod,
    BuildVisitor,
    ParallelWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IvfflatCallbackRetention {
    None,
    PostgresAllocatedResult,
    RustScanStateUntilEndOrContextReset,
    SharedWorkerRunUntilLeaderMerge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IvfflatCallbackContract {
    pub(super) callback: &'static str,
    pub(super) class: IvfflatCallbackClass,
    pub(super) borrowed_inputs: &'static str,
    pub(super) retention: IvfflatCallbackRetention,
}

pub(super) const IVFFLAT_CALLBACK_CONTRACTS: [IvfflatCallbackContract; 17] = [
    contract(
        "pgcontext_ivfflat_handler",
        IvfflatCallbackClass::Handler,
        "FunctionCallInfo is live for the guarded call",
        IvfflatCallbackRetention::PostgresAllocatedResult,
    ),
    contract(
        "ivfflat_build_phase_name",
        IvfflatCallbackClass::AccessMethod,
        "phase is a copied scalar",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_build",
        IvfflatCallbackClass::AccessMethod,
        "heap/index relations and IndexInfo remain locked for the call and joined worker lifecycle",
        IvfflatCallbackRetention::PostgresAllocatedResult,
    ),
    contract(
        "ivfflat_build_callback",
        IvfflatCallbackClass::BuildVisitor,
        "relation, datum/null arrays, TID, and exclusive collector state are call-bounded",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_build_empty",
        IvfflatCallbackClass::AccessMethod,
        "index relation is exclusively live for the call",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_insert",
        IvfflatCallbackClass::AccessMethod,
        "relation, datum/null arrays, heap TID, and IndexInfo are call-bounded",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_cost_estimate",
        IvfflatCallbackClass::AccessMethod,
        "planner inputs are borrowed and output cost pointers are writable",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_validate",
        IvfflatCallbackClass::AccessMethod,
        "opclass OID is a copied scalar",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_begin_scan",
        IvfflatCallbackClass::AccessMethod,
        "index relation is live and scan-key counts are copied scalars",
        IvfflatCallbackRetention::RustScanStateUntilEndOrContextReset,
    ),
    contract(
        "ivfflat_rescan",
        IvfflatCallbackClass::AccessMethod,
        "scan descriptor and counted key arrays are call-bounded",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_get_tuple",
        IvfflatCallbackClass::AccessMethod,
        "scan descriptor and memory-context-owned state remain live",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_end_scan",
        IvfflatCallbackClass::AccessMethod,
        "scan descriptor owns at most one Rust drop slot",
        IvfflatCallbackRetention::None,
    ),
    contract(
        "ivfflat_bulk_delete",
        IvfflatCallbackClass::AccessMethod,
        "VACUUM info, optional stats, callback, and callback state are call-bounded",
        IvfflatCallbackRetention::PostgresAllocatedResult,
    ),
    contract(
        "ivfflat_vacuum_cleanup",
        IvfflatCallbackClass::AccessMethod,
        "VACUUM info and optional stats are call-bounded",
        IvfflatCallbackRetention::PostgresAllocatedResult,
    ),
    contract(
        "pgcontext_ivfflat_options",
        IvfflatCallbackClass::AccessMethod,
        "reloptions datum is PostgreSQL-owned for the call",
        IvfflatCallbackRetention::PostgresAllocatedResult,
    ),
    contract(
        "pgcontext_ivfflat_parallel_build_main",
        IvfflatCallbackClass::ParallelWorker,
        "DSM segment and TOC remain attached until worker return",
        IvfflatCallbackRetention::SharedWorkerRunUntilLeaderMerge,
    ),
    contract(
        "ivfflat_parallel_build_callback",
        IvfflatCallbackClass::BuildVisitor,
        "parallel scan relation, datum/null arrays, TID, and exclusive worker collector are call-bounded",
        IvfflatCallbackRetention::None,
    ),
];

const fn contract(
    callback: &'static str,
    class: IvfflatCallbackClass,
    borrowed_inputs: &'static str,
    retention: IvfflatCallbackRetention,
) -> IvfflatCallbackContract {
    IvfflatCallbackContract {
        callback,
        class,
        borrowed_inputs,
        retention,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn callback_inventory_is_unique_complete_and_capability_scoped() {
        let callbacks = IVFFLAT_CALLBACK_CONTRACTS
            .iter()
            .map(|contract| contract.callback)
            .collect::<BTreeSet<_>>();

        assert_eq!(callbacks.len(), IVFFLAT_CALLBACK_CONTRACTS.len());
        assert!(
            IVFFLAT_CALLBACK_CONTRACTS
                .iter()
                .all(|contract| !contract.borrowed_inputs.is_empty())
        );
        assert_eq!(
            IVFFLAT_CALLBACK_CONTRACTS
                .iter()
                .filter(|contract| contract.class == IvfflatCallbackClass::AccessMethod)
                .count(),
            13
        );
        assert_eq!(
            IVFFLAT_CALLBACK_CONTRACTS
                .iter()
                .filter(|contract| contract.class == IvfflatCallbackClass::BuildVisitor)
                .count(),
            2
        );
    }
}
