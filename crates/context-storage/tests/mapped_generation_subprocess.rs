//! Sanitizer-friendly subprocess coverage for real immutable mappings.

#![allow(
    unsafe_code,
    reason = "the subprocess explicitly upholds the immutable-generation mmap contract"
)]

use std::{
    fs,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use context_storage::{
    MappedGraphIdentity, MappedPackedGraphImage, PackedGraphImageLayer, PackedGraphImageNode,
    SegmentKind, encode_mapped_packed_graph, encode_packed_graph_image, write_segment_atomic,
};

const CHILD_ENV: &str = "PGCONTEXT_MAPPED_GENERATION_CHILD";
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const IDENTITY: MappedGraphIdentity = MappedGraphIdentity {
    database_oid: 11,
    index_oid: 22,
    rel_file_number: 33,
    directory_epoch: 44,
    meta_lsn: 55,
};

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn create() -> std::io::Result<Self> {
        let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pgcontext-mapped-subprocess-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn packed_image(first: f32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
        &[first, 0.0, 0.0, 1.0],
    )?)
}

fn publish(path: &std::path::Path, first: f32) -> Result<(), Box<dyn std::error::Error>> {
    let payload = encode_mapped_packed_graph(IDENTITY, &packed_image(first)?);
    write_segment_atomic(path, SegmentKind::HnswGraph, &payload)?;
    Ok(())
}

#[test]
fn mapped_generation_real_file_subprocess() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(CHILD_ENV).is_some() {
        return Ok(());
    }
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("mapped_generation_replacement_child")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .status()?;
    assert!(status.success(), "mapped generation subprocess failed");
    Ok(())
}

#[test]
fn mapped_generation_replacement_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(CHILD_ENV).is_none() {
        return Ok(());
    }
    let directory = TestDirectory::create()?;
    let path = Arc::new(directory.0.join("generation.pgctxseg"));
    publish(&path, 1.0)?;

    // SAFETY: the pathname may be atomically replaced below, but the inode
    // opened by this owner is never modified or truncated while it is mapped.
    let mapped = unsafe { MappedPackedGraphImage::open(path.as_ref(), IDENTITY)? };
    let writer_path = Arc::clone(&path);
    let writer = thread::spawn(move || -> Result<(), String> {
        for value in 2_u8..=64 {
            publish(&writer_path, f32::from(value)).map_err(|error| error.to_string())?;
        }
        Ok(())
    });

    for _ in 0..1_000 {
        let node = mapped
            .view()
            .node(0)
            .ok_or_else(|| std::io::Error::other("mapped node should remain live"))?;
        assert_eq!(mapped.view().node_vector(node), Some(&[1.0, 0.0][..]));
    }
    writer
        .join()
        .map_err(|_| std::io::Error::other("replacement writer should not panic"))?
        .map_err(std::io::Error::other)?;
    drop(mapped);

    // SAFETY: publication is complete and this test exclusively owns the
    // installed generation until the returned owner is dropped.
    let replacement = unsafe { MappedPackedGraphImage::open(path.as_ref(), IDENTITY)? };
    let node = replacement
        .view()
        .node(0)
        .ok_or_else(|| std::io::Error::other("replacement node should exist"))?;
    assert_eq!(replacement.view().node_vector(node), Some(&[64.0, 0.0][..]));
    drop(replacement);
    fs::remove_file(path.as_ref())?;
    Ok(())
}
