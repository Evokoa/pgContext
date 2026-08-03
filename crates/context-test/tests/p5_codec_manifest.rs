//! Phase 5 codec-promotion manifest contract.

use context_test::{
    P5_CODEC_DIMENSIONS, P5_CODEC_GATES, P5_CODEC_GENERATOR_REVISION, P5_CODEC_QUERY_COUNT,
    P5_CODEC_QUERY_IDS, P5_CODEC_ROWS, P5_CODEC_TOP_K, P5_CODEC_TRAINING_SAMPLE_ROWS,
    p5_codec_manifest_hash,
};

#[test]
fn p5_codec_manifest_freezes_the_one_million_row_gate() {
    assert_eq!(P5_CODEC_ROWS, 1_000_000);
    assert_eq!(P5_CODEC_DIMENSIONS, 32);
    assert_eq!(P5_CODEC_TOP_K, 10);
    assert_eq!(P5_CODEC_QUERY_COUNT, 5);
    assert_eq!(P5_CODEC_GENERATOR_REVISION, 2);
    assert_eq!(P5_CODEC_QUERY_IDS, [17, 200_003, 400_009, 700_001, 999_983]);
    assert_eq!(P5_CODEC_TRAINING_SAMPLE_ROWS, 4_096);
    assert_eq!(
        P5_CODEC_GATES.map(|gate| gate.mode),
        ["binary", "scalar", "pq"]
    );
    assert!(
        P5_CODEC_GATES
            .iter()
            .all(|gate| (0.0..=1.0).contains(&gate.minimum_recall))
    );
    assert_ne!(p5_codec_manifest_hash(), 0);
}

#[test]
fn p5_codec_manifest_hash_is_stable() {
    assert_eq!(p5_codec_manifest_hash(), 0xd40f_e51a_782c_d221);
}
