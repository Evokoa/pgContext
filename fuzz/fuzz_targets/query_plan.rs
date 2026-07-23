#![no_main]

use context_core::{PointId, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, ExecutionBudget,
    HydratedCandidate, QueryExecutor, QueryIr, SourceReadiness, SourceRechecker, StageDiagnostic,
    TelemetrySink, parse_query_plan,
};
use libfuzzer_sys::fuzz_target;

struct Source;

impl CandidateSource for Source {
    fn readiness(&mut self, _query: &QueryIr) -> context_query::Result<SourceReadiness> {
        Ok(SourceReadiness::Ready)
    }

    fn candidates(
        &mut self,
        _query: &QueryIr,
        _filter: Option<&context_query::FilterCandidateBatch>,
        limit: usize,
    ) -> context_query::Result<CandidatePage> {
        let rows = (1..=limit.min(8))
            .map(|point_id| {
                Candidate::new(
                    PointId::new(point_id as u64),
                    point_id as f64,
                    CandidateBranch::DenseExact,
                )
            })
            .collect::<context_query::Result<Vec<_>>>()?;
        Ok(CandidatePage::new(rows, true))
    }
}

struct Rechecker;

impl SourceRechecker for Rechecker {
    fn recheck(
        &mut self,
        _query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> context_query::Result<Vec<HydratedCandidate>> {
        candidates
            .iter()
            .take(limit)
            .map(|candidate| {
                HydratedCandidate::new(
                    candidate.point_id(),
                    SourceKey::new(candidate.point_id().get().to_string())?,
                    candidate.score(),
                )
            })
            .collect()
    }
}

struct Telemetry;

impl TelemetrySink for Telemetry {
    fn record(&mut self, _diagnostic: &StageDiagnostic) -> context_query::Result<()> {
        Ok(())
    }
}

struct NeverCancel;

impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice(data) else {
        return;
    };
    let Ok(query) = parse_query_plan(&value) else {
        return;
    };

    let Ok(budget) = ExecutionBudget::new(64, 64, 64, 64, 8, 8) else {
        return;
    };
    let _ = QueryExecutor::new(
        &mut Source,
        None,
        &mut Rechecker,
        &mut Telemetry,
        &NeverCancel,
    )
    .execute(&query, budget);
});
