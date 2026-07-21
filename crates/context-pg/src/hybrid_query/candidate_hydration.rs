//! Candidate hydration adapters for hybrid retrieval branches.

use std::{collections::BTreeMap, error::Error, fmt};

use context_hybrid::{BranchCandidate, CandidateBatch, CandidateBranch};

/// One raw candidate row emitted by a PostgreSQL branch adapter.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HydratedCandidate {
    point_id: i64,
    source_key: String,
    branch_score: Option<f64>,
}

impl HydratedCandidate {
    pub(super) fn with_score(point_id: i64, source_key: String, branch_score: f64) -> Self {
        Self {
            point_id,
            source_key,
            branch_score: Some(branch_score),
        }
    }

    fn without_score(point_id: i64, source_key: String) -> Self {
        Self {
            point_id,
            source_key,
            branch_score: None,
        }
    }
}

/// A fully hydrated branch batch ready for reciprocal-rank fusion.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HydratedBranch {
    pub(super) candidates: CandidateBatch,
    pub(super) source_keys: BTreeMap<u64, String>,
}

/// Hydration failures before candidates cross into `context-hybrid`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum HydrationError {
    NegativePointId {
        context: &'static str,
        point_id: i64,
    },
    NonFiniteScore {
        context: &'static str,
        point_id: i64,
        score: f64,
    },
    MissingScore {
        context: &'static str,
        point_id: i64,
    },
    ConflictingSourceKey {
        context: &'static str,
        point_id: u64,
        first: String,
        second: String,
    },
}

impl fmt::Display for HydrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativePointId { context, point_id } => {
                write!(f, "{context} point_id is negative: {point_id}")
            }
            Self::NonFiniteScore {
                context,
                point_id,
                score,
            } => write!(
                f,
                "{context} branch_score is not finite for point_id {point_id}: {score}"
            ),
            Self::MissingScore { context, point_id } => {
                write!(
                    f,
                    "{context} branch_score is missing for point_id {point_id}"
                )
            }
            Self::ConflictingSourceKey {
                context,
                point_id,
                first,
                second,
            } => write!(
                f,
                "{context} has conflicting source_key values for point_id {point_id}: {first} vs {second}"
            ),
        }
    }
}

impl Error for HydrationError {}

pub(super) fn hydrate_dense_exact_candidates(
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    hydrate_scored_candidates(CandidateBranch::DenseExact, candidates, context)
}

#[allow(dead_code)]
pub(super) fn hydrate_dense_ann_candidates(
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    hydrate_scored_candidates(CandidateBranch::DenseAnn, candidates, context)
}

pub(super) fn hydrate_full_text_candidates(
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    hydrate_scored_candidates(CandidateBranch::FullText, candidates, context)
}

#[allow(dead_code)]
pub(super) fn hydrate_sparse_planned_candidates(
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    hydrate_scored_candidates(CandidateBranch::SparsePlanned, candidates, context)
}

#[allow(dead_code)]
pub(super) fn hydrate_user_provided_candidates(
    point_ids: Vec<i64>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    let candidates = point_ids
        .into_iter()
        .map(|point_id| HydratedCandidate::without_score(point_id, point_id.to_string()))
        .collect();
    hydrate_candidates(CandidateBranch::UserProvided, candidates, context, false)
}

fn hydrate_scored_candidates(
    branch: CandidateBranch,
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
) -> Result<HydratedBranch, HydrationError> {
    hydrate_candidates(branch, candidates, context, true)
}

fn hydrate_candidates(
    branch: CandidateBranch,
    candidates: Vec<HydratedCandidate>,
    context: &'static str,
    require_score: bool,
) -> Result<HydratedBranch, HydrationError> {
    let mut branch_candidates = Vec::with_capacity(candidates.len());
    let mut source_keys = BTreeMap::new();

    for candidate in candidates {
        let point_id =
            u64::try_from(candidate.point_id).map_err(|_| HydrationError::NegativePointId {
                context,
                point_id: candidate.point_id,
            })?;
        insert_source_key(&mut source_keys, context, point_id, candidate.source_key)?;

        let branch_candidate = match candidate.branch_score {
            Some(score) if score.is_finite() => BranchCandidate::with_score(point_id, score),
            Some(score) => {
                return Err(HydrationError::NonFiniteScore {
                    context,
                    point_id: candidate.point_id,
                    score,
                });
            }
            None if require_score => {
                return Err(HydrationError::MissingScore {
                    context,
                    point_id: candidate.point_id,
                });
            }
            None => BranchCandidate::new(point_id),
        };
        branch_candidates.push(branch_candidate);
    }

    Ok(HydratedBranch {
        candidates: CandidateBatch::from_candidates(branch, branch_candidates),
        source_keys,
    })
}

fn insert_source_key(
    source_keys: &mut BTreeMap<u64, String>,
    context: &'static str,
    point_id: u64,
    source_key: String,
) -> Result<(), HydrationError> {
    match source_keys.get(&point_id) {
        Some(first) if first != &source_key => Err(HydrationError::ConflictingSourceKey {
            context,
            point_id,
            first: first.clone(),
            second: source_key,
        }),
        Some(_) => Ok(()),
        None => {
            source_keys.insert(point_id, source_key);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HydratedCandidate, HydrationError, hydrate_dense_ann_candidates,
        hydrate_dense_exact_candidates, hydrate_full_text_candidates,
        hydrate_sparse_planned_candidates, hydrate_user_provided_candidates,
    };
    use context_hybrid::CandidateBranch;

    #[test]
    fn hydrates_all_branch_shapes_to_candidate_batches() -> Result<(), Box<dyn std::error::Error>> {
        let dense = hydrate_dense_exact_candidates(scored_rows(), "dense exact")?;
        let ann = hydrate_dense_ann_candidates(scored_rows(), "dense ann")?;
        let full_text = hydrate_full_text_candidates(scored_rows(), "full text")?;
        let sparse = hydrate_sparse_planned_candidates(scored_rows(), "sparse planned")?;
        let user = hydrate_user_provided_candidates(vec![7, 9], "user batch")?;

        assert_eq!(dense.candidates.branch(), CandidateBranch::DenseExact);
        assert_eq!(ann.candidates.branch(), CandidateBranch::DenseAnn);
        assert_eq!(full_text.candidates.branch(), CandidateBranch::FullText);
        assert_eq!(sparse.candidates.branch(), CandidateBranch::SparsePlanned);
        assert_eq!(user.candidates.branch(), CandidateBranch::UserProvided);
        assert_eq!(dense.candidates.points()[0].point_id(), 7);
        assert_eq!(dense.source_keys.get(&9), Some(&"beta".to_owned()));
        assert_eq!(user.source_keys.get(&7), Some(&"7".to_owned()));

        Ok(())
    }

    #[test]
    fn hydration_rejects_negative_point_ids() {
        let result = hydrate_dense_exact_candidates(
            vec![HydratedCandidate::with_score(-1, "bad".to_owned(), 0.1)],
            "dense exact",
        );

        assert_eq!(
            result,
            Err(HydrationError::NegativePointId {
                context: "dense exact",
                point_id: -1,
            })
        );
    }

    #[test]
    fn hydration_rejects_non_finite_scores() {
        let result = hydrate_full_text_candidates(
            vec![HydratedCandidate::with_score(
                7,
                "alpha".to_owned(),
                f64::NAN,
            )],
            "full text",
        );

        assert!(matches!(
            result,
            Err(HydrationError::NonFiniteScore {
                context: "full text",
                point_id: 7,
                score,
            }) if score.is_nan()
        ));
    }

    #[test]
    fn hydration_rejects_missing_scores_for_scored_branches() {
        let result = hydrate_sparse_planned_candidates(
            vec![HydratedCandidate::without_score(7, "alpha".to_owned())],
            "sparse planned",
        );

        assert_eq!(
            result,
            Err(HydrationError::MissingScore {
                context: "sparse planned",
                point_id: 7,
            })
        );
    }

    #[test]
    fn hydration_rejects_conflicting_source_keys_for_same_point() {
        let result = hydrate_dense_ann_candidates(
            vec![
                HydratedCandidate::with_score(7, "alpha".to_owned(), 0.1),
                HydratedCandidate::with_score(7, "beta".to_owned(), 0.2),
            ],
            "dense ann",
        );

        assert_eq!(
            result,
            Err(HydrationError::ConflictingSourceKey {
                context: "dense ann",
                point_id: 7,
                first: "alpha".to_owned(),
                second: "beta".to_owned(),
            })
        );
    }

    fn scored_rows() -> Vec<HydratedCandidate> {
        vec![
            HydratedCandidate::with_score(7, "alpha".to_owned(), 0.1),
            HydratedCandidate::with_score(9, "beta".to_owned(), 0.2),
        ]
    }
}
