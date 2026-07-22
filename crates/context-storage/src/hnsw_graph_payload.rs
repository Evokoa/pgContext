//! Portable HNSW graph payload encoding for rebuildable segment artifacts.
//!
//! The decoder validates dimensions, record counts, node ordering, neighbor
//! identities, and byte bounds before constructing owned Rust values. These
//! payloads are derived acceleration data and never replace PostgreSQL rows as
//! the authoritative source.

use core::{fmt, mem::size_of};

use context_core::DenseVector;

mod quantization;
mod quantized_view;

pub use quantization::{
    HnswGraphQuantization, HnswGraphQuantizationCodebook, PreparedQuantizedQuery,
};
pub(crate) use quantization::{
    QUANTIZATION_NONE, decode_quantization_codebook, encode_quantization_codebook,
    quantization_mode, validate_quantization, validate_quantized_code,
};
pub use quantized_view::{
    QuantizedHnswGraphNodeView, QuantizedHnswGraphView, QuantizedNeighborIter,
};

const HNSW_GRAPH_PAYLOAD_MAGIC: [u8; 8] = *b"PGCTXHNS";
/// Oldest HNSW graph payload version accepted by the decoder.
pub const MIN_READABLE_HNSW_GRAPH_PAYLOAD_VERSION: u32 = 1;
/// Current HNSW graph payload version used for quantized artifacts.
pub const CURRENT_HNSW_GRAPH_PAYLOAD_VERSION: u32 = 2;
const HNSW_GRAPH_PAYLOAD_VERSION_V1: u32 = 1;
const HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1: usize = 24;
const HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2: usize = 40;
const HNSW_GRAPH_RECORD_HEADER_LEN: usize = 16;
const MAX_HNSW_GRAPH_RECORDS: usize = 1_000_000;
type DecodedRecords = (Vec<HnswGraphArtifactRecord>, usize, Vec<Vec<u8>>);

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

/// Version-aware decoded HNSW graph payload.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphPayload {
    version: u32,
    records: Vec<HnswGraphArtifactRecord>,
    quantization: Option<HnswGraphQuantization>,
}

impl HnswGraphPayload {
    /// Returns the decoded payload format version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the full-precision graph records.
    #[must_use]
    pub fn records(&self) -> &[HnswGraphArtifactRecord] {
        &self.records
    }

    /// Returns persisted quantization metadata and codes, when present.
    #[must_use]
    pub const fn quantization(&self) -> Option<&HnswGraphQuantization> {
        self.quantization.as_ref()
    }

    /// Consumes the decoded payload into its graph records.
    #[must_use]
    pub fn into_records(self) -> Vec<HnswGraphArtifactRecord> {
        self.records
    }

    /// Consumes the decoded payload into graph records and quantization data.
    #[must_use]
    pub fn into_parts(self) -> (Vec<HnswGraphArtifactRecord>, Option<HnswGraphQuantization>) {
        (self.records, self.quantization)
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
    /// Quantization metadata or codes violate the v2 payload contract.
    InvalidQuantization(String),
    /// Reserved payload header field is non-zero.
    NonZeroReserved {
        /// Raw reserved value.
        value: u32,
    },
    /// Payload declared zero records.
    EmptyGraph,
    /// Payload declared zero dimensions.
    EmptyVector,
    /// Payload declares more records than the serving policy allows.
    RecordCountLimit {
        /// Declared record count.
        declared: usize,
        /// Maximum accepted record count.
        maximum: usize,
    },
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
            Self::InvalidQuantization(message) => {
                write!(formatter, "invalid HNSW graph quantization: {message}")
            }
            Self::NonZeroReserved { value } => {
                write!(
                    formatter,
                    "HNSW graph payload reserved field is non-zero: {value}"
                )
            }
            Self::EmptyGraph => formatter.write_str("HNSW graph payload has no records"),
            Self::EmptyVector => formatter.write_str("HNSW graph payload has zero dimensions"),
            Self::RecordCountLimit { declared, maximum } => write!(
                formatter,
                "HNSW graph payload record count {declared} exceeds limit {maximum}"
            ),
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
    let capacity =
        hnsw_graph_payload_len(records, dimensions, HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1, 0)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&HNSW_GRAPH_PAYLOAD_MAGIC);
    output.extend_from_slice(&HNSW_GRAPH_PAYLOAD_VERSION_V1.to_le_bytes());
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

/// Encodes a version-2 graph payload with optional persisted quantization.
///
/// Full-precision vectors remain in the artifact for validation and exact
/// recovery. Quantized codes are an additional navigation representation and
/// are bound positionally to the contiguous graph records.
///
/// # Errors
///
/// Returns [`HnswGraphPayloadError`] when graph records, codebook metadata, or
/// per-node codes violate the portable payload contract.
pub fn encode_hnsw_graph_payload_v2(
    records: &[HnswGraphArtifactRecord],
    quantization: Option<&HnswGraphQuantization>,
) -> Result<Vec<u8>, HnswGraphPayloadError> {
    validate_hnsw_graph_records(records)?;
    let dimensions = records[0].vector.dimension();
    let (mode, code_len, codebook_bytes) = match quantization {
        Some(quantization) => {
            validate_quantization(quantization, records.len(), dimensions)?;
            let mode = quantization_mode(quantization.codebook());
            let bytes = encode_quantization_codebook(quantization.codebook())?;
            (mode, quantization.codebook().code_len(), bytes)
        }
        None => (QUANTIZATION_NONE, 0, Vec::new()),
    };
    let capacity = hnsw_graph_payload_len(
        records,
        dimensions,
        HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2
            .checked_add(codebook_bytes.len())
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?,
        code_len,
    )?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(&HNSW_GRAPH_PAYLOAD_MAGIC);
    output.extend_from_slice(&CURRENT_HNSW_GRAPH_PAYLOAD_VERSION.to_le_bytes());
    output.extend_from_slice(&usize_to_u32(records.len(), 0)?.to_le_bytes());
    output.extend_from_slice(&usize_to_u32(dimensions, 0)?.to_le_bytes());
    output.extend_from_slice(&mode.to_le_bytes());
    output.extend_from_slice(&usize_to_u32(code_len, 0)?.to_le_bytes());
    output.extend_from_slice(&usize_to_u32(codebook_bytes.len(), 0)?.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&codebook_bytes);

    for (record_index, record) in records.iter().enumerate() {
        encode_record(&mut output, record)?;
        if let Some(quantization) = quantization {
            output.extend_from_slice(&quantization.codes()[record_index]);
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
    decode_hnsw_graph_payload_versioned(payload).map(HnswGraphPayload::into_records)
}

/// Decodes a version-1 or version-2 HNSW graph payload.
///
/// # Errors
///
/// Returns [`HnswGraphPayloadError`] when the payload is malformed or its
/// quantization metadata cannot be safely reconstructed.
pub fn decode_hnsw_graph_payload_versioned(
    payload: &[u8],
) -> Result<HnswGraphPayload, HnswGraphPayloadError> {
    if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1 {
        return Err(HnswGraphPayloadError::TruncatedHeader {
            actual: payload.len(),
            minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
        });
    }
    if payload[0..8] != HNSW_GRAPH_PAYLOAD_MAGIC {
        return Err(HnswGraphPayloadError::BadMagic);
    }
    let version = read_u32(payload, 8);
    match version {
        HNSW_GRAPH_PAYLOAD_VERSION_V1 => decode_v1_payload(payload),
        CURRENT_HNSW_GRAPH_PAYLOAD_VERSION => decode_v2_payload(payload),
        _ => Err(HnswGraphPayloadError::UnsupportedVersion { version }),
    }
}

fn decode_v1_payload(payload: &[u8]) -> Result<HnswGraphPayload, HnswGraphPayloadError> {
    let (record_count, dimensions, offset) = decode_v1_header(payload)?;
    let (records, offset, _) = decode_records(payload, record_count, dimensions, offset, 0)?;
    require_no_trailing_bytes(payload, offset)?;
    validate_hnsw_graph_records(&records)?;
    Ok(HnswGraphPayload {
        version: HNSW_GRAPH_PAYLOAD_VERSION_V1,
        records,
        quantization: None,
    })
}

fn decode_v2_payload(payload: &[u8]) -> Result<HnswGraphPayload, HnswGraphPayloadError> {
    if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2 {
        return Err(HnswGraphPayloadError::TruncatedHeader {
            actual: payload.len(),
            minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2,
        });
    }
    let record_count = require_nonzero_u32(read_u32(payload, 12), true)? as usize;
    let dimensions = require_nonzero_u32(read_u32(payload, 16), false)? as usize;
    let mode = read_u32(payload, 20);
    let code_len = read_u32(payload, 24) as usize;
    let codebook_len = read_u32(payload, 28) as usize;
    let reserved = read_u32(payload, 32);
    if reserved != 0 {
        return Err(HnswGraphPayloadError::NonZeroReserved { value: reserved });
    }
    let reserved = read_u32(payload, 36);
    if reserved != 0 {
        return Err(HnswGraphPayloadError::NonZeroReserved { value: reserved });
    }
    let codebook_end = HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2
        .checked_add(codebook_len)
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    if payload.len() < codebook_end {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "truncated codebook: expected {codebook_len} bytes, got {}",
            payload
                .len()
                .saturating_sub(HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2)
        )));
    }
    let codebook = decode_quantization_codebook(
        mode,
        dimensions,
        code_len,
        &payload[HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2..codebook_end],
    )?;
    let (records, offset, codes) =
        decode_records(payload, record_count, dimensions, codebook_end, code_len)?;
    require_no_trailing_bytes(payload, offset)?;
    validate_hnsw_graph_records(&records)?;
    let quantization = codebook.map(|codebook| HnswGraphQuantization::new(codebook, codes));
    if let Some(quantization) = &quantization {
        validate_quantization(quantization, record_count, dimensions)?;
    }
    Ok(HnswGraphPayload {
        version: CURRENT_HNSW_GRAPH_PAYLOAD_VERSION,
        records,
        quantization,
    })
}

fn decode_records(
    payload: &[u8],
    record_count: usize,
    dimensions: usize,
    mut offset: usize,
    code_len: usize,
) -> Result<DecodedRecords, HnswGraphPayloadError> {
    let vector_bytes = dimensions
        .checked_mul(size_of_f32())
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    if record_count > MAX_HNSW_GRAPH_RECORDS {
        return Err(HnswGraphPayloadError::RecordCountLimit {
            declared: record_count,
            maximum: MAX_HNSW_GRAPH_RECORDS,
        });
    }
    let minimum_record_bytes = HNSW_GRAPH_RECORD_HEADER_LEN
        .checked_add(vector_bytes)
        .and_then(|bytes| bytes.checked_add(code_len))
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    let minimum_payload_bytes = record_count
        .checked_mul(minimum_record_bytes)
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    let available = payload.len().saturating_sub(offset);
    if available < minimum_payload_bytes {
        return Err(HnswGraphPayloadError::TruncatedRecord {
            record_index: 0,
            expected: minimum_payload_bytes,
            actual: available,
        });
    }
    let mut records = Vec::with_capacity(record_count);
    let mut codes = Vec::with_capacity(if code_len == 0 { 0 } else { record_count });

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
        require_payload_bytes(payload, offset, code_len, record_index)?;
        if code_len != 0 {
            codes.push(payload[offset..offset + code_len].to_vec());
        }
        offset += code_len;
    }
    Ok((records, offset, codes))
}

fn decode_v1_header(payload: &[u8]) -> Result<(usize, usize, usize), HnswGraphPayloadError> {
    if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1 {
        return Err(HnswGraphPayloadError::TruncatedHeader {
            actual: payload.len(),
            minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
        });
    }
    if payload[0..8] != HNSW_GRAPH_PAYLOAD_MAGIC {
        return Err(HnswGraphPayloadError::BadMagic);
    }
    let version = read_u32(payload, 8);
    if version != HNSW_GRAPH_PAYLOAD_VERSION_V1 {
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
        HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
    ))
}

fn encode_record(
    output: &mut Vec<u8>,
    record: &HnswGraphArtifactRecord,
) -> Result<(), HnswGraphPayloadError> {
    output.extend_from_slice(&record.node_id.to_le_bytes());
    output.extend_from_slice(
        &usize_to_u32(record.base_neighbors.len(), record.node_id as usize)?.to_le_bytes(),
    );
    output.extend_from_slice(&record.point_id.to_le_bytes());
    for value in record.vector.as_slice() {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for neighbor in &record.base_neighbors {
        output.extend_from_slice(&neighbor.to_le_bytes());
    }
    Ok(())
}

fn require_nonzero_u32(value: u32, record_count: bool) -> Result<u32, HnswGraphPayloadError> {
    if value != 0 {
        Ok(value)
    } else if record_count {
        Err(HnswGraphPayloadError::EmptyGraph)
    } else {
        Err(HnswGraphPayloadError::EmptyVector)
    }
}

fn require_no_trailing_bytes(payload: &[u8], offset: usize) -> Result<(), HnswGraphPayloadError> {
    if offset == payload.len() {
        Ok(())
    } else {
        Err(HnswGraphPayloadError::TrailingBytes {
            bytes: payload.len() - offset,
        })
    }
}

fn usize_to_u32(value: usize, record_index: usize) -> Result<u32, HnswGraphPayloadError> {
    u32::try_from(value).map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index })
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
    prefix_len: usize,
    code_len: usize,
) -> Result<usize, HnswGraphPayloadError> {
    let vector_bytes = dimensions
        .checked_mul(size_of_f32())
        .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
    records.iter().try_fold(prefix_len, |total, record| {
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
            .and_then(|bytes| bytes.checked_add(code_len))
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
            expected: expected.saturating_sub(HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1),
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

fn read_u16(input: &[u8], offset: usize) -> u16 {
    let mut bytes = [0; 2];
    bytes.copy_from_slice(&input[offset..offset + 2]);
    u16::from_le_bytes(bytes)
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
        CURRENT_HNSW_GRAPH_PAYLOAD_VERSION, HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
        HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2, HnswGraphArtifactRecord, HnswGraphPayloadError,
        HnswGraphQuantization, HnswGraphQuantizationCodebook, decode_hnsw_graph_payload,
        decode_hnsw_graph_payload_versioned, encode_hnsw_graph_payload,
        encode_hnsw_graph_payload_v2,
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
        assert_eq!(
            decode_hnsw_graph_payload_versioned(view.payload())?.version(),
            1
        );
        Ok(())
    }

    #[test]
    fn quantized_v2_payload_round_trips_scalar_codes() -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![
            hnsw_record(0, 101, &[-1.0, 0.5], &[1])?,
            hnsw_record(1, 102, &[1.0, -0.5], &[0])?,
        ];
        let quantization = HnswGraphQuantization::new(
            HnswGraphQuantizationCodebook::Scalar {
                dimensions: 2,
                minimum: -1.0,
                maximum: 1.0,
                levels: 256,
            },
            vec![vec![0, 191], vec![255, 64]],
        );

        let encoded = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
        let decoded = decode_hnsw_graph_payload_versioned(&encoded)?;

        assert_eq!(decoded.version(), CURRENT_HNSW_GRAPH_PAYLOAD_VERSION);
        assert_eq!(decoded.records(), records);
        assert_eq!(decoded.quantization(), Some(&quantization));
        assert_eq!(decode_hnsw_graph_payload(&encoded)?, records);
        Ok(())
    }

    #[test]
    fn quantized_v2_payload_round_trips_product_codebooks() -> Result<(), Box<dyn std::error::Error>>
    {
        let records = vec![hnsw_record(0, 101, &[0.0, 1.0], &[])?];
        let quantization = HnswGraphQuantization::new(
            HnswGraphQuantizationCodebook::Product {
                dimensions: 2,
                subvector_dimensions: 1,
                codebooks: vec![
                    vec![DenseVector::new(vec![-1.0])?, DenseVector::new(vec![1.0])?],
                    vec![DenseVector::new(vec![0.0])?, DenseVector::new(vec![2.0])?],
                ],
            },
            vec![vec![1, 0]],
        );

        let encoded = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
        assert_eq!(
            decode_hnsw_graph_payload_versioned(&encoded)?.quantization(),
            Some(&quantization)
        );
        Ok(())
    }

    #[test]
    fn quantized_v2_payload_rejects_binary_padding_corruption()
    -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![hnsw_record(0, 101, &[1.0; 9], &[])?];
        let quantization = HnswGraphQuantization::new(
            HnswGraphQuantizationCodebook::Binary { dimensions: 9 },
            vec![vec![0xff, 0x01]],
        );
        let mut encoded = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
        let code_byte = encoded
            .last_mut()
            .ok_or_else(|| std::io::Error::other("encoded graph has no code bytes"))?;
        *code_byte |= 0x80;

        assert!(matches!(
            decode_hnsw_graph_payload_versioned(&encoded),
            Err(HnswGraphPayloadError::InvalidQuantization(message))
                if message.contains("padding")
        ));
        Ok(())
    }

    #[test]
    fn quantized_v2_payload_rejects_truncated_codebook() -> Result<(), Box<dyn std::error::Error>> {
        let records = vec![hnsw_record(0, 101, &[1.0], &[])?];
        let quantization = HnswGraphQuantization::new(
            HnswGraphQuantizationCodebook::Binary { dimensions: 1 },
            vec![vec![1]],
        );
        let encoded = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
        let truncated = &encoded[..HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2 + 1];

        assert!(matches!(
            decode_hnsw_graph_payload_versioned(truncated),
            Err(HnswGraphPayloadError::InvalidQuantization(message))
                if message.contains("truncated codebook")
        ));
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
                actual: 23,
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
                actual: 24,
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
                minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
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
