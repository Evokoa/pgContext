//! Borrowed, allocation-bounded view over quantized HNSW graph payloads.

use core::iter::FusedIterator;

use context_core::policy::MAX_VECTOR_DIMENSIONS;

use super::{
    CURRENT_HNSW_GRAPH_PAYLOAD_VERSION, HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
    HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2, HNSW_GRAPH_PAYLOAD_MAGIC, HNSW_GRAPH_RECORD_HEADER_LEN,
    HnswGraphPayloadError, MAX_HNSW_GRAPH_RECORDS, QUANTIZATION_NONE, decode_quantization_codebook,
    read_f32, read_u32, read_u64, require_no_trailing_bytes, require_payload_bytes, size_of_f32,
    size_of_u32, validate_quantized_code,
};
use super::{HNSW_GRAPH_PAYLOAD_VERSION_V1, HnswGraphQuantizationCodebook};

#[derive(Debug, Clone, Copy)]
struct NodeLocation {
    point_id: u64,
    neighbors_offset: usize,
    neighbor_count: usize,
    code_offset: usize,
}

/// Validated borrowed view of a quantized HNSW base-layer node.
#[derive(Debug, Clone, Copy)]
pub struct QuantizedHnswGraphNodeView<'a> {
    point_id: u64,
    neighbors: &'a [u8],
    code: &'a [u8],
}

impl<'a> QuantizedHnswGraphNodeView<'a> {
    /// Returns the authoritative pgContext point id.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Returns the borrowed encoded navigation bytes.
    #[must_use]
    pub const fn code(self) -> &'a [u8] {
        self.code
    }

    /// Iterates base-layer neighbor node ids without allocating a list.
    #[must_use]
    pub const fn neighbors(self) -> QuantizedNeighborIter<'a> {
        QuantizedNeighborIter {
            bytes: self.neighbors,
            offset: 0,
        }
    }
}

/// Exact-size iterator over little-endian neighbor ids in a borrowed payload.
#[derive(Debug, Clone)]
pub struct QuantizedNeighborIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Iterator for QuantizedNeighborIter<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.bytes.len() {
            return None;
        }
        let value = read_u32(self.bytes, self.offset);
        self.offset += size_of_u32();
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.bytes.len() - self.offset) / size_of_u32();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for QuantizedNeighborIter<'_> {}
impl FusedIterator for QuantizedNeighborIter<'_> {}

/// Safe borrowed view over a version-2 quantized HNSW graph payload.
///
/// Attachment validates the complete payload and allocates only one compact
/// location record per node. Full vectors, codes, and neighbor lists remain in
/// the caller-owned payload bytes.
#[derive(Debug)]
pub struct QuantizedHnswGraphView<'a> {
    payload: &'a [u8],
    dimensions: usize,
    code_len: usize,
    codebook: HnswGraphQuantizationCodebook,
    nodes: Vec<NodeLocation>,
}

impl<'a> QuantizedHnswGraphView<'a> {
    /// Attaches to a validated quantized payload.
    ///
    /// Version-1 and unquantized version-2 payloads return `Ok(None)` so callers
    /// can use their compatibility decoder.
    ///
    /// # Errors
    ///
    /// Returns [`HnswGraphPayloadError`] when a quantized v2 payload is
    /// truncated, corrupt, oversized, or internally inconsistent.
    pub fn attach(payload: &'a [u8]) -> Result<Option<Self>, HnswGraphPayloadError> {
        if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1 {
            return Err(HnswGraphPayloadError::TruncatedHeader {
                actual: payload.len(),
                minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1,
            });
        }
        if payload[0..8] != HNSW_GRAPH_PAYLOAD_MAGIC {
            return Err(HnswGraphPayloadError::BadMagic);
        }
        match read_u32(payload, 8) {
            HNSW_GRAPH_PAYLOAD_VERSION_V1 => return Ok(None),
            CURRENT_HNSW_GRAPH_PAYLOAD_VERSION => {}
            version => return Err(HnswGraphPayloadError::UnsupportedVersion { version }),
        }
        if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2 {
            return Err(HnswGraphPayloadError::TruncatedHeader {
                actual: payload.len(),
                minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_V2,
            });
        }
        let record_count_u32 = read_u32(payload, 12);
        let record_count = record_count_u32 as usize;
        let dimensions = read_u32(payload, 16) as usize;
        let mode = read_u32(payload, 20);
        if mode == QUANTIZATION_NONE {
            return Ok(None);
        }
        if record_count == 0 {
            return Err(HnswGraphPayloadError::EmptyGraph);
        }
        if dimensions == 0 {
            return Err(HnswGraphPayloadError::EmptyVector);
        }
        if dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(HnswGraphPayloadError::InvalidVector(format!(
                "dense vector dimensions exceeds limit {MAX_VECTOR_DIMENSIONS}: {dimensions}"
            )));
        }
        if record_count > MAX_HNSW_GRAPH_RECORDS {
            return Err(HnswGraphPayloadError::RecordCountLimit {
                declared: record_count,
                maximum: MAX_HNSW_GRAPH_RECORDS,
            });
        }
        for offset in [32, 36] {
            let value = read_u32(payload, offset);
            if value != 0 {
                return Err(HnswGraphPayloadError::NonZeroReserved { value });
            }
        }

        let code_len = read_u32(payload, 24) as usize;
        let codebook_len = read_u32(payload, 28) as usize;
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
        )?
        .ok_or_else(|| {
            HnswGraphPayloadError::InvalidQuantization(
                "quantized payload has no codebook".to_owned(),
            )
        })?;

        let vector_bytes = dimensions
            .checked_mul(size_of_f32())
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let minimum_record_bytes = HNSW_GRAPH_RECORD_HEADER_LEN
            .checked_add(vector_bytes)
            .and_then(|bytes| bytes.checked_add(code_len))
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let minimum_payload_bytes = record_count
            .checked_mul(minimum_record_bytes)
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let available = payload.len().saturating_sub(codebook_end);
        if available < minimum_payload_bytes {
            return Err(HnswGraphPayloadError::TruncatedRecord {
                record_index: 0,
                expected: minimum_payload_bytes,
                actual: available,
            });
        }

        let mut nodes = Vec::with_capacity(record_count);
        let mut offset = codebook_end;
        for record_index in 0..record_count {
            require_payload_bytes(payload, offset, HNSW_GRAPH_RECORD_HEADER_LEN, record_index)?;
            let node_id = read_u32(payload, offset);
            let expected = u32::try_from(record_index)
                .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index })?;
            if node_id != expected {
                return Err(HnswGraphPayloadError::NonContiguousNodeId {
                    expected,
                    actual: node_id,
                });
            }
            let neighbor_count = read_u32(payload, offset + 4) as usize;
            let point_id = read_u64(payload, offset + 8);
            offset += HNSW_GRAPH_RECORD_HEADER_LEN;

            require_payload_bytes(payload, offset, vector_bytes, record_index)?;
            for dimension in 0..dimensions {
                let value = read_f32(payload, offset + dimension * size_of_f32());
                if !value.is_finite() {
                    return Err(HnswGraphPayloadError::InvalidVector(format!(
                        "record {node_id} value at dimension {dimension} is not finite: {value}"
                    )));
                }
            }
            offset += vector_bytes;

            let neighbor_bytes = neighbor_count
                .checked_mul(size_of_u32())
                .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index })?;
            require_payload_bytes(payload, offset, neighbor_bytes, record_index)?;
            let neighbors_offset = offset;
            for neighbor in (QuantizedNeighborIter {
                bytes: &payload[offset..offset + neighbor_bytes],
                offset: 0,
            }) {
                if neighbor >= record_count_u32 {
                    return Err(HnswGraphPayloadError::NeighborOutOfRange {
                        node_id,
                        neighbor_id: neighbor,
                        record_count: record_count_u32,
                    });
                }
            }
            offset += neighbor_bytes;

            require_payload_bytes(payload, offset, code_len, record_index)?;
            validate_quantized_code(&codebook, record_index, &payload[offset..offset + code_len])?;
            nodes.push(NodeLocation {
                point_id,
                neighbors_offset,
                neighbor_count,
                code_offset: offset,
            });
            offset += code_len;
        }
        require_no_trailing_bytes(payload, offset)?;
        Ok(Some(Self {
            payload,
            dimensions,
            code_len,
            codebook,
            nodes,
        }))
    }

    /// Returns the graph's vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the number of graph nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns whether the graph contains no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the persisted codebook used by every node code.
    #[must_use]
    pub const fn codebook(&self) -> &HnswGraphQuantizationCodebook {
        &self.codebook
    }

    /// Borrows one validated node by its contiguous node id.
    #[must_use]
    pub fn node(&self, node_id: usize) -> Option<QuantizedHnswGraphNodeView<'a>> {
        let location = self.nodes.get(node_id)?;
        let neighbor_bytes = location.neighbor_count * size_of_u32();
        Some(QuantizedHnswGraphNodeView {
            point_id: location.point_id,
            neighbors: &self.payload
                [location.neighbors_offset..location.neighbors_offset + neighbor_bytes],
            code: &self.payload[location.code_offset..location.code_offset + self.code_len],
        })
    }
}
