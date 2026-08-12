//! Bridge from a [`RerankBackend`] to the executor's reranking port.
//!
//! The executor already owns a reranking port — [`ExternalReranker`] — and
//! there must be exactly one. This module adapts a transport-level backend to
//! it rather than introducing a parallel path: the backend knows how to talk to
//! a provider, and the adapter knows how the executor's budget, ordering, and
//! authority rules apply to what comes back.
//!
//! Authorization is the caller's job, not the provider's. A hydrated row carries
//! no text and no version, so releasing anything requires an
//! [`AuthorizedRowSource`] that reads the authoritative row and decides what may
//! leave the database. The adapter calls that source again after provider
//! scoring and compares the complete authorized snapshot; a source-version,
//! text, metadata, identity, deletion, ACL, RLS, or filter change therefore
//! fails closed before any reranked row is returned.
//!
//! # The score a reranked row carries
//!
//! The executor re-sorts a completed page by [`HydratedCandidate::score`] and
//! stages above this one — a score threshold, a weight, a formula — read the
//! same value. So the row carries the *provider's own score*, not its rank: a
//! rank ordinal would order correctly and then make every score-reading stage
//! above it meaningless. After an external rerank the score is the reranker's
//! relevance, which is what a threshold over a reranked query should filter on.
//!
//! # Reranking is all or nothing
//!
//! A row can miss the provider because the comparison budget cut it or the
//! provider returned no score for it. Authority withholding is not a partial
//! result policy and fails closed before dispatch. The
//! executor requires a completed page to hold exactly the rows it asked for,
//! and no honest score exists for a row nobody ranked — so when fewer than
//! `limit` rows come back scored, the whole page is reported as
//! *non-authoritative* rather than padded with invented scores.
//!
//! A row that was never ranked therefore does not appear in a complete answer.
//! That is the point: it was excluded by the deployment's own authorization or
//! budget, not quietly reordered. A provider cannot exploit it either, since
//! omitting a score is worth no more to it than returning the lowest one — and
//! omitting enough scores degrades the query instead of trimming the answer.

use core::mem::size_of;

use context_core::{OccurrenceId, PointId};
use context_query::{
    Cancellation, ExternalRerankPage, ExternalReranker, HydratedCandidate,
    MAX_RERANK_REQUEST_BYTES, PortBudget, QueryClock, QueryError, QueryIr, QueryKind,
    RerankCandidate, RerankFallbackPolicy, RerankModelName, RerankQuery, RerankRequest,
    RerankRequestId, RerankScore, ScoreOrder,
};

use crate::{
    MAX_RERANK_BATCH, RerankBackend, RerankBackendError, RerankBatches, RerankOutcome, score_all,
};

/// Supplies the authorized snapshot for hydrated rows.
///
/// The implementation is the component with database authority: it re-reads the
/// row, applies whatever the deployment considers releasable, and stamps the
/// occurrence identity and source version the response is validated against.
/// It is called once before provider release and again before finalization, so
/// it must evaluate current deletion, filter, ACL, and RLS state on every call.
/// Returning `None` withholds the row from the provider and fails the attempt
/// closed. Provider fallback never applies to authority or source drift.
pub trait AuthorizedRowSource {
    /// Returns what may be released for `row`, or `None` to withhold it.
    ///
    /// `max_candidate_bytes` is a hard pre-allocation allowance. Database
    /// implementations must check bounded column lengths and aggregate shape
    /// before materializing authorized text or metadata. The adapter verifies
    /// the returned candidate's conservative projection as a second boundary;
    /// exceeding the allowance is a port-contract failure.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral failure when the authoritative row cannot be
    /// read.
    fn authorize(
        &mut self,
        row: &HydratedCandidate,
        max_candidate_bytes: usize,
    ) -> context_query::Result<Option<RerankCandidate>>;
}

/// Immutable query/model policy carried by a [`BackendReranker`].
#[derive(Clone, Debug)]
pub struct BackendRerankerConfig {
    model: RerankModelName,
    query_text: RerankQuery,
    policy: RerankFallbackPolicy,
    first_request_id: RerankRequestId,
}

impl BackendRerankerConfig {
    /// Creates an already-validated adapter configuration.
    #[must_use]
    pub const fn new(
        model: RerankModelName,
        query_text: RerankQuery,
        policy: RerankFallbackPolicy,
        first_request_id: RerankRequestId,
    ) -> Self {
        Self {
            model,
            query_text,
            policy,
            first_request_id,
        }
    }
}

/// Adapts a [`RerankBackend`] to the executor's [`ExternalReranker`] port.
pub struct BackendReranker<'a, B, S, C, K> {
    backend: &'a B,
    rows: &'a mut S,
    cancellation: &'a C,
    clock: &'a K,
    model: RerankModelName,
    query_text: RerankQuery,
    policy: RerankFallbackPolicy,
    batch_size: usize,
    next_request_id: u64,
}

impl<'a, B, S, C, K> BackendReranker<'a, B, S, C, K>
where
    B: RerankBackend,
    S: AuthorizedRowSource,
    C: Cancellation,
    K: QueryClock,
{
    /// Creates the adapter.
    ///
    /// `first_request_id` seeds the request identities this adapter derives; it
    /// is advanced past every batch it issues, so no two calls through the same
    /// adapter can be confused for one another.
    pub fn new(
        backend: &'a B,
        rows: &'a mut S,
        cancellation: &'a C,
        clock: &'a K,
        config: BackendRerankerConfig,
    ) -> Self {
        Self {
            backend,
            rows,
            cancellation,
            clock,
            model: config.model,
            query_text: config.query_text,
            policy: config.policy,
            batch_size: MAX_RERANK_BATCH,
            next_request_id: config.first_request_id.get(),
        }
    }

    /// Sets the number of candidates sent to the backend per call.
    ///
    /// # Errors
    ///
    /// Returns [`RerankBackendError::InvalidPlan`] when `batch_size` is outside
    /// `1..=`[`MAX_RERANK_BATCH`]. It is rejected at configuration time so a
    /// misconfiguration cannot surface as a failure mid-query.
    pub fn with_batch_size(mut self, batch_size: usize) -> crate::BackendResult<Self> {
        if batch_size == 0 || batch_size > MAX_RERANK_BATCH {
            return Err(RerankBackendError::InvalidPlan {
                reason: format!("rerank batch size must be 1..={MAX_RERANK_BATCH}"),
            });
        }
        self.batch_size = batch_size;
        Ok(self)
    }
}

impl<B, S, C, K> ExternalReranker for BackendReranker<'_, B, S, C, K>
where
    B: RerankBackend,
    S: AuthorizedRowSource,
    C: Cancellation,
    K: QueryClock,
{
    fn rerank(
        &mut self,
        query: &QueryIr,
        rows: &[HydratedCandidate],
        limit: usize,
        budget: PortBudget,
    ) -> context_query::Result<ExternalRerankPage> {
        let model_revision = self.backend.model_revision();
        if rows.is_empty() || limit == 0 {
            return Ok(ExternalRerankPage::new(Vec::new(), 0, true, model_revision));
        }
        // The plan names the revision it requires. The executor checks this too,
        // but only after the port returns — by then the authorized text has
        // already been released, so a backend serving the wrong weights has to
        // be caught before anything leaves the database.
        if let QueryKind::ExternalRerank {
            model_revision: required,
            ..
        } = query.kind()
            && *required != model_revision
        {
            return Err(QueryError::PortFailure {
                stage: "external_rerank",
                message: "backend serves a different model revision than the plan requires"
                    .to_owned(),
            });
        }

        // A provider ranks by descending relevance, and the executor sorts the
        // returned page by the query's ordering. Those only agree under
        // `HigherIsBetter` — which is what the IR pins this query kind to — so
        // any other ordering is refused rather than silently inverted.
        if query.score_order() != ScoreOrder::HigherIsBetter {
            return Err(QueryError::PortFailure {
                stage: "external_rerank",
                message: "external reranking requires higher-is-better ordering".to_owned(),
            });
        }

        // Choosing the top `limit` rows requires scoring the entire bounded
        // candidate set. Scoring only the fused prefix would make omitted rows
        // unable to win and would therefore preserve membership rather than
        // rerank it. Refuse before authorization when the comparison budget
        // cannot cover every supplied row.
        if budget.max_comparisons() < rows.len() {
            return Ok(unranked(model_revision, 0));
        }
        let first_pass_hydration = rows.iter().try_fold(0_usize, |total, row| {
            total.checked_add(row.source_key().as_str().len()).ok_or(
                QueryError::ArithmeticOverflow {
                    operation: "rerank_hydration_projection",
                },
            )
        })?;
        let winner_recheck_hydration = largest_output_key_bytes(rows, limit.min(rows.len()))?;
        let hydration_bytes = first_pass_hydration
            .checked_add(winner_recheck_hydration)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_hydration_projection",
            })?;
        if hydration_bytes > budget.max_hydration_bytes() {
            return Ok(unranked(model_revision, 0));
        }

        let fixed_memory = fixed_adapter_memory_bytes(rows, limit)?;
        if fixed_memory > budget.max_memory_bytes() {
            return Ok(unranked(model_revision, 0));
        }

        let mut candidates = Vec::with_capacity(rows.len());
        let mut identities = Vec::with_capacity(rows.len());
        let mut candidate_bytes = 0_usize;
        let mut largest_candidate_bytes = 0_usize;
        for row in rows {
            // `authorize` is a database read per row; without this the phase
            // before the first provider call is uninterruptible.
            self.cancellation.check_interrupt()?;
            let remaining_candidate_bytes = budget
                .max_memory_bytes()
                .checked_sub(fixed_memory)
                .and_then(|remaining| remaining.checked_sub(candidate_bytes))
                .unwrap_or(0);
            let Some(candidate) = self.rows.authorize(row, remaining_candidate_bytes)? else {
                return authority_changed("external_rerank_source_authorization");
            };
            if candidate.point_id() != row.point_id() {
                return Err(QueryError::PortFailure {
                    stage: "external_rerank",
                    message: "authorized row changed the requested point identity".to_owned(),
                });
            }
            let bytes = candidate.projected_bytes()?;
            if bytes > remaining_candidate_bytes {
                return Err(QueryError::PortFailure {
                    stage: "external_rerank_source_authorization",
                    message: "authorized row exceeded its memory allowance".to_owned(),
                });
            }
            candidate_bytes =
                candidate_bytes
                    .checked_add(bytes)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "rerank_candidate_memory_projection",
                    })?;
            largest_candidate_bytes = largest_candidate_bytes.max(bytes);
            identities.push((candidate.occurrence_id(), row.point_id()));
            identities.sort_unstable_by_key(|(occurrence, _)| *occurrence);
            if identities.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(QueryError::PortFailure {
                    stage: "external_rerank",
                    message: "authorized rows repeated an occurrence".to_owned(),
                });
            }
            candidates.push(candidate);
        }
        let projected_memory = projected_adapter_memory_bytes(
            fixed_memory,
            candidate_bytes,
            largest_candidate_bytes,
            rows.len(),
            self.batch_size,
            &self.model,
            &self.query_text,
        )?;
        if projected_memory > budget.max_memory_bytes() {
            return Ok(unranked(model_revision, 0));
        }

        // The provider compares everything released to it, whether or not it
        // answers for every row, so this is the work the query performed.
        let comparisons = candidates.len();
        let expires_at_micros = self
            .clock
            .now_micros()
            .checked_add(budget.remaining_elapsed_micros())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_deadline_projection",
            })?;
        let batches = RerankBatches::plan(
            RerankRequestId::new(self.next_request_id)?,
            &self.model,
            model_revision,
            expires_at_micros,
            &self.query_text,
            candidates,
            self.batch_size,
        )
        .map_err(port_failure)?;
        let issued = u64::try_from(batches.requests().len()).map_err(|_| {
            QueryError::ArithmeticOverflow {
                operation: "rerank_request_identity",
            }
        })?;
        self.next_request_id =
            self.next_request_id
                .checked_add(issued)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_request_identity",
                })?;

        let outcome = score_all(
            self.backend,
            &batches,
            self.cancellation,
            self.clock,
            self.policy,
        )
        .map_err(port_failure)?;
        let scores = match outcome {
            RerankOutcome::Reranked(scores) => scores,
            RerankOutcome::Degraded { .. } => {
                return Ok(unranked(model_revision, comparisons));
            }
        };

        // Only rows the provider actually judged can carry a reranked score.
        // A page short of `limit` cannot be completed without inventing a score
        // for a row nobody ranked, so it is reported as non-authoritative
        // instead. That is also what stops a provider from editing the answer:
        // omitting a score degrades the query, it never removes a row.
        if scores.len() < limit {
            return Ok(unranked(model_revision, comparisons));
        }

        let mut ranked = scores;
        ranked.sort_by(|left, right| {
            right
                .score()
                .total_cmp(&left.score())
                .then_with(|| left.occurrence_id().cmp(&right.occurrence_id()))
        });
        let mut selected = Vec::with_capacity(limit);
        for score in ranked.into_iter().take(limit) {
            let Some((_, point_id)) = identities
                .binary_search_by_key(&score.occurrence_id(), |(occurrence, _)| *occurrence)
                .ok()
                .and_then(|index| identities.get(index))
            else {
                // `score_all` already refuses an occurrence the request never
                // released, so this is unreachable through the validated path.
                // It is refused rather than skipped so no future path can
                // introduce a row the caller never hydrated.
                return Err(QueryError::PortFailure {
                    stage: "external_rerank",
                    message: "provider scored an unreleased occurrence".to_owned(),
                });
            };
            selected.push((*point_id, score.score()));
        }

        let mut page = Vec::with_capacity(limit);
        for (point_id, score) in selected {
            let Some(row) = rows.iter().find(|row| row.point_id() == point_id) else {
                return Err(QueryError::UnexpectedPointId {
                    stage: "external_rerank",
                    point_id,
                });
            };
            let Some(released) = batches
                .requests()
                .iter()
                .flat_map(|request| request.candidates())
                .find(|candidate| candidate.point_id() == point_id)
            else {
                return Err(QueryError::UnexpectedPointId {
                    stage: "external_rerank_source_recheck",
                    point_id,
                });
            };
            self.cancellation.check_interrupt()?;
            let Some(current) = self.rows.authorize(row, largest_candidate_bytes)? else {
                return authority_changed("external_rerank_source_recheck");
            };
            if current.projected_bytes()? > largest_candidate_bytes || current != *released {
                return authority_changed("external_rerank_source_recheck");
            }
            page.push(HydratedCandidate::new(
                point_id,
                row.source_key().clone(),
                score,
            )?);
        }
        Ok(ExternalRerankPage::new(
            page,
            comparisons,
            true,
            model_revision,
        ))
    }
}

fn fixed_adapter_memory_bytes(
    rows: &[HydratedCandidate],
    limit: usize,
) -> context_query::Result<usize> {
    let row_count = rows.len();
    let retained_slots = checked_product(
        row_count,
        size_of::<RerankCandidate>(),
        "rerank_memory_projection",
    )?;
    let identity_slots = checked_product(
        row_count,
        size_of::<(OccurrenceId, PointId)>()
            .checked_add(size_of::<OccurrenceId>())
            .and_then(|bytes| bytes.checked_add(size_of::<PointId>()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_memory_projection",
            })?,
        "rerank_memory_projection",
    )?;
    let score_slots = checked_product(
        row_count,
        size_of::<RerankScore>()
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(size_of::<(PointId, f64)>()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_memory_projection",
            })?,
        "rerank_memory_projection",
    )?;
    let output_slots = checked_product(
        limit.min(row_count),
        size_of::<HydratedCandidate>(),
        "rerank_memory_projection",
    )?;
    let output_keys = largest_output_key_bytes(rows, limit.min(row_count))?;
    retained_slots
        .checked_add(identity_slots)
        .and_then(|bytes| bytes.checked_add(score_slots))
        .and_then(|bytes| bytes.checked_add(output_slots))
        .and_then(|bytes| bytes.checked_add(output_keys))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "rerank_memory_projection",
        })
}

fn largest_output_key_bytes(
    rows: &[HydratedCandidate],
    output_count: usize,
) -> context_query::Result<usize> {
    let first_winner = rows.len().saturating_sub(output_count);
    rows.iter()
        .enumerate()
        .try_fold(0_usize, |total, (index, row)| {
            let key_bytes = row.source_key().as_str().len();
            let rank = rows
                .iter()
                .enumerate()
                .fold(0_usize, |rank, (other_index, other)| {
                    let other_bytes = other.source_key().as_str().len();
                    rank.saturating_add(usize::from(
                        other_bytes < key_bytes
                            || (other_bytes == key_bytes && other_index < index),
                    ))
                });
            if rank < first_winner {
                return Ok(total);
            }
            total
                .checked_add(key_bytes)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_memory_projection",
                })
        })
}

fn projected_adapter_memory_bytes(
    fixed_memory: usize,
    candidate_bytes: usize,
    largest_candidate_bytes: usize,
    row_count: usize,
    batch_size: usize,
    model: &RerankModelName,
    query: &RerankQuery,
) -> context_query::Result<usize> {
    let request_fixed = size_of::<RerankRequest>()
        .checked_add(model.as_str().len())
        .and_then(|bytes| bytes.checked_add(query.as_str().len()))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "rerank_memory_projection",
        })?;
    let payload_capacity =
        MAX_RERANK_REQUEST_BYTES
            .checked_sub(request_fixed)
            .ok_or(QueryError::InvalidInput {
                field: "rerank_request",
                reason: "fixed request fields exhaust the byte budget".to_owned(),
            })?;
    let count_batches = row_count.div_ceil(batch_size);
    let byte_batches = candidate_bytes.div_ceil(payload_capacity.max(1));
    // The greedy splitter can leave two adjacent batches less than half full,
    // but never three: otherwise the first and third could not both have caused
    // a byte-boundary split. Twice the aggregate byte quotient plus the count
    // quotient is therefore a conservative upper bound on retained requests.
    let batch_count = count_batches
        .checked_add(
            byte_batches
                .checked_mul(2)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_memory_projection",
                })?,
        )
        .map(|count| count.min(row_count))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "rerank_memory_projection",
        })?;
    let request_bytes = checked_product(batch_count, request_fixed, "rerank_memory_projection")?;
    fixed_memory
        .checked_add(candidate_bytes)
        // One current authoritative snapshot coexists with the released
        // request during the post-provider recheck.
        .and_then(|bytes| bytes.checked_add(largest_candidate_bytes))
        .and_then(|bytes| bytes.checked_add(request_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "rerank_memory_projection",
        })
}

fn checked_product(
    count: usize,
    bytes: usize,
    operation: &'static str,
) -> context_query::Result<usize> {
    count
        .checked_mul(bytes)
        .ok_or(QueryError::ArithmeticOverflow { operation })
}

/// Reports that no authoritative reranking happened.
///
/// The port has no channel for "usable but not reranked", so an un-exhausted
/// page is the honest answer: the executor surfaces it as a budget-exhausted
/// completion rather than presenting an un-reranked ordering as a reranked one.
/// Under [`RerankFallbackPolicy::DegradeWithoutRerank`] that is the difference
/// between a visibly incomplete answer and a hard error, not between two
/// orderings.
///
/// `comparisons` still reports the work released to the provider. The attempt
/// cost the same whether or not its answer was usable, and a degraded stage
/// that reported zero would understate what the query actually did.
fn unranked(model_revision: u64, comparisons: usize) -> ExternalRerankPage {
    ExternalRerankPage::new(Vec::new(), comparisons, false, model_revision)
}

fn authority_changed(stage: &'static str) -> context_query::Result<ExternalRerankPage> {
    Err(QueryError::PortFailure {
        stage,
        message: "authoritative rerank source changed before finalization".to_owned(),
    })
}

fn port_failure(error: RerankBackendError) -> QueryError {
    QueryError::PortFailure {
        stage: "external_rerank",
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    include!("adapter/tests.rs");
}
