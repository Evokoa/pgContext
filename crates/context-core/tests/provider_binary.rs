//! Provider-native packed-binary ordering and padding tests.

use context_core::{Error, ProviderBinaryVector, ProviderBitOrder, ProviderByteOrder};

#[test]
fn provider_bytes_decode_all_declared_orderings() -> Result<(), Error> {
    let msb = ProviderBinaryVector::new(
        vec![0b1010_0000],
        4,
        ProviderBitOrder::MostSignificantFirst,
        ProviderByteOrder::MostSignificantFirst,
    )?;
    assert_eq!(msb.to_bit_vector()?.to_string(), "1010");

    let lsb = ProviderBinaryVector::new(
        vec![0b0000_0101],
        4,
        ProviderBitOrder::LeastSignificantFirst,
        ProviderByteOrder::MostSignificantFirst,
    )?;
    assert_eq!(lsb.to_bit_vector()?.to_string(), "1010");

    let reversed = ProviderBinaryVector::new(
        vec![0b0101_0101, 0b1010_1010],
        16,
        ProviderBitOrder::MostSignificantFirst,
        ProviderByteOrder::LeastSignificantFirst,
    )?;
    assert_eq!(reversed.to_bit_vector()?.to_string(), "1010101001010101");
    Ok(())
}

#[test]
fn provider_bytes_reject_length_and_nonzero_padding() {
    assert!(
        ProviderBinaryVector::new(
            vec![],
            1,
            ProviderBitOrder::MostSignificantFirst,
            ProviderByteOrder::MostSignificantFirst,
        )
        .is_err()
    );
    assert!(
        ProviderBinaryVector::new(
            vec![0b1010_0001],
            4,
            ProviderBitOrder::MostSignificantFirst,
            ProviderByteOrder::MostSignificantFirst,
        )
        .is_err()
    );
    assert!(
        ProviderBinaryVector::new(
            vec![0b1000_0000],
            0,
            ProviderBitOrder::MostSignificantFirst,
            ProviderByteOrder::MostSignificantFirst,
        )
        .is_err()
    );
}

#[test]
fn provider_bytes_accept_the_maximum_logical_length() -> Result<(), Error> {
    let logical_bits = context_core::policy::MAX_VECTOR_DIMENSIONS;
    let vector = ProviderBinaryVector::new(
        vec![0; logical_bits / 8],
        logical_bits,
        ProviderBitOrder::MostSignificantFirst,
        ProviderByteOrder::MostSignificantFirst,
    )?;
    assert_eq!(vector.logical_bits(), logical_bits);
    assert_eq!(vector.to_bit_vector()?.len(), logical_bits);
    Ok(())
}

#[test]
fn voyage_documented_binary_fixture_uses_msb_first_packing() -> Result<(), Error> {
    // Voyage's quantization guide publishes this exact eight-value sign
    // sequence as packed unsigned byte 77 (0b0100_1101).
    // https://docs.voyageai.com/docs/flexible-dimensions-and-quantization
    let provider = ProviderBinaryVector::new(
        vec![77],
        8,
        ProviderBitOrder::MostSignificantFirst,
        ProviderByteOrder::MostSignificantFirst,
    )?;
    assert_eq!(provider.to_bit_vector()?.to_string(), "01001101");
    Ok(())
}
