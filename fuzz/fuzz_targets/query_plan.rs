#![no_main]

use context_core::{PointId, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, ExecutionBudget,
    Formula, HydratedCandidate, QueryExecutor, QueryIr, QueryKind, ScoreOrder, SourceReadiness,
    SourceRechecker, StageDiagnostic, TelemetrySink,
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
    let limit = usize::from(data.first().copied().unwrap_or(1) % 8 + 1);
    let Ok(mut query) = QueryIr::nearest(
        None,
        vec![1.0, 0.0],
        ScoreOrder::HigherIsBetter,
        None,
        limit,
    ) else {
        return;
    };

    for byte in data.iter().copied().skip(1).take(24) {
        let kind = match byte % 6 {
            0 => QueryKind::Weighted {
                query: Box::new(query.clone()),
                weight: f64::from(byte) / 32.0,
            },
            1 => QueryKind::ScoreThreshold {
                query: Box::new(query.clone()),
                minimum: Some(f64::from(byte % 8)),
                maximum: None,
            },
            2 => QueryKind::Formula {
                query: Box::new(query.clone()),
                formula: match Formula::new(if byte & 8 == 0 {
                    "$score * 2 + 1"
                } else {
                    "invalid($score)"
                }) {
                    Ok(formula) => formula,
                    Err(_) => return,
                },
            },
            3 => QueryKind::Rerank {
                query: Box::new(query.clone()),
            },
            4 => QueryKind::Prefetch {
                branches: vec![query.clone(), query.clone()],
            },
            _ => continue,
        };
        let Ok(next) = QueryIr::new(kind, ScoreOrder::HigherIsBetter, None, limit) else {
            break;
        };
        query = next;
    }

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
