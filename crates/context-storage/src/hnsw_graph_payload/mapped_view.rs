//! Borrowed, allocation-bounded view over mapped HNSW graph payloads.

use core::{iter::FusedIterator, mem::size_of};

use context_codec::QuantizedCodebook;
use context_core::policy::MAX_VECTOR_DIMENSIONS;

use crate::CodecArtifactView;

use super::{
    CURRENT_HNSW_GRAPH_PAYLOAD_VERSION, HNSW_GRAPH_PAYLOAD_HEADER_LEN_CURRENT,
    HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1, HNSW_GRAPH_PAYLOAD_MAGIC, HNSW_GRAPH_PAYLOAD_VERSION_V1,
    HNSW_GRAPH_RECORD_HEADER_LEN, HnswGraphPayloadError, MAX_HNSW_GRAPH_RECORDS,
    codec_artifact_error, read_f32, read_u32, read_u64, require_no_trailing_bytes,
    require_payload_bytes, size_of_f32, size_of_u32,
};

#[derive(Debug, Clone, Copy)]
struct NodeLocation {
    point_id: u64,
    vector_offset: usize,
    neighbors_offset: usize,
    neighbor_count: usize,
}

/// Validated borrowed view of one mapped HNSW base-layer node.
#[derive(Debug, Clone, Copy)]
pub struct MappedGraphNodeView<'a> {
    point_id: u64,
    vector: &'a [u8],
    neighbors: &'a [u8],
    code: &'a [u8],
}

impl<'a> MappedGraphNodeView<'a> {
    /// Returns the authoritative pgContext point id.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Decodes this node's vector into reusable caller-owned scratch storage.
    ///
    /// Only the vector for the requested node is copied. The graph payload,
    /// adjacency, and encoded navigation representation remain map-resident.
    pub fn decode_vector_into(self, scratch: &mut Vec<f32>) -> &[f32] {
        scratch.clear();
        scratch.reserve(self.vector.len() / size_of_f32());
        for offset in (0..self.vector.len()).step_by(size_of_f32()) {
            scratch.push(read_f32(self.vector, offset));
        }
        scratch.as_slice()
    }

    /// Iterates base-layer neighbor ids directly from mapped bytes.
    #[must_use]
    pub const fn neighbors(self) -> MappedNeighborIter<'a> {
        MappedNeighborIter {
            bytes: self.neighbors,
            offset: 0,
        }
    }

    /// Returns encoded navigation bytes when this is a quantized generation.
    #[must_use]
    pub fn code(self) -> Option<&'a [u8]> {
        (!self.code.is_empty()).then_some(self.code)
    }

    pub(super) const fn code_bytes(self) -> &'a [u8] {
        self.code
    }
}

/// Exact-size iterator over little-endian neighbor ids in mapped bytes.
#[derive(Debug, Clone, Copy)]
pub struct MappedNeighborIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Iterator for MappedNeighborIter<'_> {
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

impl ExactSizeIterator for MappedNeighborIter<'_> {}
impl FusedIterator for MappedNeighborIter<'_> {}

/// Safe borrowed view over a validated mapped HNSW graph generation.
///
/// Attachment validates every header, record boundary, finite vector value,
/// neighbor id, code, and trailing byte. It allocates one compact location
/// record per node but never copies the graph payload itself. Node vectors are
/// decoded on demand into caller-provided scratch storage.
#[derive(Debug)]
pub struct MappedGraphView<'a> {
    payload: &'a [u8],
    version: u32,
    dimensions: usize,
    codec: Option<CodecArtifactView<'a>>,
    nodes: Vec<NodeLocation>,
}

impl<'a> MappedGraphView<'a> {
    /// Attaches to a version-1 or current HNSW payload without copying it.
    ///
    /// # Errors
    ///
    /// Returns [`HnswGraphPayloadError`] when any byte range, graph identity,
    /// vector, adjacency, quantization code, or version invariant is invalid.
    pub fn attach(payload: &'a [u8]) -> Result<Self, HnswGraphPayloadError> {
        Self::attach_impl(payload, None)
    }

    /// Attaches while bounding every decoded heap allocation retained by the view.
    ///
    /// The node-location projection is checked before its `Vec` is allocated.
    /// Product-codebook storage is projected from persisted counts and checked
    /// before codec decoding allocates nested vectors.
    ///
    /// # Errors
    ///
    /// Returns [`HnswGraphPayloadError::MemoryBudgetExceeded`] before an
    /// allocation that would cross `max_resident_bytes`, in addition to the
    /// structural errors returned by [`Self::attach`].
    pub fn attach_with_memory_budget(
        payload: &'a [u8],
        max_resident_bytes: usize,
    ) -> Result<Self, HnswGraphPayloadError> {
        Self::attach_impl(payload, Some(max_resident_bytes))
    }

    fn attach_impl(
        payload: &'a [u8],
        max_resident_bytes: Option<usize>,
    ) -> Result<Self, HnswGraphPayloadError> {
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
        let record_count_u32 = read_u32(payload, 12);
        let dimensions = read_u32(payload, 16) as usize;
        let (records_offset, codec_len) = match version {
            HNSW_GRAPH_PAYLOAD_VERSION_V1 => {
                let reserved = read_u32(payload, 20);
                if reserved != 0 {
                    return Err(HnswGraphPayloadError::NonZeroReserved { value: reserved });
                }
                (HNSW_GRAPH_PAYLOAD_HEADER_LEN_V1, 0)
            }
            CURRENT_HNSW_GRAPH_PAYLOAD_VERSION => {
                if payload.len() < HNSW_GRAPH_PAYLOAD_HEADER_LEN_CURRENT {
                    return Err(HnswGraphPayloadError::TruncatedHeader {
                        actual: payload.len(),
                        minimum: HNSW_GRAPH_PAYLOAD_HEADER_LEN_CURRENT,
                    });
                }
                let reserved = read_u32(payload, 20);
                if reserved != 0 {
                    return Err(HnswGraphPayloadError::NonZeroReserved { value: reserved });
                }
                let codec_len = usize::try_from(read_u64(payload, 24))
                    .map_err(|_| HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
                (HNSW_GRAPH_PAYLOAD_HEADER_LEN_CURRENT, codec_len)
            }
            version => return Err(HnswGraphPayloadError::UnsupportedVersion { version }),
        };

        let record_count = record_count_u32 as usize;
        if record_count == 0 {
            return Err(HnswGraphPayloadError::EmptyGraph);
        }
        if record_count > MAX_HNSW_GRAPH_RECORDS {
            return Err(HnswGraphPayloadError::RecordCountLimit {
                declared: record_count,
                maximum: MAX_HNSW_GRAPH_RECORDS,
            });
        }
        let node_location_bytes = record_count
            .checked_mul(size_of::<NodeLocation>())
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        require_memory_budget(node_location_bytes, max_resident_bytes)?;
        if dimensions == 0 {
            return Err(HnswGraphPayloadError::EmptyVector);
        }
        if dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(HnswGraphPayloadError::InvalidVector(format!(
                "dense vector dimensions exceeds limit {MAX_VECTOR_DIMENSIONS}: {dimensions}"
            )));
        }

        let vector_bytes = dimensions
            .checked_mul(size_of_f32())
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let minimum_record_bytes = HNSW_GRAPH_RECORD_HEADER_LEN
            .checked_add(vector_bytes)
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let minimum_payload_bytes = record_count
            .checked_mul(minimum_record_bytes)
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        let available = payload.len().saturating_sub(records_offset);
        if available < minimum_payload_bytes {
            return Err(HnswGraphPayloadError::TruncatedRecord {
                record_index: 0,
                expected: minimum_payload_bytes,
                actual: available,
            });
        }

        let mut nodes = Vec::with_capacity(record_count);
        let mut offset = records_offset;
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
            let vector_offset = offset;
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
            for neighbor in (MappedNeighborIter {
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

            nodes.push(NodeLocation {
                point_id,
                vector_offset,
                neighbors_offset,
                neighbor_count,
            });
        }
        let codec_offset = if codec_len == 0 {
            offset
        } else {
            let aligned = super::align_up_16(offset)
                .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
            if payload
                .get(offset..aligned)
                .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
            {
                return Err(HnswGraphPayloadError::InvalidQuantization(
                    "codec artifact alignment padding is invalid".to_owned(),
                ));
            }
            aligned
        };
        let codec_end = codec_offset
            .checked_add(codec_len)
            .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
        if codec_end != payload.len() {
            return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                "codec artifact length mismatch: declared {codec_len}, available {}",
                payload.len().saturating_sub(codec_offset)
            )));
        }
        let codec = if codec_len == 0 {
            require_no_trailing_bytes(payload, offset)?;
            None
        } else {
            let codec_payload = &payload[codec_offset..codec_end];
            let codebook_resident_bytes =
                CodecArtifactView::projected_resident_bytes(codec_payload)
                    .map_err(codec_artifact_error)?;
            let total_resident_bytes = node_location_bytes
                .checked_add(codebook_resident_bytes)
                .ok_or(HnswGraphPayloadError::RecordSizeOverflow { record_index: 0 })?;
            require_memory_budget(total_resident_bytes, max_resident_bytes)?;
            let codec = CodecArtifactView::attach(codec_payload).map_err(codec_artifact_error)?;
            if codec.dimensions() != dimensions || codec.codes().row_count() != record_count {
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                    "codec artifact binding mismatch: expected {record_count} rows of {dimensions} dimensions, got {} rows of {} dimensions",
                    codec.codes().row_count(),
                    codec.dimensions()
                )));
            }
            Some(codec)
        };

        Ok(Self {
            payload,
            version,
            dimensions,
            codec,
            nodes,
        })
    }

    /// Returns the graph payload format version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
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

    /// Returns the quantization codebook for encoded generations.
    #[must_use]
    pub const fn codebook(&self) -> Option<&QuantizedCodebook> {
        match &self.codec {
            Some(codec) => Some(codec.codebook()),
            None => None,
        }
    }

    /// Borrows one validated node by its contiguous node id.
    #[must_use]
    pub fn node(&self, node_id: usize) -> Option<MappedGraphNodeView<'a>> {
        let location = self.nodes.get(node_id)?;
        let vector_bytes = self.dimensions * size_of_f32();
        let neighbor_bytes = location.neighbor_count * size_of_u32();
        let code = self
            .codec
            .as_ref()
            .and_then(|codec| codec.codes().code(node_id))
            .unwrap_or(&[]);
        Some(MappedGraphNodeView {
            point_id: location.point_id,
            vector: &self.payload[location.vector_offset..location.vector_offset + vector_bytes],
            neighbors: &self.payload
                [location.neighbors_offset..location.neighbors_offset + neighbor_bytes],
            code,
        })
    }
}

fn require_memory_budget(
    required: usize,
    maximum: Option<usize>,
) -> Result<(), HnswGraphPayloadError> {
    if let Some(maximum) = maximum
        && required > maximum
    {
        return Err(HnswGraphPayloadError::MemoryBudgetExceeded { required, maximum });
    }
    Ok(())
}
