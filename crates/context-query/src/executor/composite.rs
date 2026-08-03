//! Recursive composite-query execution over the bounded leaf executor.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::mem::size_of;

use context_core::{PointId, SourceKey, policy::MAX_SOURCE_KEY_BYTES};
use context_hybrid::{
    RankedPoint, RrfK, WeightedRankedBranch, reciprocal_rank_fusion,
    weighted_reciprocal_rank_fusion,
};

use super::{
    ExecutionDeadline, QueryExecutor, cancelled, hydrated_allocation_bytes, outcome,
    reject_duplicate_candidates, reject_duplicate_hydrated,
};
use crate::{
    BudgetUsage, Completion, ExecutionBudget, ExecutionOutcome, ExecutionState, Fusion,
    HydratedCandidate, MAX_FORMULA_OPERATIONS, QueryError, QueryIr, QueryKind, Result, ScoreOrder,
    StageDiagnostic, StageKind, types::deterministic_points,
};

const MAX_FORMULA_TOTAL_OPERATIONS: usize = 16_384;
const TOPOLOGY_REQUIRED_STAGES: usize = 2;

fn port_allocation_fits<T>(
    count: usize,
    port_budget: crate::PortBudget,
    hydrate_source_keys: bool,
) -> bool {
    let Some(memory_bytes) = count.checked_mul(size_of::<T>()) else {
        return false;
    };
    if memory_bytes > port_budget.max_memory_bytes() {
        return false;
    }
    if !hydrate_source_keys {
        return true;
    }
    count
        .checked_mul(MAX_SOURCE_KEY_BYTES)
        .is_some_and(|bytes| bytes <= port_budget.max_hydration_bytes())
}

fn budget_exhausted_child(child: ExecutionOutcome) -> ExecutionOutcome {
    ExecutionOutcome::new(
        child.state().clone(),
        Completion::BudgetExhausted,
        Vec::new(),
        child.diagnostics().to_vec(),
        child.usage(),
    )
}

fn cancelled_child(child: ExecutionOutcome) -> ExecutionOutcome {
    ExecutionOutcome::new(
        child.state().clone(),
        Completion::Cancelled,
        Vec::new(),
        child.diagnostics().to_vec(),
        child.usage(),
    )
}

fn stopped_child(
    child: &ExecutionOutcome,
    completion: Completion,
    usage: BudgetUsage,
) -> ExecutionOutcome {
    ExecutionOutcome::new(
        child.state().clone(),
        completion,
        Vec::new(),
        child.diagnostics().to_vec(),
        usage,
    )
}

impl QueryExecutor<'_> {
    pub(super) fn execute_composite(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionOutcome> {
        self.execute_node(query, budget, deadline)
    }

    fn execute_node(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionOutcome> {
        let mut initial_usage = BudgetUsage::default();
        if let Some(completion) = self.checkpoint(deadline, &mut initial_usage)? {
            return Ok(outcome(completion, Vec::new(), Vec::new(), initial_usage));
        }
        match query.kind() {
            QueryKind::Prefetch { branches, fusion } => {
                self.execute_prefetch(query, branches, *fusion, budget, deadline)
            }
            QueryKind::Weighted {
                query: child,
                weight,
            } => {
                let child = self.execute_node(child, budget, deadline)?;
                let comparisons = child.points().len();
                self.transform_scores(
                    query,
                    child,
                    budget,
                    "weighted_score",
                    comparisons,
                    |score| Ok(score * weight),
                )
            }
            QueryKind::ScoreThreshold {
                query: child,
                minimum,
                maximum,
            } => {
                let child = self.execute_node(child, budget, deadline)?;
                self.filter_scores(query, child, budget, *minimum, *maximum)
            }
            QueryKind::Formula {
                query: child,
                formula,
            } => {
                let compiled = formula.compile()?;
                let child = self.execute_node(child, budget, deadline)?;
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
                self.transform_scores(query, child, budget, "formula_score", projected, |score| {
                    compiled.evaluate(score, MAX_FORMULA_OPERATIONS)
                })
            }
            QueryKind::Rerank { query: child } => {
                let child = self.execute_node(child, budget, deadline)?;
                self.rerank(query, child, budget)
            }
            QueryKind::ExternalRerank {
                query: child,
                model_revision,
            } => {
                let child = self.execute_node(child, budget, deadline)?;
                self.external_rerank(query, child, *model_revision, budget, deadline)
            }
            QueryKind::TopologyExpand {
                query: child,
                max_depth,
            } => {
                let child = self.execute_node(child, budget, deadline)?;
                self.topology_expand(query, child, *max_depth, budget, deadline)
            }
            _ => self.execute_leaf(query, budget, deadline),
        }
    }

    fn external_rerank(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        model_revision: u64,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() != Completion::Complete {
            return Ok(child);
        }
        if child.usage().stages() >= budget.max_stages() {
            return Ok(budget_exhausted_child(child));
        }
        let mut usage = child.usage();
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(stopped_child(&child, completion, usage));
        }
        let port_budget = self.port_budget(budget, usage, deadline)?;
        let reranker = self
            .external_reranker
            .as_deref_mut()
            .ok_or(QueryError::PortFailure {
                stage: "external_reranker",
                message: "query requires an external reranker port".to_owned(),
            })?;
        if port_budget.remaining_elapsed_micros() == 0 {
            return Ok(budget_exhausted_child(child));
        }
        if cancelled(self.cancellation)? {
            return Ok(cancelled_child(child));
        }
        let limit = query.limit().min(child.points().len());
        if !port_allocation_fits::<HydratedCandidate>(limit, port_budget, true) {
            return Ok(budget_exhausted_child(child));
        }
        let page = reranker.rerank(query, child.points(), limit, port_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(stopped_child(&child, completion, usage));
        }
        if page.rows().len() > limit {
            return Err(QueryError::PortContractViolation {
                stage: "external_reranker",
                requested: limit,
                returned: page.rows().len(),
            });
        }
        if page.exhausted() && page.rows().len() != limit {
            return Err(QueryError::PortContractViolation {
                stage: "external_reranker_complete_page",
                requested: limit,
                returned: page.rows().len(),
            });
        }
        if page.model_revision() != model_revision {
            return Err(QueryError::PortFailure {
                stage: "external_reranker",
                message: "adapter returned a different model revision".to_owned(),
            });
        }
        if page.comparisons() > port_budget.max_comparisons() {
            return Err(QueryError::PortContractViolation {
                stage: "external_reranker_comparisons",
                requested: port_budget.max_comparisons(),
                returned: page.comparisons(),
            });
        }
        reject_duplicate_hydrated("external_reranker", page.rows())?;
        let response_bytes = hydrated_allocation_bytes(page.rows());
        let output_bytes = page.rows().iter().fold(0_usize, |total, row| {
            let contribution_bytes = child
                .points()
                .iter()
                .find(|candidate| candidate.point_id() == row.point_id())
                .map_or(0, |candidate| {
                    candidate
                        .contributions()
                        .len()
                        .saturating_mul(size_of::<crate::BranchContribution>())
                });
            total
                .saturating_add(size_of::<HydratedCandidate>())
                .saturating_add(row.source_key().as_str().len())
                .saturating_add(contribution_bytes)
        });
        let map_bytes = child
            .points()
            .len()
            .saturating_mul(size_of::<PointId>() + size_of::<&HydratedCandidate>());
        usage.add_comparisons(page.comparisons());
        usage.add_memory_bytes(
            response_bytes
                .saturating_add(output_bytes)
                .saturating_add(map_bytes),
        );
        usage.add_hydration_bytes(
            page.rows()
                .iter()
                .map(|row| row.source_key().as_str().len())
                .sum(),
        );
        if budget.exhausted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let input = child
            .points()
            .iter()
            .map(|row| (row.point_id(), row))
            .collect::<BTreeMap<_, _>>();
        let rows = page
            .rows()
            .iter()
            .map(|row| {
                let original = input
                    .get(&row.point_id())
                    .ok_or(QueryError::UnexpectedPointId {
                        stage: "external_reranker",
                        point_id: row.point_id(),
                    })?;
                if original.source_key() != row.source_key() {
                    return Err(QueryError::PortFailure {
                        stage: "external_reranker",
                        message: "adapter changed an authoritative source key".to_owned(),
                    });
                }
                Ok(row
                    .clone()
                    .with_contributions(original.contributions().to_vec()))
            })
            .collect::<Result<Vec<_>>>()?;
        usage.add_stage();
        let mut diagnostics = child.diagnostics().to_vec();
        let diagnostic = StageDiagnostic::new(
            StageKind::ExternalRerank,
            "external_reranker",
            child.points().len(),
            rows.len(),
            None,
        );
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                completion,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if !page.exhausted() || budget.exhausted(usage) {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        Ok(ExecutionOutcome::new(
            ExecutionState::Ready,
            Completion::Complete,
            deterministic_points(rows, query.limit(), query.score_order()),
            diagnostics,
            usage,
        ))
    }

    fn topology_expand(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        max_depth: usize,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() != Completion::Complete {
            return Ok(child);
        }
        let Some(remaining) = budget.remaining(child.usage(), false) else {
            return Ok(budget_exhausted_child(child));
        };
        if remaining.max_stages() < TOPOLOGY_REQUIRED_STAGES {
            return Ok(budget_exhausted_child(child));
        }
        let mut usage = child.usage();
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(stopped_child(&child, completion, usage));
        }
        let port_budget = self.port_budget(budget, usage, deadline)?;
        let expander = self
            .topology_expander
            .as_deref_mut()
            .ok_or(QueryError::PortFailure {
                stage: "topology_expander",
                message: "query requires a topology expander port".to_owned(),
            })?;
        if cancelled(self.cancellation)? {
            return Ok(cancelled_child(child));
        }
        let limit = remaining.max_candidates().min(query.limit());
        if !port_allocation_fits::<crate::Candidate>(limit, port_budget, false) {
            return Ok(budget_exhausted_child(child));
        }
        let page = expander.expand(query, child.points(), max_depth, limit, port_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(stopped_child(&child, completion, usage));
        }
        if page.candidates().len() > limit {
            return Err(QueryError::PortContractViolation {
                stage: "topology_expander",
                requested: limit,
                returned: page.candidates().len(),
            });
        }
        if page.candidates().iter().any(|candidate| {
            candidate.provenance().branch() != crate::CandidateBranch::Topology
                || candidate.provenance().source() != crate::CandidateSourceKind::Topology
        }) {
            return Err(QueryError::PortFailure {
                stage: "topology_expander",
                message: "adapter returned a candidate outside the topology registry".to_owned(),
            });
        }
        reject_duplicate_candidates("topology_expander", page.candidates())?;
        let seed_ids = child
            .points()
            .iter()
            .map(HydratedCandidate::point_id)
            .collect::<BTreeSet<_>>();
        if let Some(candidate) = page
            .candidates()
            .iter()
            .find(|candidate| seed_ids.contains(&candidate.point_id()))
        {
            return Err(QueryError::PortFailure {
                stage: "topology_expander",
                message: format!(
                    "adapter returned seed point ID {} as an expansion",
                    candidate.point_id().get()
                ),
            });
        }
        if page.scored_count() > port_budget.max_comparisons() {
            return Err(QueryError::PortContractViolation {
                stage: "topology_comparisons",
                requested: port_budget.max_comparisons(),
                returned: page.scored_count(),
            });
        }
        if page.expansion_count() > remaining.max_expansions() {
            return Err(QueryError::PortContractViolation {
                stage: "topology_expander",
                requested: remaining.max_expansions(),
                returned: page.expansion_count(),
            });
        }
        usage.add_candidates(page.candidates().len());
        usage.add_comparisons(page.scored_count());
        usage.add_expansions(page.expansion_count());
        usage.add_memory_bytes(
            page.candidates()
                .len()
                .saturating_mul(size_of::<crate::Candidate>()),
        );
        if page.candidates().is_empty() {
            usage.add_stage();
            let mut diagnostics = child.diagnostics().to_vec();
            let expansion = StageDiagnostic::new(
                StageKind::TopologyExpansion,
                page.strategy(),
                child.points().len(),
                0,
                None,
            );
            self.telemetry.record(&expansion)?;
            diagnostics.push(expansion);
            if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
                return Ok(ExecutionOutcome::new(
                    ExecutionState::Ready,
                    completion,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                if page.exhausted() {
                    Completion::Complete
                } else {
                    Completion::BudgetExhausted
                },
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if budget.resources_depleted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let recheck_limit = remaining.max_rechecks().min(page.candidates().len());
        let recheck_budget = self.port_budget(budget, usage, deadline)?;
        if !port_allocation_fits::<HydratedCandidate>(recheck_limit, recheck_budget, true) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let recheck_page =
            self.rechecker
                .recheck(query, page.candidates(), recheck_limit, recheck_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(stopped_child(&child, completion, usage));
        }
        if recheck_page.rows().len() > recheck_limit {
            return Err(QueryError::PortContractViolation {
                stage: "topology_source_rechecker",
                requested: recheck_limit,
                returned: recheck_page.rows().len(),
            });
        }
        if recheck_page.comparisons() > recheck_budget.max_comparisons() {
            return Err(QueryError::PortContractViolation {
                stage: "topology_recheck_comparisons",
                requested: recheck_budget.max_comparisons(),
                returned: recheck_page.comparisons(),
            });
        }
        reject_duplicate_hydrated("topology_source_rechecker", recheck_page.rows())?;
        let recheck_comparisons = recheck_page.comparisons();
        let rows = recheck_page.into_rows();
        let hydration_bytes = rows
            .iter()
            .map(|row| row.source_key().as_str().len())
            .sum::<usize>();
        let projected_bytes = hydrated_allocation_bytes(&rows)
            .saturating_add(
                rows.len()
                    .saturating_mul(size_of::<crate::BranchContribution>()),
            )
            .saturating_add(
                page.candidates()
                    .len()
                    .saturating_mul(size_of::<PointId>() + size_of::<&crate::Candidate>()),
            );
        usage.add_comparisons(recheck_comparisons);
        usage.add_rechecks(recheck_limit);
        usage.add_hydration_bytes(hydration_bytes);
        usage.add_memory_bytes(projected_bytes);
        if budget.exhausted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let candidates = page
            .candidates()
            .iter()
            .map(|candidate| (candidate.point_id(), candidate))
            .collect::<BTreeMap<_, _>>();
        let rows = rows
            .into_iter()
            .map(|row| {
                let source_score = row.score();
                let candidate =
                    candidates
                        .get(&row.point_id())
                        .ok_or(QueryError::UnexpectedPointId {
                            stage: "topology_source_rechecker",
                            point_id: row.point_id(),
                        })?;
                Ok(
                    row.with_contributions(vec![crate::BranchContribution::source(
                        candidate.provenance(),
                        source_score,
                        candidate.diagnostics().source_rank(),
                    )]),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        usage.add_stage();
        usage.add_stage();
        let mut diagnostics = child.diagnostics().to_vec();
        let expansion = StageDiagnostic::new(
            StageKind::TopologyExpansion,
            page.strategy(),
            child.points().len(),
            page.candidates().len(),
            None,
        );
        let recheck = StageDiagnostic::new(
            StageKind::SourceRecheck,
            "topology_source_recheck",
            recheck_limit,
            rows.len(),
            None,
        );
        self.telemetry.record(&expansion)?;
        self.telemetry.record(&recheck)?;
        diagnostics.push(expansion);
        diagnostics.push(recheck);
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                completion,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if !page.exhausted()
            || recheck_limit < page.candidates().len()
            || budget.exhausted(usage)
            || usage.stages() > budget.max_stages()
        {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        Ok(ExecutionOutcome::new(
            ExecutionState::Ready,
            Completion::Complete,
            deterministic_points(rows, query.limit(), query.score_order()),
            diagnostics,
            usage,
        ))
    }

    fn execute_prefetch(
        &mut self,
        query: &QueryIr,
        branches: &[QueryIr],
        fusion: Fusion,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionOutcome> {
        let mut usage = BudgetUsage::default();
        let mut diagnostics = Vec::new();
        usage.add_memory_bytes(
            branches
                .len()
                .saturating_mul(size_of::<(ScoreOrder, Vec<HydratedCandidate>)>())
                .saturating_add(branches.len().saturating_mul(size_of::<f64>())),
        );
        if budget.exhausted(usage) {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        let mut branch_points = Vec::with_capacity(branches.len());
        let mut weights = Vec::with_capacity(branches.len());

        for branch in branches {
            let (execution_branch, weight, rank_limit) = match branch.kind() {
                QueryKind::Weighted {
                    query: weighted_child,
                    weight,
                } => (weighted_child.as_ref(), *weight, branch.limit()),
                _ => (branch, 1.0, branch.limit()),
            };
            let Some(remaining) = budget.remaining(usage, execution_branch.has_filter_in_subtree())
            else {
                return Ok(ExecutionOutcome::new(
                    ExecutionState::Ready,
                    Completion::BudgetExhausted,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            };
            let child = self.execute_node(execution_branch, remaining, deadline)?;
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
            let ranked_points = &child.points()[..child.points().len().min(rank_limit)];
            let clone_bytes = hydrated_allocation_bytes(ranked_points);
            let clone_hydration = ranked_points
                .iter()
                .map(|row| row.source_key().as_str().len())
                .sum::<usize>();
            usage.add_memory_bytes(clone_bytes);
            usage.add_hydration_bytes(clone_hydration);
            if budget.exhausted(usage) {
                return Ok(ExecutionOutcome::new(
                    ExecutionState::Ready,
                    Completion::BudgetExhausted,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            branch_points.push((execution_branch.score_order(), ranked_points.to_vec()));
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
        let input_count: usize = branch_points.iter().map(|(_, points)| points.len()).sum();
        if usage
            .comparisons()
            .checked_add(input_count)
            .is_none_or(|total| total > budget.max_comparisons())
        {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        let weighted = matches!(fusion, Fusion::WeightedRrf { .. });
        let Some(projection) = fusion_projection(&branch_points, query.limit(), weighted) else {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        };
        if usage
            .comparisons()
            .checked_add(projection.work)
            .is_none_or(|total| total > budget.max_comparisons())
            || usage
                .memory_bytes()
                .checked_add(projection.memory_bytes)
                .is_none_or(|total| total > budget.max_memory_bytes())
            || usage
                .hydration_bytes()
                .checked_add(projection.hydration_bytes)
                .is_none_or(|total| total > budget.max_hydration_bytes())
        {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        usage.add_memory_bytes(projection.memory_bytes);
        usage.add_hydration_bytes(projection.hydration_bytes);
        if budget.exhausted(usage) {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        let points = match fusion {
            Fusion::Rrf { rank_constant } => fuse_points(
                &branch_points,
                &weights,
                rank_constant,
                query.limit(),
                false,
            )?,
            Fusion::WeightedRrf { rank_constant } => {
                fuse_points(&branch_points, &weights, rank_constant, query.limit(), true)?
            }
        };
        usage.add_comparisons(projection.work);
        usage.add_stage();
        let diagnostic = StageDiagnostic::new(
            StageKind::Fusion,
            match fusion {
                Fusion::Rrf { .. } => "reciprocal_rank_fusion",
                Fusion::WeightedRrf { .. } => "weighted_reciprocal_rank_fusion",
            },
            input_count,
            points.len(),
            None,
        );
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);
        if budget.exhausted(usage) {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
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
        comparison_count: usize,
        mut transform: impl FnMut(f64) -> Result<f64>,
    ) -> Result<ExecutionOutcome> {
        if child.state() != &ExecutionState::Ready || child.completion() == Completion::Cancelled {
            return Ok(child);
        }
        let usage = preflight_row_clone(&child);
        if budget.exhausted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        if usage
            .comparisons()
            .checked_add(comparison_count)
            .is_none_or(|total| total > budget.max_comparisons())
        {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let input_count = child.points().len();
        let mut rows = Vec::with_capacity(input_count);
        for point in child.points() {
            rows.push(
                HydratedCandidate::new(
                    point.point_id(),
                    point.source_key().clone(),
                    transform(point.score())?,
                )?
                .with_contributions(point.contributions().to_vec()),
            );
        }
        self.finish_transform(
            query,
            child,
            budget,
            rows,
            StageKind::ScoreTransform,
            strategy,
            usage,
            comparison_count,
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
        let usage = preflight_row_clone(&child);
        if budget.exhausted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let bounds_per_point = usize::from(minimum.is_some()) + usize::from(maximum.is_some());
        let comparison_count = child.points().len().checked_mul(bounds_per_point).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "score_threshold_comparison_projection",
            },
        )?;
        if usage
            .comparisons()
            .checked_add(comparison_count)
            .is_none_or(|total| total > budget.max_comparisons())
        {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
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
            usage,
            comparison_count,
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
        let usage = preflight_row_clone(&child);
        if budget.exhausted(usage) {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let comparison_count = child.points().len();
        if usage
            .comparisons()
            .checked_add(comparison_count)
            .is_none_or(|total| total > budget.max_comparisons())
        {
            return Ok(stopped_child(&child, Completion::BudgetExhausted, usage));
        }
        let rows = child.points().to_vec();
        self.finish_transform(
            query,
            child,
            budget,
            rows,
            StageKind::Rerank,
            "final_score_order",
            usage,
            comparison_count,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the transform finalizer receives the validated stage identity and precharged execution state"
    )]
    fn finish_transform(
        &mut self,
        query: &QueryIr,
        child: ExecutionOutcome,
        budget: ExecutionBudget,
        rows: Vec<HydratedCandidate>,
        stage: StageKind,
        strategy: &'static str,
        mut usage: BudgetUsage,
        comparison_count: usize,
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
        usage.add_comparisons(comparison_count);
        usage.add_stage();
        let diagnostic = StageDiagnostic::new(stage, strategy, input_count, points.len(), None);
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);
        if budget.exhausted(usage) {
            return Ok(ExecutionOutcome::new(
                ExecutionState::Ready,
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
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

fn preflight_row_clone(child: &ExecutionOutcome) -> BudgetUsage {
    let mut usage = child.usage();
    let hydration_bytes = child
        .points()
        .iter()
        .map(|row| row.source_key().as_str().len())
        .sum::<usize>();
    usage.add_memory_bytes(hydrated_allocation_bytes(child.points()));
    usage.add_hydration_bytes(hydration_bytes);
    usage
}

struct FusionProjection {
    memory_bytes: usize,
    hydration_bytes: usize,
    work: usize,
}

struct FusionMetadata {
    source_key: SourceKey,
    contributions: Vec<crate::BranchContribution>,
}

fn checked_add_product(total: &mut usize, count: usize, size: usize) -> Option<()> {
    *total = total.checked_add(count.checked_mul(size)?)?;
    Some(())
}

fn fusion_projection(
    branches: &[(ScoreOrder, Vec<HydratedCandidate>)],
    limit: usize,
    weighted: bool,
) -> Option<FusionProjection> {
    let input_count = branches.iter().try_fold(0_usize, |total, (_, points)| {
        total.checked_add(points.len())
    })?;
    let contribution_count = branches.iter().try_fold(0_usize, |total, (_, points)| {
        points.iter().try_fold(total, |total, point| {
            total.checked_add(point.contributions().len())
        })
    })?;
    let source_key_bytes = branches.iter().try_fold(0_usize, |total, (_, points)| {
        points.iter().try_fold(total, |total, point| {
            total.checked_add(point.source_key().as_str().len())
        })
    })?;
    let output_count = input_count.min(limit);
    let mut memory_bytes = 0_usize;

    // Ranked branch vectors and their outer allocation.
    checked_add_product(
        &mut memory_bytes,
        branches.len(),
        size_of::<Vec<RankedPoint>>(),
    )?;
    checked_add_product(&mut memory_bytes, input_count, size_of::<RankedPoint>())?;
    checked_add_product(
        &mut memory_bytes,
        branches.len(),
        if weighted {
            size_of::<WeightedRankedBranch<'static>>()
        } else {
            size_of::<&'static [RankedPoint]>()
        },
    )?;

    // The hybrid implementation retains a score map, per-branch seen sets,
    // and a fused output vector. Three machine words conservatively cover a
    // B-tree node's links/metadata in addition to each key/value pair.
    let tree_overhead = size_of::<usize>().checked_mul(3)?;
    checked_add_product(
        &mut memory_bytes,
        input_count,
        size_of::<u64>()
            .checked_add(size_of::<f64>())?
            .checked_add(tree_overhead)?,
    )?;
    checked_add_product(
        &mut memory_bytes,
        input_count,
        size_of::<u64>().checked_add(tree_overhead)?,
    )?;
    checked_add_product(
        &mut memory_bytes,
        input_count,
        size_of::<context_hybrid::FusedPoint>(),
    )?;

    // Metadata is built once and moved into final rows. Account every input
    // key/contribution so duplicate point IDs cannot hide maximal overlap.
    checked_add_product(
        &mut memory_bytes,
        input_count,
        size_of::<PointId>()
            .checked_add(size_of::<FusionMetadata>())?
            .checked_add(tree_overhead)?,
    )?;
    memory_bytes = memory_bytes.checked_add(source_key_bytes)?;
    checked_add_product(
        &mut memory_bytes,
        contribution_count,
        size_of::<crate::BranchContribution>().checked_mul(4)?,
    )?;
    checked_add_product(
        &mut memory_bytes,
        output_count,
        size_of::<HydratedCandidate>(),
    )?;

    let work = input_count
        .checked_mul(2)?
        .checked_add(contribution_count)?
        .checked_add(output_count)?;
    Some(FusionProjection {
        memory_bytes,
        hydration_bytes: source_key_bytes,
        work,
    })
}

fn fuse_points(
    branches: &[(ScoreOrder, Vec<HydratedCandidate>)],
    weights: &[f64],
    rank_constant: u32,
    limit: usize,
    normalize_weights: bool,
) -> Result<Vec<HydratedCandidate>> {
    let k = RrfK::new(rank_constant).ok_or(QueryError::InvalidInput {
        field: "rank_constant",
        reason: "must be positive".to_owned(),
    })?;
    let total_weight = weights.iter().copied().sum::<f64>();
    let mut ranked = Vec::with_capacity(branches.len());
    let mut metadata = BTreeMap::<PointId, FusionMetadata>::new();
    for ((_, branch), weight) in branches.iter().zip(weights) {
        let normalized_weight = if normalize_weights {
            *weight / total_weight
        } else {
            1.0
        };
        let mut ranked_branch = Vec::with_capacity(branch.len());
        for (rank, point) in branch.iter().enumerate() {
            ranked_branch.push(RankedPoint::new(point.point_id().get()));
            let rank = u32::try_from(rank.saturating_add(1)).map_err(|_| {
                QueryError::ArithmeticOverflow {
                    operation: "fusion_rank",
                }
            })?;
            let fusion_contribution = normalized_weight / (f64::from(k.get()) + f64::from(rank));
            let entry = match metadata.entry(point.point_id()) {
                Entry::Vacant(entry) => entry.insert(FusionMetadata {
                    source_key: point.source_key().clone(),
                    contributions: Vec::new(),
                }),
                Entry::Occupied(entry) => {
                    if entry.get().source_key != *point.source_key() {
                        return Err(QueryError::PortFailure {
                            stage: "fusion",
                            message: format!(
                                "point ID {} resolved to inconsistent source keys across branches",
                                point.point_id().get()
                            ),
                        });
                    }
                    entry.into_mut()
                }
            };
            entry
                .contributions
                .extend(point.contributions().iter().copied().enumerate().map(
                    |(occurrence, contribution)| {
                        contribution.with_fusion_contribution(if occurrence == 0 {
                            fusion_contribution
                        } else {
                            0.0
                        })
                    },
                ));
        }
        ranked.push(ranked_branch);
    }
    let fused = if normalize_weights {
        let weighted = ranked
            .iter()
            .zip(weights)
            .map(|(points, weight)| WeightedRankedBranch::new(points, *weight))
            .collect::<Vec<_>>();
        weighted_reciprocal_rank_fusion(&weighted, k, limit).map_err(|error| {
            QueryError::InvalidInput {
                field: "weight",
                reason: format!("weighted RRF failed: {error:?}"),
            }
        })?
    } else {
        let ranked_refs = ranked.iter().map(Vec::as_slice).collect::<Vec<_>>();
        reciprocal_rank_fusion(&ranked_refs, k, limit)
    };
    fused
        .into_iter()
        .map(|point| {
            let point_id = PointId::new(point.point_id());
            let metadata = metadata
                .remove(&point_id)
                .ok_or(QueryError::UnexpectedPointId {
                    stage: "fusion",
                    point_id,
                })?;
            HydratedCandidate::new(point_id, metadata.source_key, point.score())
                .map(|row| row.with_contributions(metadata.contributions))
        })
        .collect()
}
