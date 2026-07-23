//! Quantized compatibility view over the general mapped graph decoder.

use super::{
    HnswGraphPayloadError, HnswGraphQuantizationCodebook, MappedGraphView, MappedNeighborIter,
};

/// Validated borrowed view of a quantized HNSW base-layer node.
#[derive(Debug, Clone, Copy)]
pub struct QuantizedHnswGraphNodeView<'a> {
    point_id: u64,
    neighbors: MappedNeighborIter<'a>,
    code: &'a [u8],
}

impl<'a> QuantizedHnswGraphNodeView<'a> {
    /// Returns the authoritative pgContext point id.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }

    /// Returns the borrowed encoded navigation bytes.
    #[must_use]
    pub const fn code(&self) -> &'a [u8] {
        self.code
    }

    /// Iterates base-layer neighbor node ids without allocating a list.
    #[must_use]
    pub fn neighbors(self) -> QuantizedNeighborIter<'a> {
        self.neighbors
    }
}

/// Exact-size iterator over little-endian neighbor ids in a borrowed payload.
pub type QuantizedNeighborIter<'a> = MappedNeighborIter<'a>;

/// Safe borrowed view over a version-2 quantized HNSW graph payload.
///
/// This compatibility wrapper delegates all validation and byte ownership to
/// [`MappedGraphView`]. Full vectors, codes, and neighbor lists remain in the
/// caller-owned payload bytes.
#[derive(Debug)]
pub struct QuantizedHnswGraphView<'a> {
    graph: MappedGraphView<'a>,
    codebook: HnswGraphQuantizationCodebook,
}

impl<'a> QuantizedHnswGraphView<'a> {
    /// Attaches to a validated quantized payload.
    ///
    /// Version-1 and unquantized version-2 payloads return `Ok(None)` so callers
    /// can use their full-precision serving path.
    ///
    /// # Errors
    ///
    /// Returns [`HnswGraphPayloadError`] when the payload is truncated,
    /// corrupt, oversized, or internally inconsistent.
    pub fn attach(payload: &'a [u8]) -> Result<Option<Self>, HnswGraphPayloadError> {
        let graph = MappedGraphView::attach(payload)?;
        let Some(codebook) = graph.codebook().cloned() else {
            return Ok(None);
        };
        Ok(Some(Self { graph, codebook }))
    }

    /// Returns the graph's vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.graph.dimensions()
    }

    /// Returns the number of graph nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    /// Returns whether the graph contains no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    /// Returns the persisted codebook used by every node code.
    #[must_use]
    pub fn codebook(&self) -> &HnswGraphQuantizationCodebook {
        &self.codebook
    }

    /// Borrows one validated node by its contiguous node id.
    #[must_use]
    pub fn node(&self, node_id: usize) -> Option<QuantizedHnswGraphNodeView<'a>> {
        let node = self.graph.node(node_id)?;
        Some(QuantizedHnswGraphNodeView {
            point_id: node.point_id(),
            neighbors: node.neighbors(),
            code: node.code_bytes(),
        })
    }
}
