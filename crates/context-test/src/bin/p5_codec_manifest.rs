//! Emits the frozen Phase 5 codec manifest in line-oriented form.

#![allow(clippy::print_stdout)]

use context_test::{
    P5_CODEC_DIMENSIONS, P5_CODEC_GATES, P5_CODEC_GENERATOR_REVISION, P5_CODEC_QUERY_COUNT,
    P5_CODEC_QUERY_IDS, P5_CODEC_ROWS, P5_CODEC_SEED, P5_CODEC_TOP_K,
    P5_CODEC_TRAINING_SAMPLE_ROWS, p5_codec_manifest_hash,
};

fn main() {
    println!("manifest_hash\t{:016x}", p5_codec_manifest_hash());
    println!("rows\t{P5_CODEC_ROWS}");
    println!("dimensions\t{P5_CODEC_DIMENSIONS}");
    println!("seed\t{P5_CODEC_SEED}");
    println!("generator_revision\t{P5_CODEC_GENERATOR_REVISION}");
    println!(
        "query_ids\t{}",
        P5_CODEC_QUERY_IDS
            .map(|query_id| query_id.to_string())
            .join(",")
    );
    println!("query_count\t{P5_CODEC_QUERY_COUNT}");
    println!("top_k\t{P5_CODEC_TOP_K}");
    println!("training_sample_rows\t{P5_CODEC_TRAINING_SAMPLE_ROWS}");
    for gate in P5_CODEC_GATES {
        println!(
            "codec\t{}\t{}\t{}\t{}\t{}\t{}",
            gate.mode,
            gate.minimum_recall,
            gate.maximum_p95_latency_ms,
            gate.maximum_resident_bytes,
            gate.maximum_index_bytes,
            gate.maximum_build_seconds
        );
    }
}
