//! Portable codec-artifact format and corruption tests.

use context_codec::{CodecSpec, TrainedCodecArtifact};
use context_core::DenseVector;
use context_storage::{CodecArtifact, CodecArtifactView, encode_codec_artifact};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn sample() -> TestResult<Vec<DenseVector>> {
    Ok(vec![
        DenseVector::new(vec![-1.0, 0.0, 1.0, 2.0])?,
        DenseVector::new(vec![2.0, 1.0, 0.0, -1.0])?,
    ])
}

#[test]
fn codec_artifact_round_trips_a_contiguous_zero_copy_code_view() -> TestResult {
    let sample = sample()?;
    let trained = TrainedCodecArtifact::train(CodecSpec::scalar(256, None)?, &sample)?;
    let codes = trained
        .encode(&sample)?
        .ok_or_else(|| std::io::Error::other("scalar artifact has no codes"))?;
    let codebook = trained
        .codebook()
        .cloned()
        .ok_or_else(|| std::io::Error::other("scalar artifact has no codebook"))?;
    let artifact = CodecArtifact::new(
        trained.revision(),
        trained.reconstruction_policy(),
        codebook,
        codes,
    )?;

    let encoded = encode_codec_artifact(&artifact)?;
    let view = CodecArtifactView::attach(&encoded)?;

    assert_eq!(view.revision(), trained.revision());
    assert_eq!(view.dimensions(), 4);
    assert_eq!(view.codes().row_count(), 2);
    assert_eq!(view.codes().code(0), artifact.codes().code(0));
    let first = view
        .codes()
        .code(0)
        .ok_or_else(|| std::io::Error::other("first code is missing"))?;
    assert!(first.as_ptr() >= encoded.as_ptr());
    assert!(first.as_ptr() < encoded[encoded.len()..].as_ptr());

    let mut deliberately_unaligned = vec![0_u8];
    deliberately_unaligned.extend_from_slice(&encoded);
    assert!(CodecArtifactView::attach(&deliberately_unaligned[1..]).is_err());
    Ok(())
}

#[test]
fn codec_artifact_rejects_versions_checksums_lengths_and_padding() -> TestResult {
    let sample = sample()?;
    let trained = TrainedCodecArtifact::train(CodecSpec::binary(), &sample)?;
    let artifact = CodecArtifact::new(
        trained.revision(),
        trained.reconstruction_policy(),
        trained
            .codebook()
            .cloned()
            .ok_or_else(|| std::io::Error::other("binary artifact has no codebook"))?,
        trained
            .encode(&sample)?
            .ok_or_else(|| std::io::Error::other("binary artifact has no codes"))?,
    )?;
    let encoded = encode_codec_artifact(&artifact)?;

    for offset in [8_usize, 40, encoded.len() - 1] {
        let mut corrupt = encoded.clone();
        corrupt[offset] ^= 0x5a;
        assert!(CodecArtifactView::attach(&corrupt).is_err());
    }
    assert!(CodecArtifactView::attach(&encoded[..encoded.len() - 1]).is_err());
    Ok(())
}
