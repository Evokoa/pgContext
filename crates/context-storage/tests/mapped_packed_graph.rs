//! Real-file ownership tests for mapped packed graph generations.

use std::{
    fs,
    path::PathBuf,
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use context_storage::{
    MappedPackedGraphError, MappedPackedGraphImage, PackedGraphImageLayer, PackedGraphImageNode,
    SegmentKind, encode_packed_graph_image, write_segment_atomic,
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

#[test]
fn mapped_packed_graph_owner_keeps_node_borrows_live() -> Result<(), Box<dyn std::error::Error>> {
    let file = TempFile::create();
    write_segment_atomic(&file.0, SegmentKind::HnswGraph, &packed_image()?)?;

    let mapped = MappedPackedGraphImage::open(&file.0)?;
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
    write_segment_atomic(&file.0, SegmentKind::HnswGraph, b"invalid packed image")?;

    assert!(matches!(
        MappedPackedGraphImage::open(&file.0),
        Err(MappedPackedGraphError::Graph(_))
    ));
    Ok(())
}
