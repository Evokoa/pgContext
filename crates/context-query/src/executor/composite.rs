//! Recursive composite-query execution over the bounded leaf executor.

use std::collections::BTreeMap;

use context_core::{PointId, SourceKey};
use context_hybrid::{
    BranchCandidate, RankedPoint, RrfK, ScoreDirection, WeightedBranch, reciprocal_rank_fusion,
    weighted_fusion,
};

use super::{QueryExecutor, cancelled, outcome};
use crate::{
    BudgetUsage, Completion, ExecutionBudget, ExecutionOutcome, ExecutionState, HydratedCandidate,
    MAX_FORMULA_OPERATIONS, QueryError, QueryIr, QueryKind, Result, ScoreOrder, StageDiagnostic,
    StageKind, types::deterministic_points,
};

const MAX_FORMULA_TOTAL_OPERATIONS: usize = 16_384;

impl QueryExecutor<'_> {
    pub(super) fn execute_composite(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        self.execute_node(query, budget)
    }

    fn execute_node(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                Vec::new(),
                BudgetUsage::default(),
            ));
        }
        match query.kind() {
            QueryKind::Prefetch { branches } => self.execute_prefetch(query, branches, budget),
            QueryKind::Weighted {
                query: child,
                weight,
            } => {
                let child = self.execute_node(child, budget)?;
                self.transform_scores(query, child, budget, "weighted_score", |score| {
                    Ok(score * weight)
                })
            }
            QueryKind::ScoreThreshold {
                query: child,
                minimum,
                maximum,
            } => {
                let child = self.execute_node(child, budget)?;
                self.filter_scores(query, child, budget, *minimum, *maximum)
            }
            QueryKind::Formula {
                query: child,
                formula,
            } => {
                let compiled = formula.compile()?;
                let child = self.execute_node(child, budget)?;
                let projected = compiled
                    .operation_count()
                    .checked_mul(child.points().len())
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "formula_evaluation_projection",
                    })?;
                if projected > MAX_FORMULA_TOTAL_OPERATIONS {
                    return Err(QueryError::WorkBudgetExceeded {
                        budget: "formula_total_operations",
                        actual: projected,
                        maximum: MAX_FORMULA_TOTAL_OPERATIONS,
                    });
                }
                self.transform_scores(query, child, budget, "formula_score", |score| {
                    compiled.evaluate(score, MAX_FORMULA_OPERATIONS)
                })
            }
            QueryKind::Rerank { query: child } => {
                let child = self.execute_node(child, budget)?;
                self.rerank(query, child, budget)
            }
            _ => self.execute_leaf(query, budget),
        }
    }

    fn execute_prefetch(
        &mut self,
        query: &QueryIr,
        branches: &[QueryIr],
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        let mut usage = BudgetUsage::default();
        let mut diagnostics = Vec::new();
        let mut branch_points = Vec::with_capacity(branches.len());
        let mut weights = Vec::with_capacity(branches.len());
        let mut any_explicit_weight = false;

        for branch in branches {
            let weight = match branch.kind() {
                QueryKind::Weighted { weight, .. } => {
                    any_explicit_weight = true;
                    *weight
                }
                _ => 1.0,
            };
            let Some(remaining) = budget.remaining(usage, branch.has_filter_in_subtree()) else {
                return Ok(ExecutionOutcome::new(
                    ExecutionState::Ready,
                    Completion::BudgetExhausted,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            };
            let child = self.execute_node(branch, remaining)?;
            diagnostics.extend_from_slice(child.diagnostics());
            usage.merge(child.usage());
            match child.completion() {
                Completion::Complete => {}
                completion => {
                    return Ok(ExecutionOutcome::new(
                        child.state().clone(),
                        completion,
                        Vec::new(),
                        diagnostics,
                        usage,
                    ));
                }
            }
            if child.state() != &ExecutionState::Ready {
                return Ok(ExecutionOutcome::new(
                    child.state().clone(),
                    Completion::Complete,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            branch_points.push((branch.score_order(), child.points().to_vec()));
            weights.push(weight);
        }

        if usage.stages() >= budget.max_stages() {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        let input_count = branch_points.iter().map(|(_, points)| points.len()).sum();
        let points = if any_explicit_weight {
            weighted_points(&branch_points, &weights, query.limit())?
        } else {
            rrf_points(&branch_points, query.limit())?
        };
        usage.add_stage();
        let diagnostic = StageDiagnostic::new(
            StageKind::Fusion,
            if any_explicit_weight {
                "normalized_weighted_fusion"
            } else {
                "reciprocal_rank_fusion"
            },
            input_count,
            points.len(),
            None,
        );
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);
        if cancelled(self.cancellation)? {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        Ok(ExecutionOutcome::new(
            ExecutionState::Ready,
            Completion::Complete,
            points,
            diagnostics,
            usage,
        ))
    }

    fn transform_scores(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        budget: ExecutionBudget,
        strategy: &'static str,
        mut transform: impl FnMut(f64) -> Result<f64>,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() == Completion::Cancelled {
            return Ok(child);
        }
        let input_count = child.points().len();
        let mut rows = Vec::with_capacity(input_count);
        for point in child.points() {
            rows.push(HydratedCandidate::new(
                point.point_id(),
                point.source_key().clone(),
                transform(point.score())?,
            )?);
        }
        self.finish_transform(
            query,
            child,
            budget,
            rows,
            StageKind::ScoreTransform,
            strategy,
        )
    }

    fn filter_scores(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        budget: ExecutionBudget,
        minimum: Option<f64>,
        maximum: Option<f64>,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() == Completion::Cancelled {
            return Ok(child);
        }
        let rows = child
            .points()
            .iter()
            .filter(|point| minimum.is_none_or(|minimum| point.score() >= minimum))
            .filter(|point| maximum.is_none_or(|maximum| point.score() <= maximum))
            .cloned()
            .collect::<Vec<_>>();
        self.finish_transform(
            query,
            child,
            budget,
            rows,
            StageKind::ScoreTransform,
            "score_threshold",
        )
    }

    fn rerank(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() == Completion::Cancelled {
            return Ok(child);
        }
        let rows = child.points().to_vec();
        self.finish_transform(
            query,
            child,
            budget,
            rows,
            StageKind::Rerank,
            "exact_rerank",
        )
    }

    fn finish_transform(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        budget: ExecutionBudget,
        rows: Vec<HydratedCandidate>,
        stage: StageKind,
        strategy: &'static str,
    ) -> Result<ExecutionOutcome> {
        if child.usage().stages() >= budget.max_stages() {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                child.diagnostics().to_vec(),
                child.usage(),
            ));
        }
        let input_count = child.points().len();
        let points = deterministic_points(rows, query.limit(), query.score_order());
        let mut diagnostics = child.diagnostics().to_vec();
        let mut usage = child.usage();
        usage.add_stage();
        let diagnostic = StageDiagnostic::new(stage, strategy, input_count, points.len(), None);
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);
        let child_completion = child.completion();
        let was_cancelled = cancelled(self.cancellation)?;
        let completion = if was_cancelled {
            Completion::Cancelled
        } else {
            child_completion
        };
        Ok(ExecutionOutcome::new(
            ExecutionState::Ready,
            completion,
            if was_cancelled { Vec::new() } else { points },
            diagnostics,
            usage,
        ))
    }
}

fn rrf_points(
    branches: &[(ScoreOrder, Vec<HydratedCandidate>)],
    limit: usize,
) -> Result<Vec<HydratedCandidate>> {
    let ranked = branches
        .iter()
        .map(|(_, points)| {
            points
                .iter()
                .map(|point| RankedPoint::new(point.point_id().get()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let ranked_refs = ranked.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let fused = reciprocal_rank_fusion(&ranked_refs, RrfK::STANDARD, limit);
    hydrate_fused(branches, fused)
}

fn weighted_points(
    branches: &[(ScoreOrder, Vec<HydratedCandidate>)],
    weights: &[f64],
    limit: usize,
) -> Result<Vec<HydratedCandidate>> {
    let candidates = branches
        .iter()
        .map(|(_, points)| {
            points
                .iter()
                .map(|point| BranchCandidate::with_score(point.point_id().get(), point.score()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let weighted = candidates
        .iter()
        .zip(branches)
        .zip(weights)
        .map(|((candidates, (order, _)), weight)| {
            WeightedBranch::new(
                candidates,
                *weight,
                match order {
                    ScoreOrder::LowerIsBetter => ScoreDirection::LowerIsBetter,
                    ScoreOrder::HigherIsBetter => ScoreDirection::HigherIsBetter,
                },
            )
        })
        .collect::<Vec<_>>();
    let fused = weighted_fusion(&weighted, limit).map_err(|error| QueryError::InvalidInput {
        field: "weight",
        reason: format!("weighted fusion failed: {error:?}"),
    })?;
    hydrate_fused(branches, fused)
}

fn hydrate_fused(
    branches: &[(ScoreOrder, Vec<HydratedCandidate>)],
    fused: Vec<context_hybrid::FusedPoint>,
) -> Result<Vec<HydratedCandidate>> {
    let mut sources = BTreeMap::<PointId, SourceKey>::new();
    for point in branches.iter().flat_map(|(_, points)| points) {
        if let Some(existing) = sources.get(&point.point_id())
            && existing != point.source_key()
        {
            return Err(QueryError::PortFailure {
                stage: "fusion",
                message: format!(
                    "point ID {} resolved to inconsistent source keys across branches",
                    point.point_id().get()
                ),
            });
        }
        sources.insert(point.point_id(), point.source_key().clone());
    }
    fused
        .into_iter()
        .map(|point| {
            let point_id = PointId::new(point.point_id());
            let source_key =
                sources
                    .get(&point_id)
                    .cloned()
                    .ok_or(QueryError::UnexpectedPointId {
                        stage: "fusion",
                        point_id,
                    })?;
            HydratedCandidate::new(point_id, source_key, point.score())
        })
        .collect()
}
