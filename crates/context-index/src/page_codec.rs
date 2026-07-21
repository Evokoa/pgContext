//! Fixed-layout, storage-agnostic HNSW data-page envelopes.
//!
//! This module owns the portable common header shared by PostgreSQL-page and
//! mapped adapters. Adapters retain ownership of page buffers and item payloads.

use crate::{
    CURRENT_GRAPH_LAYOUT_VERSION, GRAPH_PAGE_HEADER_BYTES, GRAPH_PAGE_MAGIC, GraphPageKind,
};

/// Failure while validating a version-two HNSW data-page envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GraphPageCodecError {
    /// The bytes are structurally invalid for the current layout.
    #[error("corrupt HNSW page envelope: {reason}")]
    Corrupt {
        /// Stable corruption reason.
        reason: &'static str,
    },
    /// The bytes use a layout that must be rebuilt rather than reinterpreted.
    #[error("HNSW rebuild required: {reason}")]
    RebuildRequired {
        /// Stable rebuild reason.
        reason: &'static str,
    },
}

/// Validated common envelope for a non-metapage HNSW data page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPageEnvelope {
    kind: GraphPageKind,
    generation: u64,
    page_id: u64,
}

impl GraphPageEnvelope {
    /// Creates a data-page envelope.
    ///
    /// # Errors
    ///
    /// Returns [`GraphPageCodecError::Corrupt`] when `kind` is metadata or an
    /// identity component is zero.
    pub fn new(
        kind: GraphPageKind,
        generation: u64,
        page_id: u64,
    ) -> Result<Self, GraphPageCodecError> {
        let value = Self {
            kind,
            generation,
            page_id,
        };
        value.validate()?;
        Ok(value)
    }

    /// Encodes the fixed version-two header.
    pub fn encode(self) -> Result<[u8; GRAPH_PAGE_HEADER_BYTES], GraphPageCodecError> {
        let header_bytes =
            u16::try_from(GRAPH_PAGE_HEADER_BYTES).map_err(|_| GraphPageCodecError::Corrupt {
                reason: "page header length exceeds u16",
            })?;
        let mut bytes = [0_u8; GRAPH_PAGE_HEADER_BYTES];
        bytes[..4].copy_from_slice(&GRAPH_PAGE_MAGIC);
        bytes[4..6].copy_from_slice(&CURRENT_GRAPH_LAYOUT_VERSION.to_le_bytes());
        bytes[6] = self.kind.code();
        bytes[8..10].copy_from_slice(&header_bytes.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.page_id.to_le_bytes());
        Ok(bytes)
    }

    /// Decodes and validates a fixed version-two header.
    pub fn decode(bytes: &[u8]) -> Result<Self, GraphPageCodecError> {
        if bytes.len() != GRAPH_PAGE_HEADER_BYTES {
            return Err(GraphPageCodecError::Corrupt {
                reason: "page header length is invalid",
            });
        }
        if bytes[..4] != GRAPH_PAGE_MAGIC {
            return Err(GraphPageCodecError::RebuildRequired {
                reason: "unsupported format",
            });
        }
        if u16::from_le_bytes([bytes[4], bytes[5]]) != CURRENT_GRAPH_LAYOUT_VERSION {
            return Err(GraphPageCodecError::RebuildRequired {
                reason: "unsupported version",
            });
        }
        if bytes[7] != 0
            || usize::from(u16::from_le_bytes([bytes[8], bytes[9]])) != GRAPH_PAGE_HEADER_BYTES
            || bytes[10..16].iter().any(|byte| *byte != 0)
        {
            return Err(GraphPageCodecError::Corrupt {
                reason: "invalid envelope",
            });
        }
        // Every role in `GraphPageKind::ALL` decodes here; `validate` below is
        // what rejects `Meta` on a data page. Enumerating roles individually
        // silently rejected any kind added later (the `Delta` role of the
        // segmented write path was decoded as corrupt for exactly that reason).
        let Some(kind) = GraphPageKind::from_code(bytes[6]) else {
            return Err(GraphPageCodecError::Corrupt {
                reason: "data page has invalid kind",
            });
        };
        Self::new(kind, read_u64(bytes, 16), read_u64(bytes, 24))
    }

    /// Returns the page role.
    #[must_use]
    pub const fn kind(self) -> GraphPageKind {
        self.kind
    }
    /// Returns the owning generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }
    /// Returns the nonzero relation-local page identifier.
    #[must_use]
    pub const fn page_id(self) -> u64 {
        self.page_id
    }

    fn validate(self) -> Result<(), GraphPageCodecError> {
        if self.kind == GraphPageKind::Meta {
            return Err(GraphPageCodecError::Corrupt {
                reason: "data page has invalid kind",
            });
        }
        if self.generation == 0 || self.page_id == 0 {
            return Err(GraphPageCodecError::Corrupt {
                reason: "data page identity is invalid",
            });
        }
        Ok(())
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}
