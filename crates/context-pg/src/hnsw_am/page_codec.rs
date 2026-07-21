//! Version-two fixed-layout HNSW metapage codec.
//!
//! PostgreSQL owns page headers and line pointers. This module validates and
//! encodes only pgContext's portable item bytes before a buffer adapter copies
//! them to or from a PostgreSQL page.

use core::fmt;

use context_index::{
    CURRENT_GRAPH_LAYOUT_VERSION, GRAPH_PAGE_HEADER_BYTES, GRAPH_PAGE_MAGIC,
    GRAPH_PENDING_RESERVATION_BYTES, GraphPageCodecError, GraphPageEnvelope, GraphPageKind,
    MAX_PENDING_GRAPH_MUTATIONS,
};

const ENVELOPE_BYTES: usize = GRAPH_PAGE_HEADER_BYTES;
const PENDING_CAPACITY: usize = MAX_PENDING_GRAPH_MUTATIONS;
const META_PREFIX_BYTES: usize = 68;
const SLOT_BYTES: usize = GRAPH_PENDING_RESERVATION_BYTES;
const META_BYTES: usize = ENVELOPE_BYTES + META_PREFIX_BYTES + PENDING_CAPACITY * SLOT_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingReservation {
    pub(super) mutation_id: u64,
    pub(super) node_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MetaPageV2 {
    pub(super) generation: u64,
    pub(super) mutation_id: u64,
    pub(super) availability: u8,
    pub(super) root_level: u8,
    pub(super) dimensions: u32,
    pub(super) next_node_id: u64,
    pub(super) node_count: u64,
    pub(super) tombstone_count: u64,
    pub(super) entry_node_id: Option<u64>,
    pub(super) last_published_mutation_id: Option<u64>,
    pub(super) directory_root_page: u64,
    pub(super) descriptor_directory_root_page: u64,
    pub(super) pending: [PendingReservation; PENDING_CAPACITY],
}

/// Validated common envelope for a non-metapage HNSW data page.
///
/// PostgreSQL still owns the page header and line pointers. The typed item
/// codec that follows this envelope owns its own payload validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageHeaderV2 {
    pub(super) kind: GraphPageKind,
    pub(super) generation: u64,
    pub(super) page_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaAvailability {
    Ready,
    RepairRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageCodecError {
    Corrupt(&'static str),
    RebuildRequired(&'static str),
}

impl fmt::Display for PageCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(reason) => write!(formatter, "corrupt HNSW page: {reason}"),
            Self::RebuildRequired(reason) => write!(formatter, "HNSW rebuild required: {reason}"),
        }
    }
}
impl std::error::Error for PageCodecError {}

impl From<GraphPageCodecError> for PageCodecError {
    fn from(error: GraphPageCodecError) -> Self {
        match error {
            GraphPageCodecError::Corrupt { reason } => Self::Corrupt(reason),
            GraphPageCodecError::RebuildRequired { reason } => Self::RebuildRequired(reason),
        }
    }
}

impl MetaPageV2 {
    pub(super) fn empty() -> Self {
        Self {
            generation: 0,
            mutation_id: 0,
            availability: 0,
            root_level: 0,
            dimensions: 0,
            next_node_id: 0,
            node_count: 0,
            tombstone_count: 0,
            entry_node_id: None,
            last_published_mutation_id: None,
            directory_root_page: 0,
            descriptor_directory_root_page: 0,
            pending: [PendingReservation {
                mutation_id: 0,
                node_id: 0,
            }; PENDING_CAPACITY],
        }
    }
}

pub(super) fn encode_meta(meta: &MetaPageV2) -> Result<[u8; META_BYTES], PageCodecError> {
    validate_meta(meta)?;
    let envelope_bytes = u16::try_from(ENVELOPE_BYTES)
        .map_err(|_| PageCodecError::Corrupt("page header length exceeds u16"))?;
    let pending_capacity = u16::try_from(PENDING_CAPACITY)
        .map_err(|_| PageCodecError::Corrupt("pending capacity exceeds u16"))?;
    let mut output = [0_u8; META_BYTES];
    output[..4].copy_from_slice(&GRAPH_PAGE_MAGIC);
    put_u16(&mut output, 4, CURRENT_GRAPH_LAYOUT_VERSION);
    output[6] = GraphPageKind::Meta.code();
    put_u16(&mut output, 8, envelope_bytes);
    put_u64(&mut output, 16, meta.generation);
    put_u64(&mut output, 24, meta.mutation_id);
    let mut offset = ENVELOPE_BYTES;
    output[offset] = meta.availability;
    output[offset + 1] = meta.root_level;
    offset += 4;
    put_u32(&mut output, offset, meta.dimensions);
    offset += 4;
    put_u64(&mut output, offset, meta.next_node_id);
    offset += 8;
    put_u64(&mut output, offset, meta.node_count);
    offset += 8;
    put_u64(&mut output, offset, meta.tombstone_count);
    offset += 8;
    put_u64(&mut output, offset, meta.entry_node_id.unwrap_or(u64::MAX));
    offset += 8;
    put_u64(
        &mut output,
        offset,
        meta.last_published_mutation_id.unwrap_or(0),
    );
    offset += 8;
    put_u64(&mut output, offset, meta.directory_root_page);
    offset += 8;
    put_u64(&mut output, offset, meta.descriptor_directory_root_page);
    offset += 8;
    put_u16(&mut output, offset, pending_count(meta)?);
    put_u16(&mut output, offset + 2, pending_capacity);
    offset += 4;
    for slot in meta.pending {
        put_u64(&mut output, offset, slot.mutation_id);
        put_u64(&mut output, offset + 8, slot.node_id);
        offset += SLOT_BYTES;
    }
    Ok(output)
}

pub(super) fn encode_page_header(
    header: PageHeaderV2,
) -> Result<[u8; ENVELOPE_BYTES], PageCodecError> {
    GraphPageEnvelope::new(header.kind, header.generation, header.page_id)
        .map_err(PageCodecError::from)?
        .encode()
        .map_err(PageCodecError::from)
}

pub(super) fn decode_page_header(bytes: &[u8]) -> Result<PageHeaderV2, PageCodecError> {
    let envelope = GraphPageEnvelope::decode(bytes).map_err(PageCodecError::from)?;
    Ok(PageHeaderV2 {
        kind: envelope.kind(),
        generation: envelope.generation(),
        page_id: envelope.page_id(),
    })
}

pub(super) fn decode_meta(
    bytes: &[u8],
    max_item_bytes: usize,
) -> Result<(MetaPageV2, MetaAvailability), PageCodecError> {
    if max_item_bytes < META_BYTES {
        return Err(PageCodecError::RebuildRequired(
            "page item budget is too small",
        ));
    }
    if bytes.len() != META_BYTES {
        return Err(PageCodecError::Corrupt("metapage length is invalid"));
    }
    if bytes[..4] != GRAPH_PAGE_MAGIC {
        return Err(PageCodecError::RebuildRequired("unsupported format"));
    }
    if read_u16(bytes, 4) != CURRENT_GRAPH_LAYOUT_VERSION {
        return Err(PageCodecError::RebuildRequired("unsupported version"));
    }
    if bytes[6] != GraphPageKind::Meta.code()
        || bytes[7] != 0
        || usize::from(read_u16(bytes, 8)) != ENVELOPE_BYTES
        || bytes[10..16].iter().any(|byte| *byte != 0)
    {
        return Err(PageCodecError::Corrupt("invalid envelope"));
    }
    let mut offset = ENVELOPE_BYTES;
    let availability = bytes[offset];
    let root_level = bytes[offset + 1];
    if bytes[offset + 2..offset + 4].iter().any(|byte| *byte != 0) {
        return Err(PageCodecError::Corrupt("reserved meta bytes are nonzero"));
    }
    offset += 4;
    let dimensions = read_u32(bytes, offset);
    offset += 4;
    let next_node_id = read_u64(bytes, offset);
    offset += 8;
    let node_count = read_u64(bytes, offset);
    offset += 8;
    let tombstone_count = read_u64(bytes, offset);
    offset += 8;
    let entry = read_u64(bytes, offset);
    offset += 8;
    let last = read_u64(bytes, offset);
    offset += 8;
    let directory_root_page = read_u64(bytes, offset);
    offset += 8;
    let descriptor_directory_root_page = read_u64(bytes, offset);
    offset += 8;
    let declared_pending = read_u16(bytes, offset) as usize;
    let capacity = read_u16(bytes, offset + 2) as usize;
    offset += 4;
    if capacity != PENDING_CAPACITY {
        return Err(PageCodecError::RebuildRequired(
            "pending capacity is unsupported",
        ));
    }
    let mut pending = [PendingReservation {
        mutation_id: 0,
        node_id: 0,
    }; PENDING_CAPACITY];
    for slot in &mut pending {
        slot.mutation_id = read_u64(bytes, offset);
        slot.node_id = read_u64(bytes, offset + 8);
        offset += SLOT_BYTES;
    }
    let meta = MetaPageV2 {
        generation: read_u64(bytes, 16),
        mutation_id: read_u64(bytes, 24),
        availability,
        root_level,
        dimensions,
        next_node_id,
        node_count,
        tombstone_count,
        entry_node_id: (entry != u64::MAX).then_some(entry),
        last_published_mutation_id: (last != 0).then_some(last),
        directory_root_page,
        descriptor_directory_root_page,
        pending,
    };
    if pending_count(&meta)? as usize != declared_pending {
        return Err(PageCodecError::Corrupt(
            "pending count disagrees with slots",
        ));
    }
    validate_meta(&meta)?;
    let state = if declared_pending == 0 {
        MetaAvailability::Ready
    } else {
        MetaAvailability::RepairRequired
    };
    Ok((meta, state))
}

fn validate_meta(meta: &MetaPageV2) -> Result<(), PageCodecError> {
    if meta.availability != 0
        || meta.root_level > 63
        || meta.tombstone_count > meta.node_count
        || (meta.node_count == 0
            && (meta.generation != 0
                || meta.dimensions != 0
                || meta.entry_node_id.is_some()
                || meta.last_published_mutation_id.is_some()))
        || (meta.node_count > 0
            && (meta.generation == 0
                || meta.dimensions == 0
                || meta.entry_node_id.is_none()
                || meta.last_published_mutation_id.is_none()))
    {
        return Err(PageCodecError::Corrupt("published state is inconsistent"));
    }
    for (index, slot) in meta.pending.iter().enumerate() {
        if slot.mutation_id == 0 {
            if slot.node_id != 0 {
                return Err(PageCodecError::Corrupt("empty pending slot has node id"));
            }
        } else if meta.pending[..index]
            .iter()
            .any(|previous| previous.mutation_id == slot.mutation_id)
        {
            return Err(PageCodecError::Corrupt("duplicate pending mutation"));
        }
    }
    Ok(())
}

fn pending_count(meta: &MetaPageV2) -> Result<u16, PageCodecError> {
    u16::try_from(
        meta.pending
            .iter()
            .filter(|slot| slot.mutation_id != 0)
            .count(),
    )
    .map_err(|_| PageCodecError::Corrupt("too many pending slots"))
}
fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn data_page_headers_round_trip_for_every_kind_and_nonzero_identity(
            kind_index in 0_usize..4,
            generation in 1_u64..u64::MAX,
            page_id in 1_u64..u64::MAX,
        ) {
            let kind = [
                GraphPageKind::Directory,
                GraphPageKind::Node,
                GraphPageKind::Adjacency,
                GraphPageKind::MutationDescriptor,
            ][kind_index];
            let header = PageHeaderV2 {
                kind,
                generation,
                page_id,
            };

            let encoded = encode_page_header(header).expect("valid generated header");
            prop_assert_eq!(
                decode_page_header(&encoded).expect("generated header decodes"),
                header
            );
        }
    }

    #[test]
    fn empty_metapage_round_trips() {
        let bytes = encode_meta(&MetaPageV2::empty()).expect("encode");
        assert_eq!(bytes.len(), META_BYTES);
        assert_eq!(
            decode_meta(&bytes, META_BYTES).expect("decode").1,
            MetaAvailability::Ready
        );
    }

    #[test]
    fn published_metapage_round_trips_all_fields() {
        let mut meta = MetaPageV2::empty();
        meta.generation = 9;
        meta.mutation_id = 22;
        meta.root_level = 3;
        meta.dimensions = 768;
        meta.next_node_id = 41;
        meta.node_count = 40;
        meta.tombstone_count = 2;
        meta.entry_node_id = Some(17);
        meta.last_published_mutation_id = Some(22);
        meta.directory_root_page = 5;
        meta.descriptor_directory_root_page = 8;

        let bytes = encode_meta(&meta).expect("encode published state");
        let (decoded, availability) =
            decode_meta(&bytes, META_BYTES).expect("decode published state");

        assert_eq!(decoded, meta);
        assert_eq!(availability, MetaAvailability::Ready);
    }

    #[test]
    fn data_page_header_round_trips_and_rejects_meta_or_reserved_bytes() {
        let header = PageHeaderV2 {
            kind: GraphPageKind::Adjacency,
            generation: 9,
            page_id: 4,
        };
        let bytes = encode_page_header(header).expect("encode data page header");
        assert_eq!(
            decode_page_header(&bytes).expect("decode data page header"),
            header
        );

        let mut meta_kind = bytes;
        meta_kind[6] = GraphPageKind::Meta.code();
        assert!(matches!(
            decode_page_header(&meta_kind),
            Err(PageCodecError::Corrupt("data page has invalid kind"))
        ));

        let mut reserved = bytes;
        reserved[7] = 1;
        assert!(matches!(
            decode_page_header(&reserved),
            Err(PageCodecError::Corrupt("invalid envelope"))
        ));
    }
    #[test]
    fn truncated_and_corrupt_pages_fail_closed() {
        let bytes = encode_meta(&MetaPageV2::empty()).expect("encode");
        for size in 0..META_BYTES {
            assert!(decode_meta(&bytes[..size], META_BYTES).is_err());
        }
        let mut corrupt = bytes;
        corrupt[0] ^= 1;
        assert!(matches!(
            decode_meta(&corrupt, META_BYTES),
            Err(PageCodecError::RebuildRequired(_))
        ));
    }

    #[test]
    fn pending_reservation_requires_repair_without_changing_published_state() {
        let mut meta = MetaPageV2::empty();
        meta.pending[0] = PendingReservation {
            mutation_id: 7,
            node_id: 42,
        };
        let bytes = encode_meta(&meta).expect("encode pending state");
        let (decoded, availability) =
            decode_meta(&bytes, META_BYTES).expect("decode pending state");

        assert_eq!(availability, MetaAvailability::RepairRequired);
        assert_eq!(decoded.pending[0], meta.pending[0]);
        assert_eq!(decoded.node_count, 0);
    }

    #[test]
    fn invalid_counts_slots_and_envelope_fields_fail_closed() {
        let bytes = encode_meta(&MetaPageV2::empty()).expect("encode");
        let cases = [(7, 1_u8), (10, 1), (98, 1), (108, 1)];
        for (offset, value) in cases {
            let mut corrupt = bytes;
            corrupt[offset] = value;
            assert!(
                decode_meta(&corrupt, META_BYTES).is_err(),
                "offset {offset}"
            );
        }
        assert!(matches!(
            decode_meta(&bytes, META_BYTES - 1),
            Err(PageCodecError::RebuildRequired(_))
        ));
    }

    #[test]
    fn duplicate_pending_mutations_and_incomplete_publication_fail_closed() {
        let mut duplicate = MetaPageV2::empty();
        duplicate.pending[0] = PendingReservation {
            mutation_id: 7,
            node_id: 1,
        };
        duplicate.pending[1] = PendingReservation {
            mutation_id: 7,
            node_id: 2,
        };
        assert!(matches!(
            encode_meta(&duplicate),
            Err(PageCodecError::Corrupt("duplicate pending mutation"))
        ));

        let mut incomplete = MetaPageV2::empty();
        incomplete.node_count = 1;
        assert!(matches!(
            encode_meta(&incomplete),
            Err(PageCodecError::Corrupt("published state is inconsistent"))
        ));
    }
}
