//! Portable HNSW graph payload encoding for rebuildable segment artifacts.
//!
//! The decoder validates dimensions, record counts, node ordering, neighbor
//! identities, and byte bounds before constructing owned Rust values. These
//! payloads are derived acceleration data and never replace PostgreSQL rows as
//! the authoritative source.

use core::{fmt, mem::size_of};

use context_core::DenseVector;

const HNSW_GRAPH_PAYLOAD_MAGIC: [u8; 8] = *b"PGCTXHNS";
const HNSW_GRAPH_PAYLOAD_VERSION: u32 = 1;
const HNSW_GRAPH_PAYLOAD_HEADER_LEN: usize = 24;
const HNSW_GRAPH_RECORD_HEADER_LEN: usize = 16;

/// One portable HNSW graph record stored inside a segment payload.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphArtifactRecord {
    node_id: u32,
    point_id: u64,
    vector: DenseVector,
    base_neighbors: Vec<u32>,
}

impl HnswGraphArtifactRecord {
    /// Creates a portable HNSW graph artifact record.
    #[must_use]
    pub fn new(node_id: u32, point_id: u64, vector: DenseVector, base_neighbors: Vec<u32>) -> Self {
        Self {
            node_id,
            point_id,
            vector,
            base_neighbors,
        }
    }

    /// Returns the zero-based HNSW node id.
    #[must_use]
    pub const fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Returns the authoritative pgContext point id.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }

    /// Returns the dense vector for exact rerank and graph reconstruction.
    #[must_use]
    pub const fn vector(&self) -> &DenseVector {
        &self.vector
    }

    /// Returns base-layer neighbor node ids.
    #[must_use]
    pub fn base_neighbors(&self) -> &[u32] {
        &self.base_neighbors
    }

    /// Consumes the record into graph-reconstruction fields.
    #[must_use]
    pub fn into_parts(self) -> (u32, u64, DenseVector, Vec<u32>) {
        (
            self.node_id,
            self.point_id,
            self.vector,
            self.base_neighbors,
        )
    }
}

/// Stable HNSW graph artifact payload validation error.
#[derive(Debug, Clone, PartialEq)]
pub enum HnswGraphPayloadError {
    /// Payload is shorter than the fixed graph payload header.
    TruncatedHeader {
        /// Actual payload byte length.
        actual: usize,
        /// Minimum header length.
        minimum: usize,
    },
    /// Payload magic bytes do not match HNSW graph artifacts.
    BadMagic,
    /// Payload version is not supported.
    UnsupportedVersion {
        /// Raw payload version.
        version: u32,
    },
    /// Reserved payload header field is non-zero.
    NonZeroReserved {
        /// Raw reserved value.
        value: u32,
    },
    /// Payload declared zero records.
    EmptyGraph,
    /// Payload declared zero dimensions.
    EmptyVector,
    /// Payload ended before a complete record could be read.
    TruncatedRecord {
        /// Zero-based record index being decoded.
        record_index: usize,
        /// Expected bytes from the record start.
        expected: usize,
        /// Actual bytes available from the record start.
        actual: usize,
    },
    /// A record's encoded size overflows addressable memory.
    RecordSizeOverflow {
        /// Zero-based record index being decoded.
        record_index: usize,
    },
    /// Record node ids are not ordered contiguously from zero.
    NonContiguousNodeId {
        /// Expected node id for the record position.
        expected: u32,
        /// Actual node id encoded in the record.
        actual: u32,
    },
    /// A neighbor references a node outside the decoded graph.
    NeighborOutOfRange {
        /// Node containing the invalid neighbor.
        node_id: u32,
        /// Invalid neighbor node id.
        neighbor_id: u32,
        /// Number of records in the graph.
        record_count: u32,
    },
    /// Dense vector validation rejected decoded values.
    InvalidVector(String),
    /// Payload contains bytes after all declared records.
    TrailingBytes {
        /// Number of trailing bytes.
        bytes: usize,
    },
}

impl fmt::Display for HnswGraphPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { actual, minimum } => {
                write!(
                    formatter,
                    "truncated HNSW graph payload header: {actual} < {minimum}"
                )
            }
            Self::BadMagic => formatter.write_str("invalid HNSW graph payload magic"),
            Self::UnsupportedVersion { version } => {
                write!(
                    formatter,
                    "unsupported HNSW graph payload version: {version}"
                )
            }
            Self::NonZeroReserved { value } => {
                write!(
                    formatter,
                    "HNSW graph payload reserved field is non-zero: {value}"
                )
            }
            Self::EmptyGraph => formatter.write_str("HNSW graph payload has no records"),
            Self::EmptyVector => formatter.write_str("HNSW graph payload has zero dimensions"),
            Self::TruncatedRecord {
                record_index,
                expected,
                actual,
            } => write!(
                formatter,
                "truncated HNSW graph record {record_index}: {actual} < {expected}"
            ),
            Self::RecordSizeOverflow { record_index } => {
                write!(
                    formatter,
                    "HNSW graph record {record_index} size overflows usize"
                )
            }
            Self::NonContiguousNodeId { expected, actual } => write!(
                formatter,
                "HNSW graph record node id is not contiguous: expected {expected}, got {actual}"
            ),
            Self::NeighborOutOfRange {
                node_id,
                neighbor_id,
                record_count,
            } => write!(
                formatter,
                "HNSW graph record {node_id} has out-of-range neighbor {neighbor_id}; record count is {record_count}"
            ),
            Self::InvalidVector(message) => {
                write!(formatter, "invalid HNSW graph vector: {message}")
            }
            Self::TrailingBytes { bytes } => {
                write!(formatter, "HNSW graph payload has {bytes} trailing bytes")
            }
        }
    }
}

impl std::error::Error for HnswGraphPayloadError {}

/// Encodes a portable HNSW graph payload for a rebuildable segment.
///
/// The payload is independent of PostgreSQL page layout and native endianness:
/// all integers and `f32` values are little-endian, records are ordered by
/// contiguous node ids, and neighbor ids must reference records in the payload.
///
/// # Errors
///
/// Returns [`HnswGraphPayloadError`] when records are empty, malformed, not
/// contiguous, or contain out-of-range neighbor ids.
pub fn encode_hnsw_graph_payload(
    records: &[HnswGraphArtifactRecord],
) -> Result<Vec<u8>, HnswGraphPayloadError> {
    validate_hnsw_graph_records(records)?;
    let dimensions = records[0].vector.dimension();
    let capacity = hnsw_graph_payload_len(records, dimensions)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&HNSW_GRAPH_PAYLOAD_MAGIC);
    output.extend_from_slice(&HNSW_GRAPH_PAYLOAD_VERSION.to_le_bytes());
    output.extend_from_slice(
        &u32::try_from(records.len())
            .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?
            .to_le_bytes(),
    );
    output.extend_from_slice(
        &u32::try_from(dimensions)
            .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?
            .to_le_bytes(),
    );
    output.extend_from_slice(&0_u32.to_le_bytes());

    for record in records {
        output.extend_from_slice(&record.node_id.to_le_bytes());
        output.extend_from_slice(
            &u32::try_from(record.base_neighbors.len())
                .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow {
                    record_index: record.node_id as usize,
                })?
                .to_le_bytes(),
        );
        output.extend_from_slice(&record.point_id.to_le_bytes());
        for value in record.vector.as_slice() {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for neighbor in &record.base_neighbors {
            output.extend_from_slice(&neighbor.to_le_bytes());
        }
    }

    Ok(output)
}

/// Decodes and validates a portable HNSW graph segment payload.
///
/// # Errors
///
/// Returns [`HnswGraphPayloadError`] when the payload is malformed or violates
/// graph invariants required for serving.
pub fn decode_hnsw_graph_payload(
    payload: &[u8],
) -> Result<Vec<HnswGraphArtifactRecord>, HnswGraphPayloadError> {
    let (record_count, dimensions, mut offset) = decode_hnsw_graph_payload_header(payload)?;
    let vector_bytes = dimensions
        .checked_mul(size_of_f32())
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    let mut records = Vec::with_capacity(record_count);

    for record_index in 0..record_count {
        require_payload_bytes(payload, offset, HNSW_GRAPH_RECORD_HEADER_LEN, record_index)?;
        let node_id = read_u32(payload, offset);
        let neighbor_count = read_u32(payload, offset + 4) as usize;
        let point_id = read_u64(payload, offset + 8);
        let expected_node_id = u32::try_from(record_index)
            .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index })?;
        if node_id != expected_node_id {
            return Err(HnswGraphPayloadError::NonContiguousNodeId {
                expected: expected_node_id,
                actual: node_id,
            });
        }
        offset += HNSW_GRAPH_RECORD_HEADER_LEN;

        require_payload_bytes(payload, offset, vector_bytes, record_index)?;
        let values = (0..dimensions)
            .map(|index| read_f32(payload, offset + index * size_of_f32()))
            .collect::<Vec<_>>();
        offset += vector_bytes;

        let neighbor_bytes = neighbor_count
            .checked_mul(size_of_u32())
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index })?;
        require_payload_bytes(payload, offset, neighbor_bytes, record_index)?;
        let base_neighbors = (0..neighbor_count)
            .map(|index| read_u32(payload, offset + index * size_of_u32()))
            .collect::<Vec<_>>();
        offset += neighbor_bytes;

        let vector = DenseVector::new(values)
            .map_err(|error| HnswGraphPayloadError::InvalidVector(error.to_string()))?;
        records.push(HnswGraphArtifactRecord::new(
            node_id,
            point_id,
            vector,
            base_neighbors,
        ));
    }

    if offset != payload.len() {
        return Err(HnswGraphPayloadError::TrailingBytes {
            bytes: payload.len() - offset,
        });
    }

    validate_hnsw_graph_records(&records)?;
    Ok(records)
}

fn decode_hnsw_graph_payload_header(
    payload: &[u8],
) -> Result<(usize, usize, usize), HnswGraphPayloadError> {
    if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN {
        return Err(HnswGraphPayloadError::TruncatedHeader {
            actual: payload.len(),
            minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN,
        });
    }
    if payload[0..8] != HNSW_GRAPH_PAYLOAD_MAGIC {
        return Err(HnswGraphPayloadError::BadMagic);
    }
    let version = read_u32(payload, 8);
    if version != HNSW_GRAPH_PAYLOAD_VERSION {
        return Err(HnswGraphPayloadError::UnsupportedVersion { version });
    }
    let record_count = read_u32(payload, 12);
    if record_count == 0 {
        return Err(HnswGraphPayloadError::EmptyGraph);
    }
    let dimensions = read_u32(payload, 16);
    if dimensions == 0 {
        return Err(HnswGraphPayloadError::EmptyVector);
    }
    let reserved = read_u32(payload, 20);
    if reserved != 0 {
        return Err(HnswGraphPayloadError::NonZeroReserved { value: reserved });
    }

    Ok((
        record_count as usize,
        dimensions as usize,
        HNSW_GRAPH_PAYLOAD_HEADER_LEN,
    ))
}

fn validate_hnsw_graph_records(
    records: &[HnswGraphArtifactRecord],
) -> Result<(), HnswGraphPayloadError> {
    if records.is_empty() {
        return Err(HnswGraphPayloadError::EmptyGraph);
    }
    let dimensions = records[0].vector.dimension();
    if dimensions == 0 {
        return Err(HnswGraphPayloadError::EmptyVector);
    }
    let record_count = u32::try_from(records.len())
        .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    for (index, record) in records.iter().enumerate() {
        let expected =
            u32::try_from(index).map_err(|_| HnswGraphPayloadError::RecordSizeOverflow {
                record_index: index,
            })?;
        if record.node_id != expected {
            return Err(HnswGraphPayloadError::NonContiguousNodeId {
                expected,
                actual: record.node_id,
            });
        }
        if record.vector.dimension() != dimensions {
            return Err(HnswGraphPayloadError::InvalidVector(format!(
                "record {} has {} dimensions; expected {dimensions}",
                record.node_id,
                record.vector.dimension()
            )));
        }
        for neighbor_id in &record.base_neighbors {
            if *neighbor_id >= record_count {
                return Err(HnswGraphPayloadError::NeighborOutOfRange {
                    node_id: record.node_id,
                    neighbor_id: *neighbor_id,
                    record_count,
                });
            }
        }
    }
    Ok(())
}

fn hnsw_graph_payload_len(
    records: &[HnswGraphArtifactRecord],
    dimensions: usize,
) -> Result<usize, HnswGraphPayloadError> {
    let vector_bytes = dimensions
        .checked_mul(size_of_f32())
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    records
        .iter()
        .try_fold(HNSW_GRAPH_PAYLOAD_HEADER_LEN, |total, record| {
            let neighbor_bytes = record
                .base_neighbors
                .len()
                .checked_mul(size_of_u32())
                .ok_or(HnswGraphPayloadError::RecordSizeOverflow {
                    record_index: record.node_id as usize,
                })?;
            total
                .checked_add(HNSW_GRAPH_RECORD_HEADER_LEN)
                .and_then(|bytes| bytes.checked_add(vector_bytes))
                .and_then(|bytes| bytes.checked_add(neighbor_bytes))
                .ok_or(HnswGraphPayloadError::RecordSizeOverflow {
                    record_index: record.node_id as usize,
                })
        })
}

fn require_payload_bytes(
    payload: &[u8],
    offset: usize,
    length: usize,
    record_index: usize,
) -> Result<(), HnswGraphPayloadError> {
    let expected = offset
        .checked_add(length)
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index })?;
    if payload.len() < expected {
        return Err(HnswGraphPayloadError::TruncatedRecord {
            record_index,
            expected: expected.saturating_sub(HNSW_GRAPH_PAYLOAD_HEADER_LEN),
            actual: payload.len().saturating_sub(offset),
        });
    }
    Ok(())
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&input[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&input[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn read_f32(input: &[u8], offset: usize) -> f32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&input[offset..offset + 4]);
    f32::from_le_bytes(bytes)
}

const fn size_of_f32() -> usize {
    size_of::<f32>()
}

const fn size_of_u32() -> usize {
    size_of::<u32>()
}

#[cfg(test)]
mod tests {
    use context_core::DenseVector;

    use super::{
        HNSW_GRAPH_PAYLOAD_HEADER_LEN, HnswGraphArtifactRecord, HnswGraphPayloadError,
        decode_hnsw_graph_payload, encode_hnsw_graph_payload,
    };
    use crate::{SegmentKind, encode_segment, validate_mmap_segment};

    #[test]
    fn hnsw_graph_payload_round_trips_portable_records() -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![
            hnsw_record(0, 101, &[0.0, 1.0], &[1])?,
            hnsw_record(1, 102, &[1.0, 0.0], &[0, 2])?,
            hnsw_record(2, 103, &[0.5, 0.5], &[1])?,
        ];

        let payload = encode_hnsw_graph_payload(&records)?;
        let segment = encode_segment(SegmentKind::HnswGraph, &payload)?;
        let view = validate_mmap_segment(&segment)?;
        let decoded = decode_hnsw_graph_payload(view.payload())?;

        assert_eq!(decoded, records);
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_truncated_record() -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![hnsw_record(0, 101, &[0.0, 1.0], &[])?];
        let mut payload = encode_hnsw_graph_payload(&records)?;
        payload.pop();

        assert_eq!(
            decode_hnsw_graph_payload(&payload),
            Err(HnswGraphPayloadError::TruncatedRecord {
                record_index: 0,
                expected: 24,
                actual: 7,
            })
        );
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_bad_dimension_byte_length()
    -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![hnsw_record(0, 101, &[0.0, 1.0], &[])?];
        let mut payload = encode_hnsw_graph_payload(&records)?;
        payload[16..20].copy_from_slice(&3_u32.to_le_bytes());

        assert_eq!(
            decode_hnsw_graph_payload(&payload),
            Err(HnswGraphPayloadError::TruncatedRecord {
                record_index: 0,
                expected: 28,
                actual: 8,
            })
        );
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_out_of_range_neighbors() -> Result<(), Box<dyn std::error::Error>>
    {
        let records = vec![hnsw_record(0, 101, &[0.0, 1.0], &[1])?];

        assert_eq!(
            encode_hnsw_graph_payload(&records),
            Err(HnswGraphPayloadError::NeighborOutOfRange {
                node_id: 0,
                neighbor_id: 1,
                record_count: 1,
            })
        );
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_non_contiguous_node_ids() -> Result<(), Box<dyn std::error::Error>>
    {
        let records = vec![
            hnsw_record(0, 101, &[0.0, 1.0], &[])?,
            hnsw_record(2, 102, &[1.0, 0.0], &[])?,
        ];

        assert_eq!(
            encode_hnsw_graph_payload(&records),
            Err(HnswGraphPayloadError::NonContiguousNodeId {
                expected: 1,
                actual: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_bad_magic() -> Result<(), Box<dyn std::error::Error>> {
        let mut payload = encode_hnsw_graph_payload(&[hnsw_record(0, 101, &[0.0], &[])?])?;
        payload[0] = b'X';

        assert_eq!(
            decode_hnsw_graph_payload(&payload),
            Err(HnswGraphPayloadError::BadMagic)
        );
        Ok(())
    }

    #[test]
    fn hnsw_graph_payload_rejects_truncated_header() {
        assert_eq!(
            decode_hnsw_graph_payload(&[]),
            Err(HnswGraphPayloadError::TruncatedHeader {
                actual: 0,
                minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN,
            })
        );
    }

    fn hnsw_record(
        node_id: u32,
        point_id: u64,
        values: &[f32],
        base_neighbors: &[u32],
    ) -> context_core::Result<HnswGraphArtifactRecord> {
        Ok(HnswGraphArtifactRecord::new(
            node_id,
            point_id,
            DenseVector::new(values.to_vec())?,
            base_neighbors.to_vec(),
        ))
    }
}
