//! Delta-segment page codec for the segmented HNSW write path.
//!
//! A delta segment absorbs writes in O(1): instead of splicing every insert
//! into the HNSW graph (the measured 1-2 rows/s path), the access method
//! appends fixed-format records to delta pages and query time merges an
//! exact scan over the (bounded) delta with the base-graph results. This
//! module is the pure byte codec for one delta page payload; the access
//! method owns page allocation, WAL, and chaining.
//!
//! ## Page payload layout (little-endian, versioned, checksummed)
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 8 | magic `PGCTXDLT` |
//! | 8 | 4 | version (currently 1) |
//! | 12 | 4 | record count |
//! | 16 | 8 | compaction generation the page belongs to |
//! | 24 | 8 | FNV-1a checksum (header with zero checksum, then records) |
//! | 32 | .. | records |
//!
//! Each record: `heap_tid: u64`, `flags: u16` (`1 = LIVE`, `2 = TOMBSTONE`),
//! `dimension: u16`, then `dimension` little-endian `f32` values (zero for
//! tombstones — a tombstone needs only the heap TID it retires). Decoding
//! fails closed on any structural violation: bad magic/version, checksum
//! mismatch, truncated records, non-finite vector values, a LIVE record
//! with zero dimension, or trailing bytes after the declared record count.

use core::fmt;

/// Byte length of the fixed delta page payload header.
pub const DELTA_PAGE_HEADER_BYTES: usize = 32;

const DELTA_PAGE_MAGIC: [u8; 8] = *b"PGCTXDLT";
const DELTA_PAGE_VERSION: u32 = 1;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const RECORD_FIXED_BYTES: usize = 12;

/// Maximum vector dimension a delta record may carry, shared with the
/// packed-image and SQL-facing policy bound.
pub const DELTA_MAX_DIMENSIONS: usize = 16_000;

/// Whether a delta record adds a row or retires one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaRecordKind {
    /// The record carries a vector for a newly written heap row.
    Live,
    /// The record retires every earlier occurrence of its heap TID
    /// (in the base graph and in earlier delta records alike).
    Tombstone,
}

impl DeltaRecordKind {
    const fn flags(self) -> u16 {
        match self {
            Self::Live => 1,
            Self::Tombstone => 2,
        }
    }

    const fn from_flags(flags: u16) -> Option<Self> {
        match flags {
            1 => Some(Self::Live),
            2 => Some(Self::Tombstone),
            _ => None,
        }
    }
}

/// One decoded delta record.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaRecord {
    /// Heap tuple identity encoded as pgContext's canonical u64 TID.
    pub heap_tid: u64,
    /// Live insert or tombstone.
    pub kind: DeltaRecordKind,
    /// Vector values; empty for tombstones.
    pub vector: Vec<f32>,
}

impl DeltaRecord {
    /// Creates a live record after validating its vector.
    ///
    /// # Errors
    ///
    /// Returns [`DeltaSegmentError::InvalidRecord`] for an empty,
    /// over-dimensioned, or non-finite vector.
    pub fn live(heap_tid: u64, vector: Vec<f32>) -> Result<Self, DeltaSegmentError> {
        if vector.is_empty() || vector.len() > DELTA_MAX_DIMENSIONS {
            return Err(DeltaSegmentError::InvalidRecord {
                reason: "live delta record dimension is out of range",
            });
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(DeltaSegmentError::InvalidRecord {
                reason: "live delta record contains a non-finite value",
            });
        }
        Ok(Self {
            heap_tid,
            kind: DeltaRecordKind::Live,
            vector,
        })
    }

    /// Creates a tombstone record for a heap TID.
    #[must_use]
    pub const fn tombstone(heap_tid: u64) -> Self {
        Self {
            heap_tid,
            kind: DeltaRecordKind::Tombstone,
            vector: Vec::new(),
        }
    }

    /// Encoded byte length of this record.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        RECORD_FIXED_BYTES + self.vector.len() * size_of::<f32>()
    }
}

/// Typed decode/encode failures for delta pages.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeltaSegmentError {
    /// The payload is smaller than the fixed header.
    Truncated,
    /// The magic bytes do not identify a delta page.
    BadMagic,
    /// The version is not supported by this build.
    UnsupportedVersion {
        /// Version found in the header.
        found: u32,
    },
    /// The checksum does not match the header and record bytes.
    ChecksumMismatch,
    /// A record violates the structural contract.
    InvalidRecord {
        /// Human-readable structural violation.
        reason: &'static str,
    },
    /// The records do not fit the target page payload size.
    PageOverflow {
        /// Bytes required by the encoded records.
        required: usize,
        /// Bytes available in the page payload.
        available: usize,
    },
}

impl fmt::Display for DeltaSegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "delta page payload is truncated"),
            Self::BadMagic => write!(formatter, "delta page magic mismatch"),
            Self::UnsupportedVersion { found } => {
                write!(formatter, "unsupported delta page version {found}")
            }
            Self::ChecksumMismatch => write!(formatter, "delta page checksum mismatch"),
            Self::InvalidRecord { reason } => {
                write!(formatter, "invalid delta record: {reason}")
            }
            Self::PageOverflow {
                required,
                available,
            } => write!(
                formatter,
                "delta records need {required} bytes but the page payload holds {available}"
            ),
        }
    }
}

impl core::error::Error for DeltaSegmentError {}

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Encodes records into one delta page payload of exactly `payload_len`
/// bytes (the access method passes its usable page payload size; unused
/// tail bytes are zero and covered by the checksum).
///
/// # Errors
///
/// Returns [`DeltaSegmentError::PageOverflow`] when the records exceed
/// `payload_len` and [`DeltaSegmentError::InvalidRecord`] when a record
/// violates the structural contract.
pub fn encode_delta_page(
    generation: u64,
    records: &[DeltaRecord],
    payload_len: usize,
) -> Result<Vec<u8>, DeltaSegmentError> {
    let record_count =
        u32::try_from(records.len()).map_err(|_| DeltaSegmentError::InvalidRecord {
            reason: "record count exceeds u32",
        })?;
    let required: usize =
        DELTA_PAGE_HEADER_BYTES + records.iter().map(DeltaRecord::encoded_len).sum::<usize>();
    if required > payload_len {
        return Err(DeltaSegmentError::PageOverflow {
            required,
            available: payload_len,
        });
    }
    let mut payload = Vec::with_capacity(payload_len);
    payload.extend_from_slice(&DELTA_PAGE_MAGIC);
    payload.extend_from_slice(&DELTA_PAGE_VERSION.to_le_bytes());
    payload.extend_from_slice(&record_count.to_le_bytes());
    payload.extend_from_slice(&generation.to_le_bytes());
    payload.extend_from_slice(&0_u64.to_le_bytes());
    for record in records {
        if record.kind == DeltaRecordKind::Tombstone && !record.vector.is_empty() {
            return Err(DeltaSegmentError::InvalidRecord {
                reason: "tombstone records must not carry vector values",
            });
        }
        let dimension =
            u16::try_from(record.vector.len()).map_err(|_| DeltaSegmentError::InvalidRecord {
                reason: "record dimension exceeds u16",
            })?;
        payload.extend_from_slice(&record.heap_tid.to_le_bytes());
        payload.extend_from_slice(&record.kind.flags().to_le_bytes());
        payload.extend_from_slice(&dimension.to_le_bytes());
        for value in &record.vector {
            payload.extend_from_slice(&value.to_le_bytes());
        }
    }
    payload.resize(payload_len, 0);
    let checksum = fnv1a(FNV_OFFSET_BASIS, &payload);
    payload[24..32].copy_from_slice(&checksum.to_le_bytes());
    Ok(payload)
}

/// Decodes one delta page payload, returning its compaction generation and
/// records.
///
/// # Errors
///
/// Fails closed on every structural violation listed in the module
/// documentation.
pub fn decode_delta_page(payload: &[u8]) -> Result<(u64, Vec<DeltaRecord>), DeltaSegmentError> {
    if payload.len() < DELTA_PAGE_HEADER_BYTES {
        return Err(DeltaSegmentError::Truncated);
    }
    if payload[0..8] != DELTA_PAGE_MAGIC {
        return Err(DeltaSegmentError::BadMagic);
    }
    let version = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
    if version != DELTA_PAGE_VERSION {
        return Err(DeltaSegmentError::UnsupportedVersion { found: version });
    }
    let record_count = u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
    let generation = u64::from_le_bytes([
        payload[16],
        payload[17],
        payload[18],
        payload[19],
        payload[20],
        payload[21],
        payload[22],
        payload[23],
    ]);
    let stored_checksum = u64::from_le_bytes([
        payload[24],
        payload[25],
        payload[26],
        payload[27],
        payload[28],
        payload[29],
        payload[30],
        payload[31],
    ]);
    let mut zeroed = payload.to_vec();
    zeroed[24..32].fill(0);
    if fnv1a(FNV_OFFSET_BASIS, &zeroed) != stored_checksum {
        return Err(DeltaSegmentError::ChecksumMismatch);
    }

    let mut records = Vec::with_capacity(record_count as usize);
    let mut offset = DELTA_PAGE_HEADER_BYTES;
    for _ in 0..record_count {
        let fixed_end =
            offset
                .checked_add(RECORD_FIXED_BYTES)
                .ok_or(DeltaSegmentError::InvalidRecord {
                    reason: "record offset overflows",
                })?;
        if fixed_end > payload.len() {
            return Err(DeltaSegmentError::Truncated);
        }
        let heap_tid = u64::from_le_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
            payload[offset + 4],
            payload[offset + 5],
            payload[offset + 6],
            payload[offset + 7],
        ]);
        let flags = u16::from_le_bytes([payload[offset + 8], payload[offset + 9]]);
        let dimension = usize::from(u16::from_le_bytes([
            payload[offset + 10],
            payload[offset + 11],
        ]));
        let kind = DeltaRecordKind::from_flags(flags).ok_or(DeltaSegmentError::InvalidRecord {
            reason: "unknown record flags",
        })?;
        let vector_end = fixed_end.checked_add(dimension * size_of::<f32>()).ok_or(
            DeltaSegmentError::InvalidRecord {
                reason: "record vector length overflows",
            },
        )?;
        if vector_end > payload.len() {
            return Err(DeltaSegmentError::Truncated);
        }
        let record = match kind {
            DeltaRecordKind::Tombstone => {
                if dimension != 0 {
                    return Err(DeltaSegmentError::InvalidRecord {
                        reason: "tombstone records must not carry vector values",
                    });
                }
                DeltaRecord::tombstone(heap_tid)
            }
            DeltaRecordKind::Live => {
                if dimension == 0 || dimension > DELTA_MAX_DIMENSIONS {
                    return Err(DeltaSegmentError::InvalidRecord {
                        reason: "live delta record dimension is out of range",
                    });
                }
                let mut vector = Vec::with_capacity(dimension);
                for bytes in payload[fixed_end..vector_end].chunks_exact(size_of::<f32>()) {
                    let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    if !value.is_finite() {
                        return Err(DeltaSegmentError::InvalidRecord {
                            reason: "live delta record contains a non-finite value",
                        });
                    }
                    vector.push(value);
                }
                DeltaRecord {
                    heap_tid,
                    kind: DeltaRecordKind::Live,
                    vector,
                }
            }
        };
        records.push(record);
        offset = vector_end;
    }
    if payload[offset..].iter().any(|byte| *byte != 0) {
        return Err(DeltaSegmentError::InvalidRecord {
            reason: "trailing bytes after the declared records",
        });
    }
    Ok((generation, records))
}

/// Encodes one delta record as a standalone item payload (the page-item
/// form used by the access method's WAL-logged append path; the whole-page
/// codec above is the compaction/bulk form).
///
/// # Errors
///
/// Returns [`DeltaSegmentError::InvalidRecord`] when the record violates
/// the structural contract.
pub fn encode_delta_record(record: &DeltaRecord) -> Result<Vec<u8>, DeltaSegmentError> {
    if record.kind == DeltaRecordKind::Tombstone && !record.vector.is_empty() {
        return Err(DeltaSegmentError::InvalidRecord {
            reason: "tombstone records must not carry vector values",
        });
    }
    let dimension =
        u16::try_from(record.vector.len()).map_err(|_| DeltaSegmentError::InvalidRecord {
            reason: "record dimension exceeds u16",
        })?;
    let mut payload = Vec::with_capacity(record.encoded_len());
    payload.extend_from_slice(&record.heap_tid.to_le_bytes());
    payload.extend_from_slice(&record.kind.flags().to_le_bytes());
    payload.extend_from_slice(&dimension.to_le_bytes());
    for value in &record.vector {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    Ok(payload)
}

/// Decodes one delta record from a standalone item payload, applying the
/// same fail-closed structural checks as the page decoder.
///
/// # Errors
///
/// Returns [`DeltaSegmentError::Truncated`] or
/// [`DeltaSegmentError::InvalidRecord`] on any structural violation,
/// including trailing bytes.
pub fn decode_delta_record(payload: &[u8]) -> Result<DeltaRecord, DeltaSegmentError> {
    if payload.len() < RECORD_FIXED_BYTES {
        return Err(DeltaSegmentError::Truncated);
    }
    let heap_tid = u64::from_le_bytes([
        payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
        payload[7],
    ]);
    let flags = u16::from_le_bytes([payload[8], payload[9]]);
    let dimension = usize::from(u16::from_le_bytes([payload[10], payload[11]]));
    let kind = DeltaRecordKind::from_flags(flags).ok_or(DeltaSegmentError::InvalidRecord {
        reason: "unknown record flags",
    })?;
    let expected = RECORD_FIXED_BYTES + dimension * size_of::<f32>();
    if payload.len() != expected {
        return Err(DeltaSegmentError::InvalidRecord {
            reason: "record payload length does not match its dimension",
        });
    }
    match kind {
        DeltaRecordKind::Tombstone => {
            if dimension != 0 {
                return Err(DeltaSegmentError::InvalidRecord {
                    reason: "tombstone records must not carry vector values",
                });
            }
            Ok(DeltaRecord::tombstone(heap_tid))
        }
        DeltaRecordKind::Live => {
            let mut vector = Vec::with_capacity(dimension);
            for bytes in payload[RECORD_FIXED_BYTES..].chunks_exact(size_of::<f32>()) {
                let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if !value.is_finite() {
                    return Err(DeltaSegmentError::InvalidRecord {
                        reason: "live delta record contains a non-finite value",
                    });
                }
                vector.push(value);
            }
            DeltaRecord::live(heap_tid, vector)
        }
    }
}
