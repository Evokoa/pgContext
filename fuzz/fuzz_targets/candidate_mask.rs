#![no_main]

use context_core::{DenseVector, DistanceMetric, SearchLimit, policy};
use context_index::{CandidateMask, HnswConfig, HnswError, HnswGraph, HnswPointId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(query) = DenseVector::new(vec![0.0, 0.0]) else {
        return;
    };
    let Ok(config) = HnswConfig::new(4, 16, 16) else {
        return;
    };
    let graph = HnswGraph::new(DistanceMetric::L2, config);
    let Ok(limit) = SearchLimit::new(3) else {
        return;
    };
    let point_ids = candidate_point_ids(data);
    let mask = CandidateMask::only(point_ids.iter().copied().map(HnswPointId::new));
    let result = graph.search_with_mask(&query, limit, &mask);

    if point_ids.len() > policy::MAX_HNSW_CANDIDATE_MASK_POINTS {
        assert!(matches!(
            result,
            Err(HnswError::RecallBudgetExceeded { max, actual })
                if max == policy::MAX_HNSW_CANDIDATE_MASK_POINTS && actual == point_ids.len()
        ));
    } else {
        assert!(result.is_ok());
    }
});

fn candidate_point_ids(data: &[u8]) -> Vec<u64> {
    if data.starts_with(b"OVERBUDGET") {
        return (1..=policy::MAX_HNSW_CANDIDATE_MASK_POINTS + 1)
            .map(usize_to_u64)
            .collect();
    }

    data.chunks(8)
        .take(policy::MAX_HNSW_CANDIDATE_MASK_POINTS)
        .enumerate()
        .map(|(index, chunk)| {
            let mut bytes = [0_u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            u64::from_le_bytes(bytes).max(usize_to_u64(index + 1))
        })
        .collect()
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
