//! Vector storage and text conversion.

use core::{fmt, str::FromStr};

use crate::{Error, Result, policy::MAX_VECTOR_DIMENSIONS};

const HALF_MAX_FINITE: f32 = 65_504.0;

/// Vector storage representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorRepresentation {
    /// Dense single-precision values.
    Dense,
    /// Dense half-precision values widened to f32 in Rust.
    Half,
    /// Sparse values with explicit dimensions.
    Sparse,
    /// Dense bit values.
    Bit,
}

impl VectorRepresentation {
    /// Returns the required conversion policy between two vector
    /// representations.
    #[must_use]
    pub const fn conversion_policy_to(self, target: Self) -> VectorConversionPolicy {
        use VectorConversionPolicy::{CheckedLossy, Forbidden, Lossless};

        match (self, target) {
            (Self::Dense, Self::Dense)
            | (Self::Half, Self::Half)
            | (Self::Sparse, Self::Sparse)
            | (Self::Bit, Self::Bit)
            | (Self::Half, Self::Dense)
            | (Self::Dense, Self::Sparse)
            | (Self::Sparse, Self::Dense)
            | (Self::Half, Self::Sparse) => Lossless,
            (Self::Dense, Self::Half) | (Self::Sparse, Self::Half) => CheckedLossy,
            (Self::Dense | Self::Half | Self::Sparse, Self::Bit)
            | (Self::Bit, Self::Dense | Self::Half | Self::Sparse) => Forbidden,
        }
    }
}

/// Policy required to convert between vector representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorConversionPolicy {
    /// Conversion preserves all represented values.
    Lossless,
    /// Conversion may lose precision or clamp/reject values and must be checked.
    CheckedLossy,
    /// Conversion is not a vector representation cast.
    Forbidden,
}

/// Dense single-precision vector used by exact search and distance metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseVector {
    values: Vec<f32>,
}

impl DenseVector {
    /// Creates a dense vector after validating its values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the vector is empty, exceeds the
    /// vector dimension policy, or contains a non-finite value.
    pub fn new(values: Vec<f32>) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::InvalidVector(
                "dense vectors must contain at least one value".to_owned(),
            ));
        }
        ensure_vector_length("dense vector dimensions", values.len())?;

        if let Some((index, value)) = values
            .iter()
            .copied()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(Error::InvalidVector(format!(
                "value at dimension {index} is not finite: {value}"
            )));
        }

        Ok(Self { values })
    }

    /// Returns the vector dimension.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.values.len()
    }

    /// Returns the vector values.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.values
    }

    /// Consumes the vector and returns its validated values.
    #[must_use]
    pub fn into_values(self) -> Vec<f32> {
        self.values
    }
}

impl fmt::Display for DenseVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (index, value) in self.values.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{value}")?;
        }
        f.write_str("]")
    }
}

impl FromStr for DenseVector {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        let Some(inner) = trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        else {
            return Err(Error::InvalidVector(
                "dense vector text must be enclosed in square brackets".to_owned(),
            ));
        };

        let values = if inner.trim().is_empty() {
            Vec::new()
        } else {
            inner
                .split(',')
                .map(str::trim)
                .map(parse_value)
                .collect::<Result<Vec<_>>>()?
        };

        Self::new(values)
    }
}

/// Converts an `f32` to IEEE 754 binary16 bits with round-to-nearest-even.
///
/// Total over all inputs: overflow saturates to the signed infinity bit
/// pattern and NaN maps to a quiet half NaN, matching hardware `f32`→`f16`
/// conversion semantics. Callers that must exclude non-finite results
/// validate before or after conversion.
#[must_use]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "every cast is range-limited by the preceding masks and branch guards: the exponent field fits 8 bits, guarded branches keep unbiased exponents and shifts non-negative, and rounded mantissa/exponent combinations fit binary16's 15 value bits"
)]
pub fn f32_to_half_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;
    if exponent == 0xff {
        // Infinity keeps a zero mantissa; NaN keeps a nonzero (quieted) one.
        let nan_payload = if mantissa == 0 {
            0
        } else {
            0x0200 | ((mantissa >> 13) as u16 & 0x03ff)
        };
        return sign | 0x7c00 | nan_payload;
    }
    let unbiased = exponent - 127;
    if unbiased >= 16 {
        return sign | 0x7c00;
    }
    if unbiased >= -14 {
        // Normal half: keep 10 mantissa bits, round the 13 dropped bits to
        // nearest-even. A mantissa carry into the exponent field is correct
        // by construction (1.111..11 rounds up to the next power of two).
        let half_exponent = (unbiased + 15) as u32;
        let mut half_mantissa = mantissa >> 13;
        let dropped = mantissa & 0x1fff;
        if dropped > 0x1000 || (dropped == 0x1000 && (half_mantissa & 1) == 1) {
            half_mantissa += 1;
        }
        let combined = (half_exponent << 10) + half_mantissa;
        return sign | combined as u16;
    }
    if unbiased >= -25 {
        // Subnormal half: value = full_mantissa * 2^(unbiased - 23), and the
        // subnormal unit is 2^-24, so the target mantissa is
        // full_mantissa >> (13 + (-14 - unbiased)), rounded to nearest-even.
        // Rounding up from the largest subnormal into the smallest normal is
        // again correct by construction.
        let full_mantissa = mantissa | 0x0080_0000;
        let shift = (13 + (-14 - unbiased)) as u32;
        let mut half_mantissa = full_mantissa >> shift;
        let dropped = full_mantissa & ((1 << shift) - 1);
        let halfway = 1_u32 << (shift - 1);
        if dropped > halfway || (dropped == halfway && (half_mantissa & 1) == 1) {
            half_mantissa += 1;
        }
        return sign | half_mantissa as u16;
    }
    // Below the smallest subnormal: round to signed zero.
    sign
}

/// Widens IEEE 754 binary16 bits to the exactly representable `f32`.
#[must_use]
pub fn half_bits_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0_f32 } else { -1.0 };
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x03ff);
    // Exact power-of-two constants built directly from f32 bit patterns so
    // the conversion never depends on libm rounding.
    #[allow(
        clippy::cast_sign_loss,
        reason = "callers pass unbiased exponents in [-24, 16], so the biased field is always positive"
    )]
    let pow2 = |unbiased: i32| f32::from_bits(((unbiased + 127) as u32) << 23);
    match exponent {
        0 => sign * mantissa * pow2(-24),
        0x1f => {
            if mantissa == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mantissa / 1024.0) * pow2(i32::try_from(exponent).unwrap_or(0) - 15),
    }
}

/// Dense half-precision vector represented as checked f32 values.
#[derive(Debug, Clone, PartialEq)]
pub struct HalfVector {
    values: Vec<f32>,
}

impl HalfVector {
    /// Creates a half vector after validating its values are representable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the vector is empty, exceeds the
    /// vector dimension policy, contains a non-finite value, or contains a
    /// value outside the finite half-precision range.
    pub fn new(values: Vec<f32>) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::InvalidVector(
                "halfvec values must contain at least one value".to_owned(),
            ));
        }
        ensure_vector_length("halfvec dimensions", values.len())?;

        for (index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(Error::InvalidVector(format!(
                    "halfvec value at dimension {index} is not finite: {value}"
                )));
            }

            if value.abs() > HALF_MAX_FINITE {
                return Err(Error::InvalidVector(format!(
                    "halfvec value at dimension {index} exceeds finite half precision range: {value}"
                )));
            }
        }

        Ok(Self { values })
    }

    /// Returns the vector dimension.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.values.len()
    }

    /// Returns the vector values widened to f32.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.values
    }
}

impl fmt::Display for HalfVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (index, value) in self.values.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{value}")?;
        }
        f.write_str("]")
    }
}

impl FromStr for HalfVector {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        let Some(inner) = trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        else {
            return Err(Error::InvalidVector(
                "halfvec text must be enclosed in square brackets".to_owned(),
            ));
        };

        let values = if inner.trim().is_empty() {
            Vec::new()
        } else {
            inner
                .split(',')
                .map(str::trim)
                .map(parse_half_value)
                .collect::<Result<Vec<_>>>()?
        };

        Self::new(values)
    }
}

/// A non-zero sparse vector entry using a 1-based dimension index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SparseEntry {
    index: usize,
    value: f32,
}

impl SparseEntry {
    /// Creates a sparse entry without validating it against vector dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the index is zero or the value is
    /// not finite.
    pub fn new(index: usize, value: f32) -> Result<Self> {
        if index == 0 {
            return Err(Error::InvalidVector(
                "sparsevec indexes are 1-based".to_owned(),
            ));
        }

        if !value.is_finite() {
            return Err(Error::InvalidVector(format!(
                "sparsevec value at index {index} is not finite: {value}"
            )));
        }

        Ok(Self { index, value })
    }

    /// Returns the 1-based sparse dimension index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// Returns the sparse value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.value
    }
}

/// Sparse vector with canonical sorted entries and explicit dimensions.
#[derive(Debug, Clone, PartialEq)]
pub struct SparseVector {
    dimensions: usize,
    entries: Vec<SparseEntry>,
}

/// Dense bit vector for Hamming and Jaccard distances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitVector {
    bits: Vec<bool>,
}

impl BitVector {
    /// Creates a bit vector from boolean values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the vector is empty or exceeds the
    /// vector dimension policy.
    pub fn new(bits: Vec<bool>) -> Result<Self> {
        if bits.is_empty() {
            return Err(Error::InvalidVector(
                "bit vectors must contain at least one bit".to_owned(),
            ));
        }
        ensure_vector_length("bit vector bits", bits.len())?;

        Ok(Self { bits })
    }

    /// Returns the number of bits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// Returns true when the vector has no bits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// Returns the bit values.
    #[must_use]
    pub fn as_slice(&self) -> &[bool] {
        &self.bits
    }

    /// Computes Hamming distance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when bit lengths differ.
    pub fn hamming_distance(&self, other: &Self) -> Result<usize> {
        ensure_same_bit_length(self, other)?;

        Ok(self
            .bits
            .iter()
            .zip(&other.bits)
            .filter(|(left, right)| left != right)
            .count())
    }

    /// Computes Jaccard distance over set bits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when bit lengths differ.
    pub fn jaccard_distance(&self, other: &Self) -> Result<f64> {
        ensure_same_bit_length(self, other)?;

        let mut intersection = 0usize;
        let mut union = 0usize;
        for (left, right) in self.bits.iter().zip(&other.bits) {
            if *left || *right {
                union += 1;
            }
            if *left && *right {
                intersection += 1;
            }
        }

        if union == 0 {
            Ok(0.0)
        } else {
            let intersection = u32::try_from(intersection).map_err(|_| {
                Error::InvalidVector("bit vector intersection count exceeds u32".to_owned())
            })?;
            let union = u32::try_from(union).map_err(|_| {
                Error::InvalidVector("bit vector union count exceeds u32".to_owned())
            })?;

            Ok(1.0 - (f64::from(intersection) / f64::from(union)))
        }
    }
}

impl fmt::Display for BitVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for bit in &self.bits {
            f.write_str(if *bit { "1" } else { "0" })?;
        }
        Ok(())
    }
}

impl FromStr for BitVector {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        let mut bits = Vec::with_capacity(trimmed.len());
        for (index, byte) in trimmed.bytes().enumerate() {
            match byte {
                b'0' => bits.push(false),
                b'1' => bits.push(true),
                _ => {
                    return Err(Error::InvalidVector(format!(
                        "invalid bit at position {index}: {}",
                        byte as char
                    )));
                }
            }
        }

        Self::new(bits)
    }
}

impl SparseVector {
    /// Creates a sparse vector and canonicalizes entries by ascending index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when dimensions are zero, dimensions or
    /// nonzero entries exceed the vector policy, an entry index is zero or
    /// outside the dimensions, an entry value is not finite, or a dimension
    /// appears more than once.
    pub fn new(dimensions: usize, entries: Vec<SparseEntry>) -> Result<Self> {
        if dimensions == 0 {
            return Err(Error::InvalidVector(
                "sparsevec dimensions must be greater than zero".to_owned(),
            ));
        }
        ensure_vector_length("sparsevec dimensions", dimensions)?;
        ensure_vector_length("sparsevec nonzero entries", entries.len())?;

        let mut entries = entries;
        entries.sort_by_key(|entry| entry.index);

        let mut previous_index = None;
        for entry in &entries {
            if entry.index > dimensions {
                return Err(Error::InvalidVector(format!(
                    "sparsevec index {} exceeds dimensions {dimensions}",
                    entry.index
                )));
            }

            if previous_index == Some(entry.index) {
                return Err(Error::InvalidVector(format!(
                    "sparsevec duplicate index: {}",
                    entry.index
                )));
            }

            previous_index = Some(entry.index);
        }

        Ok(Self {
            dimensions,
            entries,
        })
    }

    /// Returns the explicit vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the number of stored entries.
    #[must_use]
    pub fn non_zero_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns canonical entries sorted by 1-based index.
    #[must_use]
    pub fn entries(&self) -> &[SparseEntry] {
        &self.entries
    }
}

impl fmt::Display for SparseVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (position, entry) in self.entries.iter().enumerate() {
            if position > 0 {
                f.write_str(",")?;
            }
            write!(f, "{}:{}", entry.index, entry.value)?;
        }
        write!(f, "}}/{}", self.dimensions)
    }
}

impl FromStr for SparseVector {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        let Some((entries_text, dimensions_text)) = trimmed.rsplit_once('/') else {
            return Err(Error::InvalidVector(
                "sparsevec text must include dimensions after '/'".to_owned(),
            ));
        };
        let Some(inner) = entries_text
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        else {
            return Err(Error::InvalidVector(
                "sparsevec entries must be enclosed in braces".to_owned(),
            ));
        };

        let dimensions = dimensions_text.trim().parse::<usize>().map_err(|_| {
            Error::InvalidVector(format!("invalid sparsevec dimensions: {dimensions_text}"))
        })?;
        let entries = if inner.trim().is_empty() {
            Vec::new()
        } else {
            inner
                .split(',')
                .map(str::trim)
                .map(parse_sparse_entry)
                .collect::<Result<Vec<_>>>()?
        };

        Self::new(dimensions, entries)
    }
}

fn parse_value(value: &str) -> Result<f32> {
    if value.is_empty() {
        return Err(Error::InvalidVector(
            "dense vector values must not be empty".to_owned(),
        ));
    }

    value
        .parse::<f32>()
        .map_err(|_| Error::InvalidVector(format!("invalid float value: {value}")))
}

fn parse_half_value(value: &str) -> Result<f32> {
    if value.is_empty() {
        return Err(Error::InvalidVector(
            "halfvec values must not be empty".to_owned(),
        ));
    }

    value
        .parse::<f32>()
        .map_err(|_| Error::InvalidVector(format!("invalid halfvec value: {value}")))
}

fn parse_sparse_entry(value: &str) -> Result<SparseEntry> {
    let Some((index, value)) = value.split_once(':') else {
        return Err(Error::InvalidVector(format!(
            "invalid sparsevec entry: {value}"
        )));
    };

    let index = index
        .trim()
        .parse::<usize>()
        .map_err(|_| Error::InvalidVector(format!("invalid sparsevec index: {index}")))?;
    let value = value
        .trim()
        .parse::<f32>()
        .map_err(|_| Error::InvalidVector(format!("invalid sparsevec value: {value}")))?;

    SparseEntry::new(index, value)
}

fn ensure_vector_length(label: &'static str, actual: usize) -> Result<()> {
    if actual > MAX_VECTOR_DIMENSIONS {
        return Err(Error::InvalidVector(format!(
            "{label} exceed policy limit {MAX_VECTOR_DIMENSIONS}: {actual}"
        )));
    }

    Ok(())
}

fn ensure_same_bit_length(left: &BitVector, right: &BitVector) -> Result<()> {
    if left.len() == right.len() {
        Ok(())
    } else {
        Err(Error::DimensionMismatch {
            left: left.len(),
            right: right.len(),
        })
    }
}
