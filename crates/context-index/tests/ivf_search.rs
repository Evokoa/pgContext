//! Pure IVFFlat construction, probing, widening, and budget contracts.

use context_core::DistanceMetric;
use context_index::{
    InMemoryIvfIndex, IvfCandidateBudget, IvfConfig, IvfError, IvfIterativePolicy, IvfListId,
    IvfPointId, IvfPosting, IvfProbeBudget, IvfProbeWindow, NeverCancelIvf, search_ivf,
    search_ivf_probe_window,
};
use proptest::prelude::*;
use std::error::Error;

fn point(id: u64, values: &[f32]) -> Result<IvfPosting, Box<dyn Error>> {
    let point_id = IvfPointId::new(id).ok_or("test point id is zero")?;
    Ok(IvfPosting::new(point_id, values.to_vec())?)
}

fn fixture() -> Result<InMemoryIvfIndex, Box<dyn Error>> {
    Ok(InMemoryIvfIndex::new(
        DistanceMetric::L2,
        vec![vec![0.0, 0.0], vec![10.0, 10.0], vec![20.0, 20.0]],
        vec![
            vec![point(2, &[0.2, 0.0])?, point(1, &[0.1, 0.0])?],
            vec![point(3, &[10.0, 10.0])?],
            vec![point(4, &[20.0, 20.0])?],
        ],
    )?)
}

#[test]
fn config_rejects_unbounded_or_inconsistent_values() {
    assert!(matches!(
        IvfConfig::new(0, 1, 1, 1, 1, 7, IvfIterativePolicy::StrictOrder),
        Err(IvfError::InvalidConfig {
            parameter: "lists",
            ..
        })
    ));
    assert!(matches!(
        IvfConfig::new(4, 3, 2, 4, 1, 7, IvfIterativePolicy::StrictOrder),
        Err(IvfError::InvalidConfig {
            parameter: "probes",
            ..
        })
    ));
    assert!(matches!(
        IvfConfig::new(4, 1, 5, 4, 1, 7, IvfIterativePolicy::StrictOrder),
        Err(IvfError::InvalidConfig {
            parameter: "max_probes",
            ..
        })
    ));
    assert!(matches!(
        IvfConfig::new(4, 1, 2, 3, 1, 7, IvfIterativePolicy::StrictOrder),
        Err(IvfError::InvalidConfig {
            parameter: "training_sample_size",
            ..
        })
    ));
}

#[test]
fn generation_rejects_dimensions_above_the_canonical_policy() -> Result<(), Box<dyn Error>> {
    let dimensions = context_core::policy::MAX_VECTOR_DIMENSIONS + 1;
    let result = InMemoryIvfIndex::new(
        DistanceMetric::L2,
        vec![vec![0.0; dimensions]],
        vec![Vec::new()],
    );

    assert!(matches!(
        result,
        Err(IvfError::DimensionMismatch { actual, .. }) if actual == dimensions
    ));
    Ok(())
}

#[test]
fn centroid_order_and_assignment_use_lower_list_id_for_ties() -> Result<(), Box<dyn Error>> {
    let index = InMemoryIvfIndex::new(
        DistanceMetric::L2,
        vec![vec![0.0], vec![2.0]],
        vec![Vec::new(), Vec::new()],
    )?;

    assert_eq!(index.assign(&[1.0])?, IvfListId::new(0));
    assert_eq!(
        index.probe_order(&[1.0])?,
        vec![IvfListId::new(0), IvfListId::new(1)]
    );
    Ok(())
}

#[test]
fn strict_search_visits_bounded_closest_lists_and_orders_score_then_id()
-> Result<(), Box<dyn Error>> {
    let config = IvfConfig::new(3, 1, 2, 3, 8, 7, IvfIterativePolicy::StrictOrder)?;
    let outcome = search_ivf(
        &fixture()?,
        &[0.0, 0.0],
        &config,
        IvfProbeBudget::new(1).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(8).ok_or("candidate budget is zero")?,
        2,
        &|_| true,
        &NeverCancelIvf,
    )?;

    assert_eq!(outcome.visited_lists(), 1);
    assert_eq!(outcome.visited_postings(), 2);
    assert_eq!(
        outcome
            .hits()
            .iter()
            .map(|hit| hit.point_id().get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    Ok(())
}

#[test]
fn strict_widening_refills_after_mask_and_reports_completion() -> Result<(), Box<dyn Error>> {
    let config = IvfConfig::new(3, 1, 3, 3, 8, 7, IvfIterativePolicy::StrictOrder)?;
    let outcome = search_ivf(
        &fixture()?,
        &[0.0, 0.0],
        &config,
        IvfProbeBudget::new(3).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(8).ok_or("candidate budget is zero")?,
        2,
        &|point_id: IvfPointId| point_id.get() >= 3,
        &NeverCancelIvf,
    )?;

    assert_eq!(outcome.visited_lists(), 3);
    assert_eq!(outcome.widening_rounds(), 2);
    assert_eq!(
        outcome
            .hits()
            .iter()
            .map(|hit| hit.point_id().get())
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(outcome.completion().is_complete());
    Ok(())
}

#[test]
fn candidate_budget_and_cancellation_fail_with_typed_errors() -> Result<(), Box<dyn Error>> {
    let config = IvfConfig::new(3, 1, 3, 3, 8, 7, IvfIterativePolicy::StrictOrder)?;
    let budget_error = search_ivf(
        &fixture()?,
        &[0.0, 0.0],
        &config,
        IvfProbeBudget::new(3).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(1).ok_or("candidate budget is zero")?,
        2,
        &|_| true,
        &NeverCancelIvf,
    );
    assert!(matches!(
        budget_error,
        Err(IvfError::CandidateBudgetExhausted { .. })
    ));

    let cancelled = search_ivf(
        &fixture()?,
        &[0.0, 0.0],
        &config,
        IvfProbeBudget::new(3).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(8).ok_or("candidate budget is zero")?,
        2,
        &|_| true,
        &|| true,
    );
    assert!(matches!(cancelled, Err(IvfError::Cancelled)));

    let exact_boundary = search_ivf(
        &fixture()?,
        &[0.0, 0.0],
        &config,
        IvfProbeBudget::new(1).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(2).ok_or("candidate budget is zero")?,
        2,
        &|_| true,
        &NeverCancelIvf,
    )?;
    assert_eq!(exact_boundary.visited_postings(), 2);
    assert_eq!(exact_boundary.hits().len(), 2);
    Ok(())
}

#[test]
fn probe_windows_visit_only_new_lists_and_charge_prior_work() -> Result<(), Box<dyn Error>> {
    let index = fixture()?;
    let config = IvfConfig::new(3, 1, 3, 3, 8, 7, IvfIterativePolicy::RelaxedOrder)?;
    let window = IvfProbeWindow::new(1, 3).ok_or("probe window is invalid")?;
    let outcome = search_ivf_probe_window(
        &index,
        &[0.0, 0.0],
        &config,
        window,
        IvfCandidateBudget::new(4).ok_or("candidate budget is zero")?,
        2,
        2,
        &|_| true,
        &NeverCancelIvf,
    )?;
    assert_eq!(outcome.visited_lists(), 2);
    assert_eq!(outcome.visited_postings(), 2);
    assert_eq!(
        outcome
            .hits()
            .iter()
            .map(|hit| hit.point_id().get())
            .collect::<Vec<_>>(),
        vec![3, 4]
    );

    let exhausted = search_ivf_probe_window(
        &index,
        &[0.0, 0.0],
        &config,
        window,
        IvfCandidateBudget::new(3).ok_or("candidate budget is zero")?,
        2,
        2,
        &|_| true,
        &NeverCancelIvf,
    );
    assert!(matches!(
        exhausted,
        Err(IvfError::CandidateBudgetExhausted {
            budget: 3,
            visited: 4
        })
    ));
    Ok(())
}

proptest! {
    #[test]
    fn assigned_centroid_is_never_farther_than_an_alternative(
        query in prop::collection::vec(-100.0_f32..100.0, 2),
        left in prop::collection::vec(-100.0_f32..100.0, 2),
        right in prop::collection::vec(-100.0_f32..100.0, 2),
    ) {
        let index = InMemoryIvfIndex::new(
            DistanceMetric::L2,
            vec![left.clone(), right.clone()],
            vec![Vec::new(), Vec::new()],
        ).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let assigned = index.assign(&query)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let assigned_values = if assigned == IvfListId::new(0) { &left } else { &right };
        let alternative = if assigned == IvfListId::new(0) { &right } else { &left };
        let assigned_distance = DistanceMetric::L2.distance_slices(&query, assigned_values)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let alternative_distance = DistanceMetric::L2.distance_slices(&query, alternative)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert!(assigned_distance <= alternative_distance);
    }
}
