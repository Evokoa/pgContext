//! Cross-crate deterministic IVFFlat training and search contract.

use context_build::deterministic_kmeans;
use context_core::DistanceMetric;
use context_index::{
    InMemoryIvfIndex, IvfCandidateBudget, IvfConfig, IvfIterativePolicy, IvfPointId, IvfPosting,
    IvfProbeBudget, NeverCancelIvf, search_ivf,
};

#[test]
fn deterministic_kmeans_builds_a_repeatable_searchable_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let vectors = vec![
        vec![0.0, 0.0],
        vec![0.1, 0.0],
        vec![10.0, 10.0],
        vec![10.1, 10.0],
    ];
    let trained = deterministic_kmeans(&vectors, 2, 16, 41)?;
    let mut lists = vec![Vec::new(), Vec::new()];
    for (ordinal, (vector, list)) in vectors.iter().zip(trained.assignments()).enumerate() {
        let point_id = IvfPointId::new((ordinal + 1) as u64).ok_or("point id is zero")?;
        lists[*list].push(IvfPosting::new(point_id, vector.clone())?);
    }
    let index = InMemoryIvfIndex::new(DistanceMetric::L2, trained.centroids().to_vec(), lists)?;
    let config = IvfConfig::new(2, 1, 2, 4, 16, 41, IvfIterativePolicy::StrictOrder)?;
    let outcome = search_ivf(
        &index,
        &[0.05, 0.0],
        &config,
        IvfProbeBudget::new(1).ok_or("probe budget is zero")?,
        IvfCandidateBudget::new(4).ok_or("candidate budget is zero")?,
        2,
        &|_: IvfPointId| true,
        &NeverCancelIvf,
    )?;

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
