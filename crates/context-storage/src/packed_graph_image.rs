//! Contiguous shared-memory image format for packed HNSW graph generations.
//!
//! The image is the wire format of the hybrid serving model's shared base
//! generation: one backend encodes a packed graph into a single byte image
//! inside a dynamic shared memory segment, and every other backend attaches a
//! zero-copy read view. Like every pgContext acceleration artifact, the image
//! is rebuildable cache material — PostgreSQL index pages stay authoritative.
//!
//! # Image Format Versions
//!
//! All integer fields are little-endian. The fixed header is 64 bytes:
//!
//! | Offset | Size | Field |
//! |---:|---:|---|
//! | 0 | 8 | magic bytes `PGCTXPKG` |
//! | 8 | 4 | image format version (`1` or `2`) |
//! | 12 | 4 | endian marker `0x01020304` |
//! | 16 | 4 | vector dimensions |
//! | 20 | 4 | reserved, currently zero |
//! | 24 | 8 | node count |
//! | 32 | 8 | layer count |
//! | 40 | 8 | neighbor count |
//! | 48 | 8 | vector value count (`node count x dimensions`) |
//! | 56 | 8 | FNV-1a checksum (header with zero checksum, then payload) |
//!
//! Sections follow the header in order, each starting 8-byte aligned:
//! nodes (32 bytes each), layers (16 bytes each), neighbors (8 bytes each),
//! then vector values (4-byte IEEE-754 `f32`, last so its offset stays
//! 4-aligned without padding bookkeeping).
//!
//! Version 2 extends the header to 96 bytes and appends an optional trained
//! quantization-codebook block followed by fixed-size per-node codes. The
//! header records quantization mode, code length, codebook length, and total
//! code bytes. Full vectors remain present as recovery/oracle data.
//!
//! Integer sections are decoded by copy on access, so the view has no
//! alignment requirement for them. The vector section is reinterpreted
//! zero-copy as `&[f32]` because distance kernels need contiguous floats;
//! `attach` fails with [`PackedGraphImageError::MisalignedVectors`] when the
//! backing buffer does not give that section 4-byte alignment (dynamic
//! shared memory segments are page-aligned, so this only affects arbitrary
//! heap buffers — copy those through [`AlignedImageBuf`] first).

#![allow(
    unsafe_code,
    reason = "zero-copy float views and aligned copies validate length and alignment before every raw-pointer construction"
)]

use core::{
    fmt,
    mem::{size_of, size_of_val},
};

use crate::hnsw_graph_payload::{
    HnswGraphQuantization, HnswGraphQuantizationCodebook, QUANTIZATION_NONE,
    decode_quantization_codebook, encode_quantization_codebook, quantization_mode,
    validate_quantization, validate_quantized_code,
};

const PACKED_GRAPH_IMAGE_MAGIC: [u8; 8] = *b"PGCTXPKG";
const PACKED_GRAPH_IMAGE_VERSION_V1: u32 = 1;
/// Current packed graph image version.
pub const CURRENT_PACKED_GRAPH_IMAGE_VERSION: u32 = 2;
/// Oldest packed graph image version accepted by the reader.
pub const MIN_READABLE_PACKED_GRAPH_IMAGE_VERSION: u32 = 1;
const PACKED_GRAPH_IMAGE_ENDIAN_MARKER: u32 = 0x0102_0304;
const PACKED_GRAPH_IMAGE_HEADER_LEN_V1: usize = 64;
const PACKED_GRAPH_IMAGE_HEADER_LEN_V2: usize = 96;
const PACKED_GRAPH_IMAGE_NODE_LEN: usize = 32;
const PACKED_GRAPH_IMAGE_LAYER_LEN: usize = 16;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One node entry inside a packed graph image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedGraphImageNode {
    /// Authoritative pgContext point id (heap TID encoding).
    pub point_id: u64,
    /// Start offset of this node's vector inside the vector section, in
    /// values (not bytes).
    pub vector_start: u64,
    /// Start index of this node's first layer inside the layer section.
    pub layers_start: u64,
    /// Number of layers this node participates in; never zero in a valid
    /// image.
    pub layer_count: u64,
}

/// One layer entry inside a packed graph image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedGraphImageLayer {
    /// Start index of this layer's neighbor ids inside the neighbor section.
    pub neighbors_start: u64,
    /// Number of neighbor ids in this layer.
    pub neighbor_count: u64,
}

/// Validation and decoding failures for packed graph images.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackedGraphImageError {
    /// The buffer is shorter than the fixed header.
    TruncatedHeader,
    /// The magic bytes are not `PGCTXPKG`.
    BadMagic,
    /// The version field is not a supported image version.
    UnsupportedVersion(u32),
    /// The endian marker does not match the writer's byte order contract.
    BadEndianMarker,
    /// The header's section counts do not fit the buffer length.
    TruncatedPayload,
    /// The checksum does not match the header and payload bytes.
    ChecksumMismatch,
    /// The vector count is not `node count x dimensions`.
    InconsistentVectorCount,
    /// A section count overflows addressable memory on this platform.
    CountOverflow,
    /// A node references vectors, layers, or neighbors out of bounds, has
    /// zero layers, or a neighbor id is not a valid node id.
    CorruptTopology,
    /// The vector section is not 4-byte aligned in this buffer.
    MisalignedVectors,
    /// Encoding input arrays disagree with each other.
    InconsistentInput,
    /// Quantization metadata or per-node codes are invalid.
    InvalidQuantization,
}

impl fmt::Display for PackedGraphImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader => write!(formatter, "packed graph image header is truncated"),
            Self::BadMagic => write!(formatter, "packed graph image magic bytes are wrong"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported packed graph image version {version}"
                )
            }
            Self::BadEndianMarker => {
                write!(formatter, "packed graph image endian marker is wrong")
            }
            Self::TruncatedPayload => {
                write!(formatter, "packed graph image payload is truncated")
            }
            Self::ChecksumMismatch => {
                write!(formatter, "packed graph image checksum mismatch")
            }
            Self::InconsistentVectorCount => write!(
                formatter,
                "packed graph image vector count does not equal nodes x dimensions"
            ),
            Self::CountOverflow => {
                write!(
                    formatter,
                    "packed graph image counts overflow this platform"
                )
            }
            Self::CorruptTopology => {
                write!(formatter, "packed graph image topology is out of bounds")
            }
            Self::MisalignedVectors => write!(
                formatter,
                "packed graph image vector section is not 4-byte aligned"
            ),
            Self::InconsistentInput => {
                write!(
                    formatter,
                    "packed graph image encoding input is inconsistent"
                )
            }
            Self::InvalidQuantization => {
                write!(formatter, "packed graph image quantization is invalid")
            }
        }
    }
}

impl std::error::Error for PackedGraphImageError {}

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut raw = [0_u8; 4];
    raw.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(raw)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

/// Returns the encoded image size in bytes for the given section counts, or
/// `None` when the total overflows `usize`.
#[must_use]
pub fn packed_graph_image_len(
    node_count: usize,
    layer_count: usize,
    neighbor_count: usize,
    vector_count: usize,
) -> Option<usize> {
    let nodes = node_count.checked_mul(PACKED_GRAPH_IMAGE_NODE_LEN)?;
    let layers = layer_count.checked_mul(PACKED_GRAPH_IMAGE_LAYER_LEN)?;
    let neighbors = neighbor_count.checked_mul(size_of::<u64>())?;
    let vectors = vector_count.checked_mul(size_of::<f32>())?;
    PACKED_GRAPH_IMAGE_HEADER_LEN_V1
        .checked_add(nodes)?
        .checked_add(layers)?
        .checked_add(neighbors)?
        .checked_add(vectors)
}

/// Encodes a packed graph into a fresh image buffer.
///
/// # Errors
///
/// Returns [`PackedGraphImageError::InconsistentInput`] when the arrays do
/// not describe one another (vector count, layer spans, neighbor spans, or
/// neighbor ids out of range), and
/// [`PackedGraphImageError::CountOverflow`] when the image size overflows.
pub fn encode_packed_graph_image(
    dimensions: u32,
    nodes: &[PackedGraphImageNode],
    layers: &[PackedGraphImageLayer],
    neighbors: &[u64],
    vectors: &[f32],
) -> Result<Vec<u8>, PackedGraphImageError> {
    validate_topology(
        dimensions,
        nodes.len(),
        layers.len(),
        neighbors.len(),
        vectors.len(),
        |index| nodes.get(index).copied(),
        |index| layers.get(index).copied(),
        |index| neighbors.get(index).copied(),
    )?;
    let total = packed_graph_image_len(nodes.len(), layers.len(), neighbors.len(), vectors.len())
        .ok_or(PackedGraphImageError::CountOverflow)?;
    let mut image = Vec::with_capacity(total);
    image.extend_from_slice(&PACKED_GRAPH_IMAGE_MAGIC);
    image.extend_from_slice(&PACKED_GRAPH_IMAGE_VERSION_V1.to_le_bytes());
    image.extend_from_slice(&PACKED_GRAPH_IMAGE_ENDIAN_MARKER.to_le_bytes());
    image.extend_from_slice(&dimensions.to_le_bytes());
    image.extend_from_slice(&0_u32.to_le_bytes());
    image.extend_from_slice(&(nodes.len() as u64).to_le_bytes());
    image.extend_from_slice(&(layers.len() as u64).to_le_bytes());
    image.extend_from_slice(&(neighbors.len() as u64).to_le_bytes());
    image.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    image.extend_from_slice(&0_u64.to_le_bytes());
    for node in nodes {
        image.extend_from_slice(&node.point_id.to_le_bytes());
        image.extend_from_slice(&node.vector_start.to_le_bytes());
        image.extend_from_slice(&node.layers_start.to_le_bytes());
        image.extend_from_slice(&node.layer_count.to_le_bytes());
    }
    for layer in layers {
        image.extend_from_slice(&layer.neighbors_start.to_le_bytes());
        image.extend_from_slice(&layer.neighbor_count.to_le_bytes());
    }
    for neighbor in neighbors {
        image.extend_from_slice(&neighbor.to_le_bytes());
    }
    for value in vectors {
        image.extend_from_slice(&value.to_le_bytes());
    }
    debug_assert_eq!(image.len(), total);
    let checksum = fnv1a(FNV_OFFSET_BASIS, &image);
    image[56..64].copy_from_slice(&checksum.to_le_bytes());
    Ok(image)
}

/// Encodes a version-2 packed graph image with optional quantized navigation.
///
/// # Errors
///
/// Returns [`PackedGraphImageError`] when topology, vector counts,
/// quantization metadata, code counts, or encoded lengths are invalid.
pub fn encode_packed_graph_image_v2(
    dimensions: u32,
    nodes: &[PackedGraphImageNode],
    layers: &[PackedGraphImageLayer],
    neighbors: &[u64],
    vectors: &[f32],
    quantization: Option<&HnswGraphQuantization>,
) -> Result<Vec<u8>, PackedGraphImageError> {
    validate_topology(
        dimensions,
        nodes.len(),
        layers.len(),
        neighbors.len(),
        vectors.len(),
        |index| nodes.get(index).copied(),
        |index| layers.get(index).copied(),
        |index| neighbors.get(index).copied(),
    )?;
    let dimensions_usize = dimensions as usize;
    let (mode, code_len, codebook_bytes, code_bytes) = match quantization {
        Some(quantization) => {
            validate_quantization(quantization, nodes.len(), dimensions_usize)
                .map_err(|_| PackedGraphImageError::InvalidQuantization)?;
            let codebook_bytes = encode_quantization_codebook(quantization.codebook())
                .map_err(|_| PackedGraphImageError::InvalidQuantization)?;
            if quantization.codebook().code_len() == 0 {
                return Err(PackedGraphImageError::InvalidQuantization);
            }
            let code_bytes = quantization
                .codes()
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            (
                quantization_mode(quantization.codebook()),
                quantization.codebook().code_len(),
                codebook_bytes,
                code_bytes,
            )
        }
        None => (QUANTIZATION_NONE, 0, Vec::new(), Vec::new()),
    };
    let total = packed_graph_image_len_v2(
        nodes.len(),
        layers.len(),
        neighbors.len(),
        vectors.len(),
        codebook_bytes.len(),
        code_bytes.len(),
    )
    .ok_or(PackedGraphImageError::CountOverflow)?;
    let code_len_u32 = u32::try_from(code_len).map_err(|_| PackedGraphImageError::CountOverflow)?;
    let mut image = Vec::with_capacity(total);
    image.extend_from_slice(&PACKED_GRAPH_IMAGE_MAGIC);
    image.extend_from_slice(&CURRENT_PACKED_GRAPH_IMAGE_VERSION.to_le_bytes());
    image.extend_from_slice(&PACKED_GRAPH_IMAGE_ENDIAN_MARKER.to_le_bytes());
    image.extend_from_slice(&dimensions.to_le_bytes());
    image.extend_from_slice(&0_u32.to_le_bytes());
    image.extend_from_slice(&(nodes.len() as u64).to_le_bytes());
    image.extend_from_slice(&(layers.len() as u64).to_le_bytes());
    image.extend_from_slice(&(neighbors.len() as u64).to_le_bytes());
    image.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    image.extend_from_slice(&0_u64.to_le_bytes());
    image.extend_from_slice(&mode.to_le_bytes());
    image.extend_from_slice(&code_len_u32.to_le_bytes());
    image.extend_from_slice(&(codebook_bytes.len() as u64).to_le_bytes());
    image.extend_from_slice(&(code_bytes.len() as u64).to_le_bytes());
    image.extend_from_slice(&0_u64.to_le_bytes());
    encode_sections(&mut image, nodes, layers, neighbors, vectors);
    image.extend_from_slice(&codebook_bytes);
    image.extend_from_slice(&code_bytes);
    debug_assert_eq!(image.len(), total);
    let checksum = fnv1a(FNV_OFFSET_BASIS, &image);
    image[56..64].copy_from_slice(&checksum.to_le_bytes());
    Ok(image)
}

fn packed_graph_image_len_v2(
    node_count: usize,
    layer_count: usize,
    neighbor_count: usize,
    vector_count: usize,
    codebook_bytes: usize,
    code_bytes: usize,
) -> Option<usize> {
    let nodes = node_count.checked_mul(PACKED_GRAPH_IMAGE_NODE_LEN)?;
    let layers = layer_count.checked_mul(PACKED_GRAPH_IMAGE_LAYER_LEN)?;
    let neighbors = neighbor_count.checked_mul(size_of::<u64>())?;
    let vectors = vector_count.checked_mul(size_of::<f32>())?;
    PACKED_GRAPH_IMAGE_HEADER_LEN_V2
        .checked_add(nodes)?
        .checked_add(layers)?
        .checked_add(neighbors)?
        .checked_add(vectors)?
        .checked_add(codebook_bytes)?
        .checked_add(code_bytes)
}

fn encode_sections(
    image: &mut Vec<u8>,
    nodes: &[PackedGraphImageNode],
    layers: &[PackedGraphImageLayer],
    neighbors: &[u64],
    vectors: &[f32],
) {
    for node in nodes {
        image.extend_from_slice(&node.point_id.to_le_bytes());
        image.extend_from_slice(&node.vector_start.to_le_bytes());
        image.extend_from_slice(&node.layers_start.to_le_bytes());
        image.extend_from_slice(&node.layer_count.to_le_bytes());
    }
    for layer in layers {
        image.extend_from_slice(&layer.neighbors_start.to_le_bytes());
        image.extend_from_slice(&layer.neighbor_count.to_le_bytes());
    }
    for neighbor in neighbors {
        image.extend_from_slice(&neighbor.to_le_bytes());
    }
    for value in vectors {
        image.extend_from_slice(&value.to_le_bytes());
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_topology(
    dimensions: u32,
    node_count: usize,
    layer_count: usize,
    neighbor_count: usize,
    vector_count: usize,
    node_at: impl Fn(usize) -> Option<PackedGraphImageNode>,
    layer_at: impl Fn(usize) -> Option<PackedGraphImageLayer>,
    neighbor_at: impl Fn(usize) -> Option<u64>,
) -> Result<(), PackedGraphImageError> {
    let dimensions = dimensions as usize;
    let expected_vectors = node_count
        .checked_mul(dimensions)
        .ok_or(PackedGraphImageError::CountOverflow)?;
    if vector_count != expected_vectors {
        return Err(PackedGraphImageError::InconsistentVectorCount);
    }
    let mut expected_vector_start = 0_u64;
    let mut expected_layers_start = 0_u64;
    let mut expected_neighbors_start = 0_u64;
    for index in 0..node_count {
        let node = node_at(index).ok_or(PackedGraphImageError::CorruptTopology)?;
        if node.layer_count == 0
            || node.vector_start != expected_vector_start
            || node.layers_start != expected_layers_start
        {
            return Err(PackedGraphImageError::CorruptTopology);
        }
        expected_vector_start = expected_vector_start
            .checked_add(dimensions as u64)
            .ok_or(PackedGraphImageError::CountOverflow)?;
        let layers_end = node
            .layers_start
            .checked_add(node.layer_count)
            .ok_or(PackedGraphImageError::CountOverflow)?;
        if layers_end > layer_count as u64 {
            return Err(PackedGraphImageError::CorruptTopology);
        }
        for layer_index in node.layers_start..layers_end {
            let layer = layer_at(
                usize::try_from(layer_index).map_err(|_| PackedGraphImageError::CountOverflow)?,
            )
            .ok_or(PackedGraphImageError::CorruptTopology)?;
            if layer.neighbors_start != expected_neighbors_start {
                return Err(PackedGraphImageError::CorruptTopology);
            }
            expected_neighbors_start = expected_neighbors_start
                .checked_add(layer.neighbor_count)
                .ok_or(PackedGraphImageError::CountOverflow)?;
        }
        expected_layers_start = layers_end;
    }
    if expected_layers_start != layer_count as u64
        || expected_neighbors_start != neighbor_count as u64
    {
        return Err(PackedGraphImageError::CorruptTopology);
    }
    for index in 0..neighbor_count {
        let neighbor = neighbor_at(index).ok_or(PackedGraphImageError::CorruptTopology)?;
        if neighbor >= node_count as u64 {
            return Err(PackedGraphImageError::CorruptTopology);
        }
    }
    Ok(())
}

/// Zero-copy validated read view over a packed graph image.
pub struct PackedGraphImageView<'a> {
    bytes: &'a [u8],
    header_len: usize,
    dimensions: usize,
    node_count: usize,
    layers_offset: usize,
    neighbors_offset: usize,
    vectors: &'a [f32],
    quantization_codebook: Option<HnswGraphQuantizationCodebook>,
    code_len: usize,
    codes: &'a [u8],
}

impl<'a> PackedGraphImageView<'a> {
    /// Validates and attaches a read view over an encoded image.
    ///
    /// Full structural validation runs once here (header, counts, section
    /// bounds, topology referential integrity, vector alignment), so the
    /// accessors can stay branch-light. `verify_checksum` additionally
    /// hashes the whole buffer; skip it for shared-memory attaches where
    /// the publisher wrote the image under a lock in the same server
    /// lifetime, and keep it for images read from files or the network.
    ///
    /// # Errors
    ///
    /// Returns the specific [`PackedGraphImageError`] describing the first
    /// validation failure.
    pub fn attach(bytes: &'a [u8], verify_checksum: bool) -> Result<Self, PackedGraphImageError> {
        if bytes.len() < PACKED_GRAPH_IMAGE_HEADER_LEN_V1 {
            return Err(PackedGraphImageError::TruncatedHeader);
        }
        if bytes[0..8] != PACKED_GRAPH_IMAGE_MAGIC {
            return Err(PackedGraphImageError::BadMagic);
        }
        let version = read_u32(bytes, 8);
        let header_len = match version {
            PACKED_GRAPH_IMAGE_VERSION_V1 => PACKED_GRAPH_IMAGE_HEADER_LEN_V1,
            CURRENT_PACKED_GRAPH_IMAGE_VERSION => PACKED_GRAPH_IMAGE_HEADER_LEN_V2,
            _ => return Err(PackedGraphImageError::UnsupportedVersion(version)),
        };
        if bytes.len() < header_len {
            return Err(PackedGraphImageError::TruncatedHeader);
        }
        if read_u32(bytes, 12) != PACKED_GRAPH_IMAGE_ENDIAN_MARKER {
            return Err(PackedGraphImageError::BadEndianMarker);
        }
        let dimensions_raw = read_u32(bytes, 16);
        let node_count = usize::try_from(read_u64(bytes, 24))
            .map_err(|_| PackedGraphImageError::CountOverflow)?;
        let layer_count = usize::try_from(read_u64(bytes, 32))
            .map_err(|_| PackedGraphImageError::CountOverflow)?;
        let neighbor_count = usize::try_from(read_u64(bytes, 40))
            .map_err(|_| PackedGraphImageError::CountOverflow)?;
        let vector_count = usize::try_from(read_u64(bytes, 48))
            .map_err(|_| PackedGraphImageError::CountOverflow)?;
        let (mode, code_len, codebook_len, codes_len) = if version == PACKED_GRAPH_IMAGE_VERSION_V1
        {
            (QUANTIZATION_NONE, 0, 0, 0)
        } else {
            if read_u64(bytes, 88) != 0 {
                return Err(PackedGraphImageError::InvalidQuantization);
            }
            (
                read_u32(bytes, 64),
                read_u32(bytes, 68) as usize,
                usize::try_from(read_u64(bytes, 72))
                    .map_err(|_| PackedGraphImageError::CountOverflow)?,
                usize::try_from(read_u64(bytes, 80))
                    .map_err(|_| PackedGraphImageError::CountOverflow)?,
            )
        };
        let expected_codes_len = node_count
            .checked_mul(code_len)
            .ok_or(PackedGraphImageError::CountOverflow)?;
        if codes_len != expected_codes_len {
            return Err(PackedGraphImageError::InvalidQuantization);
        }
        let expected_len = if version == PACKED_GRAPH_IMAGE_VERSION_V1 {
            packed_graph_image_len(node_count, layer_count, neighbor_count, vector_count)
        } else {
            packed_graph_image_len_v2(
                node_count,
                layer_count,
                neighbor_count,
                vector_count,
                codebook_len,
                codes_len,
            )
        }
        .ok_or(PackedGraphImageError::CountOverflow)?;
        if bytes.len() != expected_len {
            return Err(PackedGraphImageError::TruncatedPayload);
        }
        if verify_checksum {
            let stored = read_u64(bytes, 56);
            let mut header = bytes[..header_len].to_vec();
            header[56..64].fill(0);
            let computed = fnv1a(fnv1a(FNV_OFFSET_BASIS, &header), &bytes[header_len..]);
            if stored != computed {
                return Err(PackedGraphImageError::ChecksumMismatch);
            }
        }
        let nodes_offset = header_len;
        let layers_offset = nodes_offset + node_count * PACKED_GRAPH_IMAGE_NODE_LEN;
        let neighbors_offset = layers_offset + layer_count * PACKED_GRAPH_IMAGE_LAYER_LEN;
        let vectors_offset = neighbors_offset + neighbor_count * size_of::<u64>();
        let vectors_end = vectors_offset + vector_count * size_of::<f32>();
        let codebook_end = vectors_end + codebook_len;
        let vectors_bytes = &bytes[vectors_offset..vectors_end];
        if vectors_bytes.as_ptr().align_offset(size_of::<f32>()) != 0 {
            return Err(PackedGraphImageError::MisalignedVectors);
        }
        // SAFETY: the region is exactly `vector_count * 4` bytes (validated
        // by the length equality above), starts 4-byte aligned (checked
        // immediately above), lives as long as `bytes`, and every f32 bit
        // pattern is a valid value for reads.
        let vectors = unsafe {
            core::slice::from_raw_parts(vectors_bytes.as_ptr().cast::<f32>(), vector_count)
        };
        let quantization_codebook = decode_quantization_codebook(
            mode,
            dimensions_raw as usize,
            code_len,
            &bytes[vectors_end..codebook_end],
        )
        .map_err(|_| PackedGraphImageError::InvalidQuantization)?;
        if quantization_codebook.is_some() && code_len == 0 {
            return Err(PackedGraphImageError::InvalidQuantization);
        }
        let codes = &bytes[codebook_end..];
        if let Some(codebook) = &quantization_codebook {
            for (node_index, code) in codes.chunks_exact(code_len).enumerate() {
                validate_quantized_code(codebook, node_index, code)
                    .map_err(|_| PackedGraphImageError::InvalidQuantization)?;
            }
        }
        let view = Self {
            bytes,
            header_len,
            dimensions: dimensions_raw as usize,
            node_count,
            layers_offset,
            neighbors_offset,
            vectors,
            quantization_codebook,
            code_len,
            codes,
        };
        validate_topology(
            dimensions_raw,
            node_count,
            layer_count,
            neighbor_count,
            vector_count,
            |index| view.node(index),
            |index| view.layer(index),
            |index| view.neighbor_at(index),
        )?;
        Ok(view)
    }

    /// Returns the vector dimensionality recorded in the header.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the number of nodes in the image.
    #[must_use]
    pub const fn node_count(&self) -> usize {
        self.node_count
    }

    /// Returns the node entry at `index`, or `None` past the end.
    #[must_use]
    pub fn node(&self, index: usize) -> Option<PackedGraphImageNode> {
        if index >= self.node_count {
            return None;
        }
        let offset = self.header_len + index * PACKED_GRAPH_IMAGE_NODE_LEN;
        Some(PackedGraphImageNode {
            point_id: read_u64(self.bytes, offset),
            vector_start: read_u64(self.bytes, offset + 8),
            layers_start: read_u64(self.bytes, offset + 16),
            layer_count: read_u64(self.bytes, offset + 24),
        })
    }

    fn layer(&self, index: usize) -> Option<PackedGraphImageLayer> {
        let offset = self
            .layers_offset
            .checked_add(index.checked_mul(PACKED_GRAPH_IMAGE_LAYER_LEN)?)?;
        if offset + PACKED_GRAPH_IMAGE_LAYER_LEN > self.neighbors_offset {
            return None;
        }
        Some(PackedGraphImageLayer {
            neighbors_start: read_u64(self.bytes, offset),
            neighbor_count: read_u64(self.bytes, offset + 8),
        })
    }

    fn neighbor_at(&self, index: usize) -> Option<u64> {
        let offset = self
            .neighbors_offset
            .checked_add(index.checked_mul(size_of::<u64>())?)?;
        if offset + size_of::<u64>() > self.bytes.len() - size_of_val(self.vectors) {
            return None;
        }
        Some(read_u64(self.bytes, offset))
    }

    /// Returns the vector values for a node entry returned by [`Self::node`].
    #[must_use]
    pub fn node_vector(&self, node: PackedGraphImageNode) -> Option<&'a [f32]> {
        let start = usize::try_from(node.vector_start).ok()?;
        let end = start.checked_add(self.dimensions)?;
        self.vectors.get(start..end)
    }

    /// Returns the optional trained navigation codebook.
    #[must_use]
    pub const fn quantization_codebook(&self) -> Option<&HnswGraphQuantizationCodebook> {
        self.quantization_codebook.as_ref()
    }

    /// Returns the zero-copy quantized code for a node index.
    #[must_use]
    pub fn node_code(&self, index: usize) -> Option<&'a [u8]> {
        if index >= self.node_count || self.code_len == 0 {
            return None;
        }
        let start = index.checked_mul(self.code_len)?;
        self.codes.get(start..start + self.code_len)
    }

    /// Returns an iterator over one layer's neighbor ids for a node entry.
    ///
    /// Layer indexes at or beyond the node's `layer_count` return `None`,
    /// matching the packed in-memory accessor contract.
    #[must_use]
    pub fn neighbors(
        &self,
        node: PackedGraphImageNode,
        layer_index: usize,
    ) -> Option<impl Iterator<Item = u64> + 'a> {
        if layer_index as u64 >= node.layer_count {
            return None;
        }
        let layer = self.layer(usize::try_from(node.layers_start).ok()? + layer_index)?;
        let start = self.neighbors_offset.checked_add(
            usize::try_from(layer.neighbors_start)
                .ok()?
                .checked_mul(8)?,
        )?;
        let count = usize::try_from(layer.neighbor_count).ok()?;
        let end = start.checked_add(count.checked_mul(8)?)?;
        let bytes = self.bytes.get(start..end)?;
        Some(bytes.chunks_exact(8).map(|chunk| {
            let mut raw = [0_u8; 8];
            raw.copy_from_slice(chunk);
            u64::from_le_bytes(raw)
        }))
    }
}

/// Owning 8-byte-aligned copy of an image, for callers whose source buffer
/// cannot guarantee the vector section's alignment (arbitrary `Vec<u8>`
/// heap buffers, network reads).
pub struct AlignedImageBuf {
    storage: Vec<u64>,
    len: usize,
}

impl AlignedImageBuf {
    /// Copies `bytes` into 8-byte-aligned storage.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let words = bytes.len().div_ceil(size_of::<u64>());
        let mut storage = vec![0_u64; words];
        // SAFETY: the destination allocation is `words * 8 >= bytes.len()`
        // bytes long, u64 storage may alias as bytes, and the ranges do not
        // overlap because `storage` is freshly allocated.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                storage.as_mut_ptr().cast::<u8>(),
                bytes.len(),
            );
        }
        Self {
            storage,
            len: bytes.len(),
        }
    }

    /// Returns the image bytes at guaranteed 8-byte base alignment.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: the storage allocation holds at least `self.len`
        // initialized bytes written by `from_bytes`, and u64 storage may
        // alias as bytes.
        unsafe { core::slice::from_raw_parts(self.storage.as_ptr().cast::<u8>(), self.len) }
    }
}
