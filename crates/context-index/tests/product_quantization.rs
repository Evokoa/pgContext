//! Product quantization prototype tests.

use context_core::DenseVector;
use context_index::{ProductCodebook, ProductQuantizedVector, ProductQuantizer};

#[test]
fn product_quantize_selects_nearest_centroids() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = product_quantizer()?;
    let vector: DenseVector = "[0.9,0.1,-0.8,0.2]".parse()?;

    let quantized = quantizer.quantize(&vector)?;

    assert_eq!(quantized.codes(), &[1, 0]);

    Ok(())
}

#[test]
fn product_quantized_codes_reconstruct_dense_vector() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = product_quantizer()?;
    let quantized = ProductQuantizedVector::new(vec![1, 0])?;

    let reconstructed = quantizer.reconstruct(&quantized)?;

    assert_eq!(reconstructed.as_slice(), &[1.0, 0.0, -1.0, 0.0]);

    Ok(())
}

#[test]
fn product_quantizer_rejects_mismatched_vector_dimensions() -> Result<(), Box<dyn std::error::Error>>
{
    let quantizer = product_quantizer()?;
    let vector: DenseVector = "[1,0]".parse()?;

    let result = quantizer.quantize(&vector);

    assert!(matches!(
        result,
        Err(context_index::HnswError::DimensionMismatch { left: 4, right: 2 })
    ));

    Ok(())
}

#[test]
fn product_quantizer_rejects_invalid_codebooks() -> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(
        ProductCodebook::new(Vec::new()),
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "invalid product quantization codebook: centroids must not be empty"
    ));
    assert!(matches!(
        ProductQuantizer::new(0, vec![ProductCodebook::new(vec!["[0]".parse()?])?]),
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "invalid product quantization codebook: subvector dimensions must be greater than zero"
    ));

    Ok(())
}

#[test]
fn product_reconstruct_rejects_codes_outside_codebook() -> Result<(), Box<dyn std::error::Error>> {
    let quantizer = product_quantizer()?;
    let quantized = ProductQuantizedVector::new(vec![2, 0])?;

    let result = quantizer.reconstruct(&quantized);

    assert!(matches!(
        result,
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "product quantized code 2 exceeds codebook size 2"
    ));

    Ok(())
}

fn product_quantizer() -> Result<ProductQuantizer, Box<dyn std::error::Error>> {
    Ok(ProductQuantizer::new(
        2,
        vec![
            ProductCodebook::new(vec!["[0,0]".parse()?, "[1,0]".parse()?])?,
            ProductCodebook::new(vec!["[-1,0]".parse()?, "[0,1]".parse()?])?,
        ],
    )?)
}
