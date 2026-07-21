#![no_main]

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, HnswConfig, HnswGraph, HnswLevel, HnswPointId, NeverCancel,
};
use libfuzzer_sys::fuzz_target;

const MAX_OPERATIONS: usize = 64;

fuzz_target!(|data: &[u8]| {
    let Ok(config) = HnswConfig::new(4, 16, 8) else {
        return;
    };
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    let mut point_id = 1_u64;

    for operation in data.chunks(4).take(MAX_OPERATIONS) {
        let opcode = operation.first().copied().unwrap_or_default() % 4;
        let x = operation.get(1).copied().unwrap_or_default() as f32;
        let y = operation.get(2).copied().unwrap_or_default() as f32;
        let level = HnswLevel::new(usize::from(operation.get(3).copied().unwrap_or_default() % 6));
        let Ok(vector) = DenseVector::new(vec![x, y]) else {
            return;
        };
        match opcode {
            0 => {
                let _ = graph.insert(HnswPointId::new(point_id), vector);
                point_id = point_id.saturating_add(1);
            }
            1 => {
                if let Ok(level) = level {
                    let _ = graph.insert_at_level(HnswPointId::new(point_id), vector, level);
                    point_id = point_id.saturating_add(1);
                }
            }
            2 => {
                let Ok(limit) = SearchLimit::new(3) else {
                    return;
                };
                let _ = graph.search_with_control(
                    &vector,
                    limit,
                    &CandidateMask::all(),
                    &mut NeverCancel,
                );
            }
            _ => {
                let snapshot = graph.snapshot();
                if let Ok(bytes) = snapshot.to_bytes()
                    && let Ok(decoded) = context_index::HnswGraphSnapshot::from_bytes(&bytes)
                {
                    let _ = HnswGraph::from_snapshot(DistanceMetric::L2, config, decoded);
                }
            }
        }
    }
});
