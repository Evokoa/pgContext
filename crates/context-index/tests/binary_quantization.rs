//! Binary quantization tests.

use context_core::{BitVector, DenseVector};
use context_index::binary_quantize;

#[test]
fn binary_quantize_thresholds_dense_vector_signs() -> Result<(), Box<dyn std::error::Error>> {
    let vector: DenseVector = "[1,-2,0,3.5]".parse()?;

    let code = binary_quantize(&vector)?;

    assert_eq!(code, "1011".parse::<BitVector>()?);
    assert_eq!(code.to_string(), "1011");

    Ok(())
}

#[test]
fn binary_quantize_preserves_input_dimensions_as_bit_length()
-> Result<(), Box<dyn std::error::Error>> {
    let vector: DenseVector = "[-1,-2,-3]".parse()?;

    let code = binary_quantize(&vector)?;

    assert_eq!(code.len(), vector.dimension());
    assert_eq!(code.to_string(), "000");

    Ok(())
}

#[test]
fn binary_quantize_requires_valid_source_vectors() {
    let result = DenseVector::new(Vec::new());

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "dense vectors must contain at least one value"
    ));
}
