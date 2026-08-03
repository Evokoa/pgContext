//! Contract tests for normalized codec specifications and contiguous codes.

use context_codec::{
    CodecKind, CodecSpec, ContiguousCodeView, ContiguousCodes, ReconstructionPolicy, ScalarBounds,
    TrainedCodecArtifact,
};
use context_core::{DenseVector, DistanceMetric};

#[test]
fn codec_specs_are_validated_and_revision_stable() -> Result<(), Box<dyn std::error::Error>> {
    let sq8 = CodecSpec::scalar(256, Some(ScalarBounds::new(-1.0, 1.0)?))?;
    let same = CodecSpec::scalar(256, Some(ScalarBounds::new(-1.0, 1.0)?))?;
    let binary = CodecSpec::binary();
    let pq = CodecSpec::product(4, 16, 8)?;

    assert_eq!(sq8.kind(), CodecKind::Scalar);
    assert_eq!(sq8.revision(), same.revision());
    assert_ne!(sq8.revision(), binary.revision());
    assert_ne!(sq8.revision(), pq.revision());
    assert_eq!(
        sq8.reconstruction_policy(),
        ReconstructionPolicy::ExactSourceRerank
    );
    assert!(CodecSpec::scalar(1, None).is_err());
    assert!(CodecSpec::scalar(257, None).is_err());
    assert!(ScalarBounds::new(1.0, 1.0).is_err());
    assert!(CodecSpec::product(0, 16, 8).is_err());
    assert!(CodecSpec::product(4, 0, 8).is_err());
    assert!(CodecSpec::product(4, 257, 8).is_err());
    assert!(CodecSpec::product(4, 16, 0).is_err());
    Ok(())
}

#[test]
fn contiguous_codes_are_aligned_bounded_and_zero_copy() -> Result<(), Box<dyn std::error::Error>> {
    let rows = vec![vec![1_u8, 2, 3], vec![4, 5, 6], vec![7, 8, 9]];
    let codes = ContiguousCodes::from_rows(3, &rows)?;

    assert_eq!(codes.code_width(), 3);
    assert_eq!(codes.stride(), 16);
    assert_eq!(codes.row_count(), 3);
    assert_eq!(codes.code(0), Some(&[1, 2, 3][..]));
    assert_eq!(codes.code(2), Some(&[7, 8, 9][..]));
    assert_eq!(codes.code(3), None);
    assert!(codes.as_bytes()[3..16].iter().all(|byte| *byte == 0));
    assert_eq!(codes.as_bytes().as_ptr() as usize % 16, 0);

    let view = ContiguousCodeView::new(
        codes.code_width(),
        codes.stride(),
        codes.row_count(),
        codes.as_bytes(),
    )?;
    assert_eq!(view.code(1), Some(&[4, 5, 6][..]));
    let mut deliberately_unaligned = vec![0_u8];
    deliberately_unaligned.extend_from_slice(codes.as_bytes());
    assert!(
        ContiguousCodeView::new(
            codes.code_width(),
            codes.stride(),
            codes.row_count(),
            &deliberately_unaligned[1..],
        )
        .is_err()
    );
    assert!(ContiguousCodeView::new(3, 2, 1, &[1, 2]).is_err());
    assert!(ContiguousCodeView::new(3, 16, 2, &[0; 31]).is_err());

    let mut nonzero_padding = vec![0_u8; 16];
    nonzero_padding[15] = 1;
    assert!(ContiguousCodeView::new(3, 16, 1, &nonzero_padding).is_err());
    Ok(())
}

#[test]
fn trained_artifacts_are_deterministic_and_prepare_static_scorers()
-> Result<(), Box<dyn std::error::Error>> {
    let sample = vec![
        DenseVector::new(vec![-1.0, -0.5, 0.0, 0.5])?,
        DenseVector::new(vec![0.0, 0.5, 1.0, 1.5])?,
        DenseVector::new(vec![1.0, 1.5, 2.0, 2.5])?,
    ];
    let spec = CodecSpec::product(2, 3, 4)?;
    let first = TrainedCodecArtifact::train(spec, &sample)?;
    let second = TrainedCodecArtifact::train(spec, &sample)?;

    assert_eq!(first.revision(), second.revision());
    assert_eq!(first.codebook(), second.codebook());
    let Some(first_codes) = first.encode(&sample)? else {
        return Err(std::io::Error::other("product codec has no codes").into());
    };
    let Some(second_codes) = second.encode(&sample)? else {
        return Err(std::io::Error::other("product codec has no codes").into());
    };
    assert_eq!(first_codes, second_codes);

    let Some(prepared) = first.prepare_query(&sample[0], DistanceMetric::L2)? else {
        return Err(std::io::Error::other("product codec has no scorer").into());
    };
    let Some(second_code) = first_codes.code(1) else {
        return Err(std::io::Error::other("second product code is missing").into());
    };
    let score = prepared.score(second_code)?;
    assert!(score.is_finite());

    let plain = TrainedCodecArtifact::train(CodecSpec::plain(), &sample)?;
    assert!(plain.codebook().is_none());
    assert!(plain.encode(&sample)?.is_none());
    assert!(
        plain
            .prepare_query(&sample[0], DistanceMetric::L2)?
            .is_none()
    );
    Ok(())
}
