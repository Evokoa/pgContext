//! Bounded contiguous storage for fixed-width encoded vectors.

use bytemuck::{cast_slice, cast_slice_mut};

use crate::{CodecError, Result};

/// Required pointer and stride alignment for contiguous encoded rows.
pub const CONTIGUOUS_CODE_ALIGNMENT_BYTES: usize = 16;

/// Owned, sixteen-byte-aligned fixed-width codec rows.
///
/// Padding bytes are always zero and are excluded from [`Self::code`]. This
/// lets mapped and page adapters use the same row/stride contract without
/// allocating in the scoring loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContiguousCodes {
    code_width: usize,
    stride: usize,
    row_count: usize,
    words: Vec<u128>,
}

impl ContiguousCodes {
    /// Packs encoded rows into a sixteen-byte-strided contiguous section.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] when `code_width` is zero, a row
    /// has the wrong width, or the required section size overflows `usize`.
    pub fn from_rows(code_width: usize, rows: &[Vec<u8>]) -> Result<Self> {
        Self::from_row_writer(code_width, rows.len(), |row, output| {
            output.extend_from_slice(&rows[row]);
            Ok(())
        })
    }

    /// Encodes rows directly into one contiguous allocation while reusing one
    /// bounded scratch buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] when a callback emits the wrong
    /// width or when the required section size overflows `usize`.
    pub fn from_row_writer(
        code_width: usize,
        row_count: usize,
        mut write_row: impl FnMut(usize, &mut Vec<u8>) -> Result<()>,
    ) -> Result<Self> {
        if code_width == 0 {
            return Err(CodecError::InvalidCode(
                "contiguous code width must be positive".to_owned(),
            ));
        }
        let stride = aligned_stride(code_width)?;
        let byte_len = stride.checked_mul(row_count).ok_or_else(|| {
            CodecError::InvalidCode("contiguous code section length overflows usize".to_owned())
        })?;
        let mut words = vec![0_u128; byte_len / CONTIGUOUS_CODE_ALIGNMENT_BYTES];
        let bytes = cast_slice_mut::<u128, u8>(&mut words);
        let mut row = Vec::with_capacity(code_width);
        for index in 0..row_count {
            row.clear();
            write_row(index, &mut row)?;
            if row.len() != code_width {
                return Err(CodecError::InvalidCode(format!(
                    "contiguous code row {index} width mismatch: expected {code_width}, got {}",
                    row.len()
                )));
            }
            let start = index * stride;
            bytes[start..start + code_width].copy_from_slice(&row);
        }
        Ok(Self {
            code_width,
            stride,
            row_count,
            words,
        })
    }

    /// Returns the encoded byte width excluding alignment padding.
    #[must_use]
    pub const fn code_width(&self) -> usize {
        self.code_width
    }

    /// Returns the fixed byte distance between consecutive rows.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Returns the number of encoded rows.
    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    /// Borrows one encoded row without its padding.
    #[must_use]
    pub fn code(&self, row: usize) -> Option<&[u8]> {
        self.view().code(row)
    }

    /// Returns the complete contiguous section including zero padding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        cast_slice(&self.words)
    }

    /// Returns a zero-copy validated view over this owned section.
    #[must_use]
    pub fn view(&self) -> ContiguousCodeView<'_> {
        ContiguousCodeView {
            code_width: self.code_width,
            stride: self.stride,
            row_count: self.row_count,
            bytes: self.as_bytes(),
        }
    }
}

/// Borrowed validated view over fixed-width aligned codec rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContiguousCodeView<'a> {
    code_width: usize,
    stride: usize,
    row_count: usize,
    bytes: &'a [u8],
}

impl<'a> ContiguousCodeView<'a> {
    /// Validates and attaches to a contiguous codec section.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] for zero widths, unaligned or short
    /// strides/pointers, overflow, a section-length mismatch, or nonzero padding.
    pub fn new(
        code_width: usize,
        stride: usize,
        row_count: usize,
        bytes: &'a [u8],
    ) -> Result<Self> {
        if code_width == 0 {
            return Err(CodecError::InvalidCode(
                "contiguous code width must be positive".to_owned(),
            ));
        }
        if stride < code_width || !stride.is_multiple_of(CONTIGUOUS_CODE_ALIGNMENT_BYTES) {
            return Err(CodecError::InvalidCode(format!(
                "contiguous code stride must be at least {code_width} and a multiple of {CONTIGUOUS_CODE_ALIGNMENT_BYTES}: {stride}"
            )));
        }
        if !(bytes.as_ptr() as usize).is_multiple_of(CONTIGUOUS_CODE_ALIGNMENT_BYTES) {
            return Err(CodecError::InvalidCode(format!(
                "contiguous code address must be {CONTIGUOUS_CODE_ALIGNMENT_BYTES}-byte aligned"
            )));
        }
        let expected = stride.checked_mul(row_count).ok_or_else(|| {
            CodecError::InvalidCode("contiguous code section length overflows usize".to_owned())
        })?;
        if bytes.len() != expected {
            return Err(CodecError::InvalidCode(format!(
                "contiguous code section length mismatch: expected {expected}, got {}",
                bytes.len()
            )));
        }
        if stride > code_width
            && bytes
                .chunks_exact(stride)
                .any(|row| row[code_width..].iter().any(|byte| *byte != 0))
        {
            return Err(CodecError::InvalidCode(
                "contiguous code section has nonzero alignment padding".to_owned(),
            ));
        }
        Ok(Self {
            code_width,
            stride,
            row_count,
            bytes,
        })
    }

    /// Returns the encoded byte width excluding alignment padding.
    #[must_use]
    pub const fn code_width(self) -> usize {
        self.code_width
    }

    /// Returns the fixed byte distance between consecutive rows.
    #[must_use]
    pub const fn stride(self) -> usize {
        self.stride
    }

    /// Returns the number of encoded rows.
    #[must_use]
    pub const fn row_count(self) -> usize {
        self.row_count
    }

    /// Borrows one encoded row without allocating.
    #[must_use]
    pub fn code(self, row: usize) -> Option<&'a [u8]> {
        if row >= self.row_count {
            return None;
        }
        let start = row.checked_mul(self.stride)?;
        self.bytes.get(start..start + self.code_width)
    }

    /// Returns the complete validated section including padding.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
}

fn aligned_stride(code_width: usize) -> Result<usize> {
    code_width
        .checked_add(CONTIGUOUS_CODE_ALIGNMENT_BYTES - 1)
        .map(|value| value / CONTIGUOUS_CODE_ALIGNMENT_BYTES * CONTIGUOUS_CODE_ALIGNMENT_BYTES)
        .ok_or_else(|| CodecError::InvalidCode("contiguous code stride overflows usize".to_owned()))
}
