//! Round-trip, corruption-rejection, and property tests for the packed
//! graph image codec.
#![allow(clippy::expect_used, clippy::cast_possible_truncation)]

use context_storage::{
    AlignedImageBuf, PackedGraphImageError, PackedGraphImageLayer, PackedGraphImageNode,
    PackedGraphImageView, encode_packed_graph_image, packed_graph_image_len,
};
use proptest::prelude::*;

/// A tiny two-node graph: node 0 has two layers, node 1 has one.
fn sample_graph() -> (
    u32,
    Vec<PackedGraphImageNode>,
    Vec<PackedGraphImageLayer>,
    Vec<u64>,
    Vec<f32>,
) {
    let dimensions = 3_u32;
    let nodes = vec![
        PackedGraphImageNode {
            point_id: 4242,
            vector_start: 0,
            layers_start: 0,
            layer_count: 2,
        },
        PackedGraphImageNode {
            point_id: 4243,
            vector_start: 3,
            layers_start: 2,
            layer_count: 1,
        },
    ];
    let layers = vec![
        PackedGraphImageLayer {
            neighbors_start: 0,
            neighbor_count: 1,
        },
        PackedGraphImageLayer {
            neighbors_start: 1,
            neighbor_count: 1,
        },
        PackedGraphImageLayer {
            neighbors_start: 2,
            neighbor_count: 1,
        },
    ];
    let neighbors = vec![1, 1, 0];
    let vectors = vec![0.25, -1.0, 0.5, 0.125, 2.0, -0.75];
    (dimensions, nodes, layers, neighbors, vectors)
}

fn encode_sample() -> Vec<u8> {
    let (dimensions, nodes, layers, neighbors, vectors) = sample_graph();
    encode_packed_graph_image(dimensions, &nodes, &layers, &neighbors, &vectors)
        .expect("sample graph should encode")
}

#[test]
fn round_trip_preserves_nodes_vectors_and_neighbors() {
    let (dimensions, nodes, _, _, vectors) = sample_graph();
    let image = AlignedImageBuf::from_bytes(&encode_sample());
    let view =
        PackedGraphImageView::attach(image.as_bytes(), true).expect("valid image should attach");

    assert_eq!(view.dimensions(), dimensions as usize);
    assert_eq!(view.node_count(), nodes.len());
    for (index, expected) in nodes.iter().enumerate() {
        let node = view.node(index).expect("node should exist");
        assert_eq!(node, *expected);
        let vector = view.node_vector(node).expect("vector should exist");
        let start = index * dimensions as usize;
        assert_eq!(vector, &vectors[start..start + dimensions as usize]);
    }
    let node0 = view.node(0).expect("node 0 exists");
    assert_eq!(
        view.neighbors(node0, 0)
            .expect("layer 0")
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        view.neighbors(node0, 1)
            .expect("layer 1")
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(view.neighbors(node0, 2).is_none(), "past layer_count");
    assert!(view.node(2).is_none(), "past node_count");
}

#[test]
fn image_len_matches_encoded_len() {
    let (_, nodes, layers, neighbors, vectors) = sample_graph();
    let encoded = encode_sample();
    assert_eq!(
        packed_graph_image_len(nodes.len(), layers.len(), neighbors.len(), vectors.len()),
        Some(encoded.len())
    );
}

#[test]
fn truncated_and_corrupted_images_are_rejected() {
    let encoded = encode_sample();

    let empty = AlignedImageBuf::from_bytes(&[]);
    assert_eq!(
        PackedGraphImageView::attach(empty.as_bytes(), true).err(),
        Some(PackedGraphImageError::TruncatedHeader)
    );

    let truncated = AlignedImageBuf::from_bytes(&encoded[..encoded.len() - 4]);
    assert_eq!(
        PackedGraphImageView::attach(truncated.as_bytes(), true).err(),
        Some(PackedGraphImageError::TruncatedPayload)
    );

    let mut bad_magic = encoded.clone();
    bad_magic[0] ^= 0xFF;
    let bad_magic = AlignedImageBuf::from_bytes(&bad_magic);
    assert_eq!(
        PackedGraphImageView::attach(bad_magic.as_bytes(), true).err(),
        Some(PackedGraphImageError::BadMagic)
    );

    let mut bad_version = encoded.clone();
    bad_version[8] = 99;
    let bad_version = AlignedImageBuf::from_bytes(&bad_version);
    assert_eq!(
        PackedGraphImageView::attach(bad_version.as_bytes(), true).err(),
        Some(PackedGraphImageError::UnsupportedVersion(99))
    );

    let mut flipped_payload = encoded.clone();
    let last = flipped_payload.len() - 1;
    flipped_payload[last] ^= 0x01;
    let flipped_payload = AlignedImageBuf::from_bytes(&flipped_payload);
    assert_eq!(
        PackedGraphImageView::attach(flipped_payload.as_bytes(), true).err(),
        Some(PackedGraphImageError::ChecksumMismatch)
    );
}

#[test]
fn corrupt_topology_is_rejected_even_without_checksum() {
    let mut encoded = encode_sample();
    // Node 0's layer_count lives at header(64) + 24; set it to zero and
    // refresh nothing else — attach without checksum must still reject.
    encoded[64 + 24..64 + 32].copy_from_slice(&0_u64.to_le_bytes());
    let image = AlignedImageBuf::from_bytes(&encoded);
    assert_eq!(
        PackedGraphImageView::attach(image.as_bytes(), false).err(),
        Some(PackedGraphImageError::CorruptTopology)
    );
}

#[test]
fn out_of_range_neighbor_id_is_rejected() {
    let (dimensions, nodes, layers, mut neighbors, vectors) = sample_graph();
    neighbors[0] = 7;
    assert_eq!(
        encode_packed_graph_image(dimensions, &nodes, &layers, &neighbors, &vectors).err(),
        Some(PackedGraphImageError::CorruptTopology)
    );
}

#[test]
fn inconsistent_vector_count_is_rejected() {
    let (dimensions, nodes, layers, neighbors, mut vectors) = sample_graph();
    vectors.pop();
    assert_eq!(
        encode_packed_graph_image(dimensions, &nodes, &layers, &neighbors, &vectors).err(),
        Some(PackedGraphImageError::InconsistentVectorCount)
    );
}

/// Generates a structurally valid random graph as flat arrays.
fn arbitrary_graph() -> impl Strategy<
    Value = (
        u32,
        Vec<PackedGraphImageNode>,
        Vec<PackedGraphImageLayer>,
        Vec<u64>,
        Vec<f32>,
    ),
> {
    (1_u32..8, 1_usize..24).prop_flat_map(|(dimensions, node_count)| {
        (
            proptest::collection::vec(1_usize..4, node_count),
            proptest::collection::vec(0_usize..6, node_count * 4),
            proptest::collection::vec(-1000.0_f32..1000.0, node_count * dimensions as usize),
        )
            .prop_map(move |(layer_counts, neighbor_seeds, vectors)| {
                let mut nodes = Vec::with_capacity(node_count);
                let mut layers = Vec::new();
                let mut neighbors = Vec::new();
                let mut seed_cursor = 0;
                for (index, layer_count) in layer_counts.iter().enumerate() {
                    nodes.push(PackedGraphImageNode {
                        point_id: 10_000 + index as u64,
                        vector_start: (index * dimensions as usize) as u64,
                        layers_start: layers.len() as u64,
                        layer_count: *layer_count as u64,
                    });
                    for _ in 0..*layer_count {
                        let count = neighbor_seeds[seed_cursor % neighbor_seeds.len()];
                        seed_cursor += 1;
                        layers.push(PackedGraphImageLayer {
                            neighbors_start: neighbors.len() as u64,
                            neighbor_count: count as u64,
                        });
                        for slot in 0..count {
                            neighbors.push(((index + slot + 1) % node_count) as u64);
                        }
                    }
                }
                (dimensions, nodes, layers, neighbors, vectors)
            })
    })
}

proptest! {
    #[test]
    fn generated_graphs_round_trip(
        (dimensions, nodes, layers, neighbors, vectors) in arbitrary_graph()
    ) {
        let encoded = encode_packed_graph_image(
            dimensions, &nodes, &layers, &neighbors, &vectors,
        ).expect("generated graphs are structurally valid");
        let image = AlignedImageBuf::from_bytes(&encoded);
        let view = PackedGraphImageView::attach(image.as_bytes(), true)
            .expect("encoded image should attach");
        prop_assert_eq!(view.node_count(), nodes.len());
        for (index, expected) in nodes.iter().enumerate() {
            let node = view.node(index).expect("node exists");
            prop_assert_eq!(node, *expected);
            let vector = view.node_vector(node).expect("vector exists");
            let start = index * dimensions as usize;
            prop_assert_eq!(vector, &vectors[start..start + dimensions as usize]);
            for layer_index in 0..node.layer_count as usize {
                let decoded: Vec<u64> = view
                    .neighbors(node, layer_index)
                    .expect("layer exists")
                    .collect();
                let layer = &layers[node.layers_start as usize + layer_index];
                let expected_slice = &neighbors[layer.neighbors_start as usize
                    ..(layer.neighbors_start + layer.neighbor_count) as usize];
                prop_assert_eq!(decoded.as_slice(), expected_slice);
            }
        }
    }
}
