//! Real-file ownership tests for mapped packed graph generations.

use std::{
    fs,
    path::PathBuf,
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use context_storage::{
    MappedGraphIdentity, MappedPackedGraphError, MappedPackedGraphImage, PackedGraphImageLayer,
    PackedGraphImageNode, SegmentKind, encode_mapped_packed_graph, encode_packed_graph_image,
    write_segment_atomic,
};

static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempFile(PathBuf);

impl TempFile {
    fn create() -> Self {
        let sequence = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "pgcontext-mapped-packed-{}-{sequence}.pgctxseg",
            process::id()
        )))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn packed_image() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(encode_packed_graph_image(
        2,
        &[
            PackedGraphImageNode {
                point_id: 10,
                vector_start: 0,
                layers_start: 0,
                layer_count: 1,
            },
            PackedGraphImageNode {
                point_id: 20,
                vector_start: 2,
                layers_start: 1,
                layer_count: 1,
            },
        ],
        &[
            PackedGraphImageLayer {
                neighbors_start: 0,
                neighbor_count: 1,
            },
            PackedGraphImageLayer {
                neighbors_start: 1,
                neighbor_count: 1,
            },
        ],
        &[1, 0],
        &[1.0, 0.0, 0.0, 1.0],
    )?)
}

const IDENTITY: MappedGraphIdentity = MappedGraphIdentity {
    database_oid: 1,
    index_oid: 2,
    rel_file_number: 3,
    directory_epoch: 4,
    meta_lsn: 5,
};

#[test]
fn mapped_packed_graph_owner_keeps_node_borrows_live() -> Result<(), Box<dyn std::error::Error>> {
    let file = TempFile::create();
    let payload = encode_mapped_packed_graph(IDENTITY, &packed_image()?);
    write_segment_atomic(&file.0, SegmentKind::HnswGraph, &payload)?;

    let mapped = MappedPackedGraphImage::open(&file.0, IDENTITY)?;
    let first = mapped
        .view()
        .node(0)
        .ok_or_else(|| std::io::Error::other("first packed node should exist"))?;
    assert_eq!(mapped.view().node_vector(first), Some(&[1.0, 0.0][..]));
    assert_eq!(
        mapped
            .view()
            .neighbors(first, 0)
            .ok_or_else(|| std::io::Error::other("base layer should exist"))?
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(mapped.mapped_len() > packed_image()?.len());
    assert_eq!(mapped.path(), file.0.as_path());
    Ok(())
}

#[test]
fn mapped_packed_graph_rejects_invalid_inner_image() -> Result<(), Box<dyn std::error::Error>> {
    let file = TempFile::create();
    let payload = encode_mapped_packed_graph(IDENTITY, b"invalid packed image");
    write_segment_atomic(&file.0, SegmentKind::HnswGraph, &payload)?;

    assert!(matches!(
        MappedPackedGraphImage::open(&file.0, IDENTITY),
        Err(MappedPackedGraphError::Graph(_))
    ));
    Ok(())
}

#[test]
fn mapped_packed_graph_rejects_a_different_index_identity() -> Result<(), Box<dyn std::error::Error>>
{
    let file = TempFile::create();
    let payload = encode_mapped_packed_graph(IDENTITY, &packed_image()?);
    write_segment_atomic(&file.0, SegmentKind::HnswGraph, &payload)?;
    let wrong = MappedGraphIdentity {
        index_oid: 99,
        ..IDENTITY
    };

    assert!(matches!(
        MappedPackedGraphImage::open(&file.0, wrong),
        Err(MappedPackedGraphError::IdentityMismatch)
    ));
    Ok(())
}
