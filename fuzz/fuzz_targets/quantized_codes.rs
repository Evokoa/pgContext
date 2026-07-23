#![no_main]

use context_core::{DenseVector, DistanceMetric};
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphQuantization, HnswGraphQuantizationCodebook,
    QuantizedHnswGraphView, decode_hnsw_graph_payload_versioned, encode_hnsw_graph_payload_v2,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    exercise(data);

    let Ok(left) = DenseVector::new(vec![-1.0, 1.0, -1.0, 1.0]) else {
        return;
    };
    let Ok(right) = DenseVector::new(vec![1.0, -1.0, 1.0, -1.0]) else {
        return;
    };
    let records = vec![
        HnswGraphArtifactRecord::new(0, 1, left, vec![1]),
        HnswGraphArtifactRecord::new(1, 2, right, vec![0]),
    ];
    let quantization = HnswGraphQuantization::new(
        HnswGraphQuantizationCodebook::Binary { dimensions: 4 },
        vec![vec![0b1010], vec![0b0101]],
    );
    let Ok(mut encoded) = encode_hnsw_graph_payload_v2(&records, Some(&quantization)) else {
        return;
    };
    if let Some(first) = data.first() {
        let offset = usize::from(*first) % encoded.len();
        encoded[offset] ^= data.get(1).copied().unwrap_or(0xff);
    }
    exercise(&encoded);
});

fn exercise(data: &[u8]) {
    let _ = QuantizedHnswGraphView::attach(data);
    let Ok(payload) = decode_hnsw_graph_payload_versioned(data) else {
        return;
    };
    let Some(quantization) = payload.quantization() else {
        return;
    };
    let Some(query) = payload.records().first().map(|record| record.vector()) else {
        return;
    };
    for code in quantization.codes() {
        let _ = quantization.codebook().reconstruct(code);
        for metric in [
            DistanceMetric::L2,
            DistanceMetric::L1,
            DistanceMetric::NegativeInnerProduct,
            DistanceMetric::Cosine,
        ] {
            let _ = quantization
                .codebook()
                .approximate_distance(query, code, metric);
            if let Ok(prepared) = quantization.codebook().prepare_query(query, metric) {
                let _ = prepared.score(code);
            }
        }
    }
}
