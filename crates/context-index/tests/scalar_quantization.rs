//! Scalar quantization tests.

use context_core::DenseVector;
use context_index::{ScalarQuantizedVector, ScalarQuantizer};

#[test]
fn scalar_quantize_maps_values_to_nearest_codes() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 5)?;
    let vector: DenseVector = "[-1,-0.6,0.2,0.9]".parse()?;

    let quantized = quantizer.quantize(&vector)?;

    assert_eq!(quantized.codes(), &[0, 1, 2, 4]);

    Ok(())
}

#[test]
fn scalar_quantize_uses_constant_work_per_dimension_at_256_levels()
-> Result<(), Box<dyn std::error::Error>> {
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 256)?;
    let vector: DenseVector = "[-1,-0.5,0,0.5,1]".parse()?;

    let quantized = quantizer.quantize(&vector)?;

    assert_eq!(quantized.codes(), &[0, 64, 128, 191, 255]);
    Ok(())
}

#[test]
fn scalar_quantize_clamps_values_outside_codebook() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 5)?;
    let vector: DenseVector = "[-2,2]".parse()?;

    let quantized = quantizer.quantize(&vector)?;

    assert_eq!(quantized.codes(), &[0, 4]);

    Ok(())
}

#[test]
fn scalar_quantized_codes_reconstruct_dense_values() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 5)?;
    let quantized = ScalarQuantizedVector::new(vec![0, 2, 4])?;

    let reconstructed = quantizer.reconstruct(&quantized)?;

    assert_eq!(reconstructed.as_slice(), &[-1.0, 0.0, 1.0]);

    Ok(())
}

#[test]
fn scalar_quantizer_rejects_invalid_codebooks() {
    assert!(matches!(
        ScalarQuantizer::new(1.0, 1.0, 5),
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "invalid scalar quantization codebook: min must be less than max"
    ));
    assert!(matches!(
        ScalarQuantizer::new(-1.0, 1.0, 1),
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "invalid scalar quantization codebook: levels must be in 2..=256, got 1"
    ));
    assert!(matches!(
        ScalarQuantizer::new(f32::NAN, 1.0, 5),
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "invalid scalar quantization codebook: bounds must be finite"
    ));
}

#[test]
fn scalar_reconstruct_rejects_codes_outside_codebook() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 5)?;
    let quantized = ScalarQuantizedVector::new(vec![5])?;

    let result = quantizer.reconstruct(&quantized);

    assert!(matches!(
        result,
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "scalar quantized code 5 exceeds codebook levels 5"
    ));

    Ok(())
}
