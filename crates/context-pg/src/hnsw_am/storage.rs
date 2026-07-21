use context_core::DenseVector;
use context_index::{
    GraphDirectoryKeyKind, HnswGraphNodeSnapshot, HnswNodeId, HnswPointId, LayerIndex,
    MAX_GRAPH_LAYERS, MAX_GRAPH_NEIGHBORS_PER_LAYER,
};
use pgrx::prelude::*;
use std::mem::{align_of, size_of};
use std::ptr;
use std::slice;

use crate::error::{raise_core_error, raise_sql_error};

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswVectorRecordHeader {
    heap_tid: u64,
    node_id: u32,
    dimensions: u32,
    neighbor_count: u32,
    layer_count: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswAdjacencyRecordHeader {
    node_id: u32,
    layer: u32,
    neighbor_count: u32,
    reserved: u32,
}

#[derive(Debug, Clone)]
pub(super) struct HnswVectorRecord {
    pub(super) node_id: HnswNodeId,
    pub(super) heap_tid: u64,
    pub(super) vector: DenseVector,
    pub(super) base_neighbors: Vec<HnswNodeId>,
    pub(super) layers: Vec<Vec<HnswNodeId>>,
}

/// Borrowed, structurally validated view over one packed node-page record.
pub(super) struct HnswVectorRecordView<'a> {
    item: *const u8,
    header: HnswVectorRecordHeader,
    vector: &'a [f32],
    layers_offset: usize,
}

impl<'a> HnswVectorRecordView<'a> {
    pub(super) const fn node_id(&self) -> HnswNodeId {
        HnswNodeId::new(self.header.node_id as usize)
    }

    pub(super) const fn heap_tid(&self) -> u64 {
        self.header.heap_tid & !HNSW_TOMBSTONE_TID_FLAG
    }

    pub(super) const fn vector(&self) -> &'a [f32] {
        self.vector
    }

    pub(super) const fn layer_count(&self) -> usize {
        self.header.layer_count as usize
    }

    pub(super) fn read_neighbors_into(
        &self,
        layer: LayerIndex,
        output: &mut Vec<HnswNodeId>,
    ) -> bool {
        let wanted = layer.get();
        if wanted >= self.layer_count() {
            return false;
        }
        let mut offset = self.layers_offset;
        for layer_index in 0..self.layer_count() {
            // SAFETY: Construction validated every count and link span, and
            // the owning page remains pinned for the view lifetime.
            let neighbor_count =
                unsafe { ptr::read_unaligned(self.item.add(offset).cast::<u32>()) } as usize;
            offset += size_of::<u32>();
            if layer_index == wanted {
                output.clear();
                output.reserve(neighbor_count);
                for index in 0..neighbor_count {
                    // SAFETY: Constructor validation bounded the complete link
                    // span inside `item_len`.
                    let neighbor = unsafe {
                        ptr::read_unaligned(
                            self.item
                                .add(offset + index * size_of::<u32>())
                                .cast::<u32>(),
                        )
                    };
                    output.push(HnswNodeId::new(neighbor as usize));
                }
                return true;
            }
            offset += neighbor_count * size_of::<u32>();
        }
        false
    }
}

/// Borrows a packed vector record without copying its vector or links.
///
/// # Safety
///
/// `item` must remain readable for `item_len` bytes and four-byte aligned for
/// the returned view's lifetime. A PostgreSQL caller must retain the owning
/// buffer pin and lock until the view is dropped.
pub(super) unsafe fn hnsw_vector_record_view<'a>(
    item: *const u8,
    item_len: usize,
) -> HnswVectorRecordView<'a> {
    if item_len < size_of::<HnswVectorRecordHeader>() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record is too short",
        );
    }
    // SAFETY: The caller-bounded payload contains the complete fixed header.
    let header = unsafe { ptr::read_unaligned(item.cast::<HnswVectorRecordHeader>()) };
    let dimensions = header.dimensions as usize;
    if dimensions == 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record has zero dimensions",
        );
    }
    let vector_bytes = dimensions.checked_mul(size_of::<f32>()).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW vector dimensions overflow record size",
        )
    });
    let vector_offset = size_of::<HnswVectorRecordHeader>();
    let vector_end = checked_record_end(vector_offset, vector_bytes, item_len, "vector payload");
    // Page items and the fixed 24-byte header are MAXALIGN/f32 aligned. Check
    // explicitly before constructing the typed borrowed slice.
    // SAFETY: `vector_offset <= vector_end <= item_len` was established above.
    let vector_pointer = unsafe { item.add(vector_offset) };
    if vector_pointer.addr() % align_of::<f32>() != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record payload is misaligned",
        );
    }
    // SAFETY: The checked span contains exactly `dimensions` aligned f32s and
    // the caller keeps the page pinned for `'a`.
    let vector = unsafe { slice::from_raw_parts(vector_pointer.cast::<f32>(), dimensions) };
    let layer_count = header.layer_count as usize;
    if layer_count == 0 || layer_count > MAX_GRAPH_LAYERS {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record has invalid layer count",
        );
    }
    let mut offset = vector_end;
    let mut base_neighbor_count = None;
    for layer_index in 0..layer_count {
        let count_end = checked_record_end(offset, size_of::<u32>(), item_len, "layer count");
        // SAFETY: `count_end` bounds the count inside the item.
        let neighbor_count =
            unsafe { ptr::read_unaligned(item.add(offset).cast::<u32>()) } as usize;
        if neighbor_count > MAX_GRAPH_NEIGHBORS_PER_LAYER {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW vector record layer has too many neighbors",
            );
        }
        if layer_index == 0 {
            base_neighbor_count = Some(neighbor_count);
        }
        let link_bytes = neighbor_count
            .checked_mul(size_of::<u32>())
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "HNSW vector record neighbor bytes overflow",
                )
            });
        offset = checked_record_end(count_end, link_bytes, item_len, "layer links");
    }
    if offset != item_len || base_neighbor_count != Some(header.neighbor_count as usize) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record layout is inconsistent",
        );
    }
    HnswVectorRecordView {
        item,
        header,
        vector,
        layers_offset: vector_end,
    }
}

const HNSW_TOMBSTONE_TID_FLAG: u64 = 1_u64 << 63;

pub(super) const fn hnsw_record_is_tombstoned(record: &HnswVectorRecord) -> bool {
    record.heap_tid & HNSW_TOMBSTONE_TID_FLAG != 0
}

pub(super) const fn hnsw_record_heap_tid(record: &HnswVectorRecord) -> u64 {
    record.heap_tid & !HNSW_TOMBSTONE_TID_FLAG
}

pub(super) const fn hnsw_point_id_is_tombstoned(point_id: HnswPointId) -> bool {
    point_id.get() & HNSW_TOMBSTONE_TID_FLAG != 0
}

const fn hnsw_graph_point_id(record: &HnswVectorRecord) -> HnswPointId {
    if hnsw_record_is_tombstoned(record) {
        // A heap TID can be reused after VACUUM. Bind traversal-only tombstones
        // to their stable structural node IDs so graph reconstruction retains
        // unique point IDs without confusing an old tuple with its replacement.
        HnswPointId::new(HNSW_TOMBSTONE_TID_FLAG | record.node_id.get() as u64)
    } else {
        HnswPointId::new(record.heap_tid)
    }
}

pub(super) fn hnsw_tombstone_record(record: &HnswVectorRecord) -> HnswVectorRecord {
    let mut tombstone = record.clone();
    tombstone.heap_tid |= HNSW_TOMBSTONE_TID_FLAG;
    tombstone
}

/// One complete, bounded adjacency layer stored on a typed adjacency page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HnswAdjacencyRecord {
    pub(super) node_id: HnswNodeId,
    pub(super) layer: LayerIndex,
    pub(super) neighbors: Vec<HnswNodeId>,
}

/// Version-two bounded locator for a node, layer, or mutation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswDirectoryRecord {
    pub(super) key_kind: GraphDirectoryKeyKind,
    pub(super) generation: u64,
    pub(super) identity: u64,
    pub(super) ordinal: u16,
    pub(super) target_page: u64,
    pub(super) target_slot: u16,
    pub(super) revision: u64,
}

const HNSW_DIRECTORY_RECORD_BYTES: usize = 56;

pub(super) fn encode_hnsw_directory_record(record: HnswDirectoryRecord) -> Vec<u8> {
    let mut bytes = vec![0_u8; HNSW_DIRECTORY_RECORD_BYTES];
    bytes[0] = record.key_kind.code();
    bytes[8..16].copy_from_slice(&record.generation.to_le_bytes());
    bytes[16..24].copy_from_slice(&record.identity.to_le_bytes());
    bytes[24..26].copy_from_slice(&record.ordinal.to_le_bytes());
    bytes[32..40].copy_from_slice(&record.target_page.to_le_bytes());
    bytes[40..42].copy_from_slice(&record.target_slot.to_le_bytes());
    bytes[48..56].copy_from_slice(&record.revision.to_le_bytes());
    bytes
}

pub(super) fn decode_hnsw_directory_record(
    bytes: &[u8],
) -> Result<HnswDirectoryRecord, &'static str> {
    if bytes.len() != HNSW_DIRECTORY_RECORD_BYTES {
        return Err("directory record length is invalid");
    }
    if bytes[1..8].iter().any(|byte| *byte != 0)
        || bytes[26..32].iter().any(|byte| *byte != 0)
        || bytes[42..48].iter().any(|byte| *byte != 0)
    {
        return Err("directory record reserved bytes are nonzero");
    }
    let key_kind = match bytes[0] {
        1 => GraphDirectoryKeyKind::Node,
        2 => GraphDirectoryKeyKind::Adjacency,
        3 => GraphDirectoryKeyKind::MutationDescriptor,
        4 => GraphDirectoryKeyKind::MutationEntry,
        _ => return Err("directory record key kind is invalid"),
    };
    let mut generation = [0_u8; 8];
    generation.copy_from_slice(&bytes[8..16]);
    let mut identity = [0_u8; 8];
    identity.copy_from_slice(&bytes[16..24]);
    let mut ordinal = [0_u8; 2];
    ordinal.copy_from_slice(&bytes[24..26]);
    let mut target_page = [0_u8; 8];
    target_page.copy_from_slice(&bytes[32..40]);
    let mut target_slot = [0_u8; 2];
    target_slot.copy_from_slice(&bytes[40..42]);
    let mut revision = [0_u8; 8];
    revision.copy_from_slice(&bytes[48..56]);
    Ok(HnswDirectoryRecord {
        key_kind,
        generation: u64::from_le_bytes(generation),
        identity: u64::from_le_bytes(identity),
        ordinal: u16::from_le_bytes(ordinal),
        target_page: u64::from_le_bytes(target_page),
        target_slot: u16::from_le_bytes(target_slot),
        revision: u64::from_le_bytes(revision),
    })
}

#[allow(dead_code, reason = "retained for pre-v6 page compatibility tests")]
pub(super) fn encode_hnsw_adjacency_record(record: &HnswAdjacencyRecord) -> Vec<u8> {
    let header = HnswAdjacencyRecordHeader {
        node_id: node_id_to_u32(record.node_id),
        layer: layer_to_u32(record.layer),
        neighbor_count: neighbor_count_to_u32(record.neighbors.len()),
        reserved: 0,
    };
    let mut payload = Vec::with_capacity(
        size_of::<HnswAdjacencyRecordHeader>() + record.neighbors.len() * size_of::<u32>(),
    );
    // SAFETY: The C-layout header remains live until copied into the owned payload.
    let header_bytes = unsafe {
        slice::from_raw_parts(
            ptr::addr_of!(header).cast::<u8>(),
            size_of::<HnswAdjacencyRecordHeader>(),
        )
    };
    payload.extend_from_slice(header_bytes);
    for neighbor in &record.neighbors {
        payload.extend_from_slice(&node_id_to_u32(*neighbor).to_ne_bytes());
    }
    payload
}

pub(super) unsafe fn decode_hnsw_adjacency_record(
    item: *const u8,
    item_len: usize,
) -> HnswAdjacencyRecord {
    if item_len < size_of::<HnswAdjacencyRecordHeader>() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW adjacency record is too short",
        );
    }
    // SAFETY: The caller provides a page item span; unaligned reads are valid.
    let header = unsafe { ptr::read_unaligned(item.cast::<HnswAdjacencyRecordHeader>()) };
    if header.reserved != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW adjacency record has nonzero reserved bytes",
        );
    }
    if header.layer as usize >= MAX_GRAPH_LAYERS {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW adjacency record has invalid layer",
        );
    }
    let layer = LayerIndex::new(header.layer as usize);
    let neighbor_count = header.neighbor_count as usize;
    if neighbor_count > MAX_GRAPH_NEIGHBORS_PER_LAYER {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW adjacency record has too many neighbors",
        );
    }
    let neighbor_bytes = neighbor_count
        .checked_mul(size_of::<u32>())
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "HNSW adjacency record length overflows",
            );
        });
    let expected = checked_record_end(
        size_of::<HnswAdjacencyRecordHeader>(),
        neighbor_bytes,
        item_len,
        "adjacency links",
    );
    if expected != item_len {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW adjacency record has trailing bytes",
        );
    }
    let mut neighbors = Vec::with_capacity(neighbor_count);
    for index in 0..neighbor_count {
        // SAFETY: `expected` proves every encoded node identifier is in bounds.
        let neighbor = unsafe {
            ptr::read_unaligned(
                item.add(size_of::<HnswAdjacencyRecordHeader>() + index * size_of::<u32>())
                    .cast::<u32>(),
            )
        };
        neighbors.push(HnswNodeId::new(neighbor as usize));
    }
    HnswAdjacencyRecord {
        node_id: HnswNodeId::new(header.node_id as usize),
        layer,
        neighbors,
    }
}

pub(super) fn encode_hnsw_vector_record(record: &HnswVectorRecord) -> Vec<u8> {
    let layers = record_layers(record);
    let header = HnswVectorRecordHeader {
        heap_tid: record.heap_tid,
        node_id: node_id_to_u32(record.node_id),
        dimensions: dimension_to_u32(record.vector.dimension()),
        neighbor_count: neighbor_count_to_u32(layers[0].len()),
        layer_count: layer_count_to_u32(layers.len()),
    };
    let link_bytes = layers.iter().fold(0usize, |total, layer| {
        total
            .checked_add(size_of::<u32>())
            .and_then(|total| total.checked_add(layer.len() * size_of::<u32>()))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "HNSW layer links overflow page-record storage",
                )
            })
    });
    let mut payload = Vec::with_capacity(
        size_of::<HnswVectorRecordHeader>()
            + record.vector.dimension() * size_of::<f32>()
            + link_bytes,
    );
    // SAFETY: `HnswVectorRecordHeader` is `repr(C)` plain data and `header`
    // lives until the bytes are copied into the owned payload vector.
    let header_bytes = unsafe {
        slice::from_raw_parts(
            ptr::addr_of!(header).cast::<u8>(),
            size_of::<HnswVectorRecordHeader>(),
        )
    };
    payload.extend_from_slice(header_bytes);
    for value in record.vector.as_slice() {
        payload.extend_from_slice(&value.to_ne_bytes());
    }
    for layer in layers {
        payload.extend_from_slice(&neighbor_count_to_u32(layer.len()).to_ne_bytes());
        for neighbor in layer {
            payload.extend_from_slice(&node_id_to_u32(*neighbor).to_ne_bytes());
        }
    }
    payload
}

pub(super) unsafe fn decode_hnsw_vector_record(
    item: *const u8,
    item_len: usize,
) -> HnswVectorRecord {
    if item_len < size_of::<HnswVectorRecordHeader>() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("HNSW vector record is too short: {item_len} bytes"),
        );
    }
    // SAFETY: Callers pass a pointer to a page item or owned payload encoded by
    // `encode_hnsw_vector_record`; unaligned reads avoid alignment assumptions.
    let header = unsafe { ptr::read_unaligned(item.cast::<HnswVectorRecordHeader>()) };
    let dimensions = header.dimensions as usize;
    let vector_bytes = match dimensions.checked_mul(size_of::<f32>()) {
        Some(bytes) => bytes,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW vector dimensions overflow record size: {dimensions}"),
        ),
    };
    let vector_end = match size_of::<HnswVectorRecordHeader>().checked_add(vector_bytes) {
        Some(bytes) => bytes,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW vector record size overflows usize",
        ),
    };
    if item_len < vector_end {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!(
                "HNSW vector record is truncated before vector payload: expected {vector_end} bytes, got {item_len}"
            ),
        );
    }
    let values = (0..dimensions)
        .map(|index| {
            // SAFETY: The encoded record contains `dimensions` contiguous f32
            // values immediately after the fixed header.
            let value = unsafe {
                ptr::read_unaligned(
                    item.add(size_of::<HnswVectorRecordHeader>() + index * size_of::<f32>())
                        .cast::<f32>(),
                )
            };
            f32::from_ne_bytes(value.to_ne_bytes())
        })
        .collect();
    let vector = match DenseVector::new(values) {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let layer_count = header.layer_count as usize;
    if layer_count == 0 || layer_count > MAX_GRAPH_LAYERS {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("HNSW vector record has invalid layer count: {layer_count}"),
        );
    }
    let mut offset = vector_end;
    let mut layers = Vec::with_capacity(layer_count);
    for layer_index in 0..layer_count {
        let count_end = checked_record_end(offset, size_of::<u32>(), item_len, "layer count");
        // SAFETY: `checked_record_end` proved that the four-byte count lies in
        // the caller-provided item span; unaligned reads are required for page data.
        let neighbor_count =
            unsafe { ptr::read_unaligned(item.add(offset).cast::<u32>()) } as usize;
        offset = count_end;
        if neighbor_count > MAX_GRAPH_NEIGHBORS_PER_LAYER {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("HNSW layer {layer_index} has too many neighbors: {neighbor_count}"),
            );
        }
        let neighbor_bytes = neighbor_count
            .checked_mul(size_of::<u32>())
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    format!("HNSW layer {layer_index} neighbor bytes overflow record size"),
                )
            });
        let neighbors_end = checked_record_end(offset, neighbor_bytes, item_len, "layer links");
        let mut neighbors = Vec::with_capacity(neighbor_count);
        for index in 0..neighbor_count {
            // SAFETY: `neighbors_end` proves every fixed-width id is in bounds.
            let neighbor = unsafe {
                ptr::read_unaligned(item.add(offset + index * size_of::<u32>()).cast::<u32>())
            };
            neighbors.push(HnswNodeId::new(neighbor as usize));
        }
        offset = neighbors_end;
        layers.push(neighbors);
    }
    if item_len != offset {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!(
                "HNSW vector record has {} trailing bytes",
                item_len - offset
            ),
        );
    }
    if header.neighbor_count as usize != layers[0].len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW vector record base-neighbor count disagrees with layer zero",
        );
    }
    let base_neighbors = layers[0].clone();
    HnswVectorRecord {
        node_id: HnswNodeId::new(header.node_id as usize),
        heap_tid: header.heap_tid,
        vector,
        base_neighbors,
        layers,
    }
}

pub(super) fn hnsw_vector_record_from_snapshot(
    snapshot: &HnswGraphNodeSnapshot,
) -> HnswVectorRecord {
    HnswVectorRecord {
        node_id: snapshot.node_id(),
        heap_tid: snapshot.point_id().get(),
        vector: snapshot.vector().clone(),
        base_neighbors: snapshot.base_neighbors().to_vec(),
        layers: snapshot.layers().to_vec(),
    }
}

pub(super) fn hnsw_graph_snapshot_from_record(record: HnswVectorRecord) -> HnswGraphNodeSnapshot {
    let point_id = hnsw_graph_point_id(&record);
    let layers = if record.layers.is_empty() {
        vec![record.base_neighbors]
    } else {
        record.layers
    };
    HnswGraphNodeSnapshot::from_layers(record.node_id, point_id, record.vector, layers)
}

fn record_layers(record: &HnswVectorRecord) -> &[Vec<HnswNodeId>] {
    if record.layers.is_empty() {
        slice::from_ref(&record.base_neighbors)
    } else {
        &record.layers
    }
}

fn checked_record_end(offset: usize, width: usize, item_len: usize, part: &str) -> usize {
    let end = offset.checked_add(width).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW {part} offset overflows record size"),
        )
    });
    if end > item_len {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("HNSW vector record is truncated in {part}"),
        );
    }
    end
}

fn dimension_to_u32(dimension: usize) -> u32 {
    match u32::try_from(dimension) {
        Ok(dimension) => dimension,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("vector dimensions exceed HNSW page-record storage: {dimension}"),
        ),
    }
}

fn node_id_to_u32(node_id: HnswNodeId) -> u32 {
    match u32::try_from(node_id.get()) {
        Ok(node_id) => node_id,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "HNSW node id exceeds page-record storage: {}",
                node_id.get()
            ),
        ),
    }
}

#[allow(dead_code, reason = "used by the retained pre-v6 adjacency encoder")]
fn layer_to_u32(layer: LayerIndex) -> u32 {
    match u32::try_from(layer.get()) {
        Ok(layer) => layer,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW layer exceeds page-record storage: {}", layer.get()),
        ),
    }
}

fn neighbor_count_to_u32(neighbor_count: usize) -> u32 {
    match u32::try_from(neighbor_count) {
        Ok(neighbor_count) => neighbor_count,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW neighbor count exceeds page-record storage: {neighbor_count}"),
        ),
    }
}

fn layer_count_to_u32(layer_count: usize) -> u32 {
    match u32::try_from(layer_count) {
        Ok(layer_count) if (1..=MAX_GRAPH_LAYERS).contains(&(layer_count as usize)) => layer_count,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("HNSW layer count exceeds page-record storage: {layer_count}"),
        ),
    }
}
