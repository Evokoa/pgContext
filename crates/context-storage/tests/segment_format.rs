//! Segment format regression tests.

use std::{
    fs,
    path::PathBuf,
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use context_storage::{
    CURRENT_SEGMENT_FORMAT_VERSION, MAX_READABLE_SEGMENT_FORMAT_VERSION,
    MIN_READABLE_SEGMENT_FORMAT_VERSION, SegmentBytes, SegmentError, SegmentFileError,
    SegmentHeader, SegmentKind, SegmentVersion, SegmentWriteStage, decode_segment, encode_segment,
    export_segment_file, import_segment_file, is_supported_segment_format_version,
    load_segment_file, map_segment_file, reload_segment_file, validate_mmap_segment,
    write_segment_atomic, write_segment_atomic_with_hook,
};

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

type SegmentTestResult = Result<(), Box<dyn std::error::Error>>;

const V1_HNSW_GRAPH_PORTABLE_SEGMENT: &[u8] = &[
    0x50, 0x47, 0x43, 0x54, 0x58, 0x53, 0x45, 0x47, 0x01, 0x00, 0x00, 0x00, 0x04, 0x03, 0x02, 0x01,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xc1, 0xed, 0xa6, 0xbb, 0xe5, 0xdf, 0xb7, 0x22, 0x70, 0x6f, 0x72, 0x74, 0x61, 0x62, 0x6c, 0x65,
];

#[derive(Debug)]
struct TempSegmentDir {
    path: PathBuf,
}

impl TempSegmentDir {
    fn create() -> Result<Self, std::io::Error> {
        let sequence = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pgcontext-storage-test-{}-{sequence}",
            process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn join(&self, file_name: &str) -> PathBuf {
        self.path.join(file_name)
    }

    fn entries(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        fs::read_dir(&self.path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
}

impl Drop for TempSegmentDir {
    fn drop(&mut self) {
        if fs::remove_dir_all(&self.path).is_err() {}
    }
}

#[test]
fn segment_header_round_trips_payload() -> Result<(), SegmentError> {
    let payload = b"rebuildable graph bytes";
    let encoded = encode_segment(SegmentKind::HnswGraph, payload)?;

    let segment = decode_segment(&encoded)?;

    assert_eq!(segment.header().version(), SegmentVersion::V1);
    assert_eq!(segment.header().kind(), SegmentKind::HnswGraph);
    assert_eq!(segment.payload(), payload);
    Ok(())
}

#[test]
fn current_segment_format_version_is_v1() {
    assert_eq!(CURRENT_SEGMENT_FORMAT_VERSION, 1);
    assert_eq!(MIN_READABLE_SEGMENT_FORMAT_VERSION, 1);
    assert_eq!(
        MAX_READABLE_SEGMENT_FORMAT_VERSION,
        CURRENT_SEGMENT_FORMAT_VERSION
    );
    assert_eq!(SegmentVersion::V1.as_u32(), CURRENT_SEGMENT_FORMAT_VERSION);
    assert!(is_supported_segment_format_version(
        CURRENT_SEGMENT_FORMAT_VERSION
    ));
    assert!(!is_supported_segment_format_version(
        CURRENT_SEGMENT_FORMAT_VERSION + 1
    ));
}

#[test]
fn segment_loader_rejects_truncated_headers() {
    let truncated = vec![0; SegmentHeader::ENCODED_LEN - 1];

    assert_eq!(
        decode_segment(&truncated),
        Err(SegmentError::TruncatedHeader {
            actual: SegmentHeader::ENCODED_LEN - 1,
            minimum: SegmentHeader::ENCODED_LEN,
        })
    );
}

#[test]
fn segment_loader_rejects_unknown_versions() -> Result<(), SegmentError> {
    let mut encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;
    encoded[8..12].copy_from_slice(&2_u32.to_le_bytes());

    assert_eq!(
        decode_segment(&encoded),
        Err(SegmentError::UnknownVersion { version: 2 })
    );
    Ok(())
}

#[test]
fn segment_loader_rejects_wrong_endian_markers() -> Result<(), SegmentError> {
    let mut encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;
    encoded[12..16].copy_from_slice(&0x0403_0201_u32.to_le_bytes());

    assert_eq!(
        decode_segment(&encoded),
        Err(SegmentError::WrongEndianMarker {
            marker: 0x0403_0201
        })
    );
    Ok(())
}

#[test]
fn segment_loader_rejects_oversized_payload_lengths() -> Result<(), SegmentError> {
    let mut encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;
    encoded[24..32].copy_from_slice(&u64::MAX.to_le_bytes());

    assert_eq!(
        decode_segment(&encoded),
        Err(SegmentError::PayloadTooLarge {
            length: usize::MAX,
            maximum: context_storage::MAX_SEGMENT_PAYLOAD_BYTES,
        })
    );
    Ok(())
}

#[test]
fn segment_loader_rejects_corrupted_checksums() -> Result<(), SegmentError> {
    let mut encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;
    let last = encoded.len() - 1;
    encoded[last] ^= 0x55;

    assert_eq!(
        decode_segment(&encoded),
        Err(SegmentError::ChecksumMismatch)
    );
    Ok(())
}

#[test]
fn segment_loader_rejects_truncated_payloads() -> Result<(), SegmentError> {
    let mut encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;
    encoded.pop();

    assert_eq!(
        decode_segment(&encoded),
        Err(SegmentError::TruncatedPayload {
            expected: 7,
            actual: 6,
        })
    );
    Ok(())
}

#[test]
fn segment_bytes_accepts_payload_within_policy() -> Result<(), SegmentError> {
    let payload = vec![0; 16];
    let segment = SegmentBytes::new(SegmentKind::HnswGraph, payload.clone())?;

    assert_eq!(segment.payload(), payload.as_slice());
    Ok(())
}

#[test]
fn mmap_segment_validation_borrows_payload_bytes() -> Result<(), SegmentError> {
    let encoded = encode_segment(SegmentKind::HnswGraph, b"payload")?;

    let view = validate_mmap_segment(&encoded)?;

    assert_eq!(view.header().kind(), SegmentKind::HnswGraph);
    assert_eq!(view.payload(), b"payload");
    Ok(())
}

#[test]
fn mapped_segment_borrows_the_validated_file_payload() -> SegmentTestResult {
    let directory = TempSegmentDir::create()?;
    let path = directory.join("mapped.pgctxseg");
    write_segment_atomic(&path, SegmentKind::HnswGraph, b"mapped-payload")?;

    let mapped = map_segment_file(&path)?;

    assert_eq!(mapped.header().kind(), SegmentKind::HnswGraph);
    assert_eq!(mapped.payload(), b"mapped-payload");
    assert_eq!(mapped.mapped_len(), SegmentHeader::ENCODED_LEN + 14);
    assert_eq!(mapped.path(), path);
    Ok(())
}

#[test]
fn atomic_write_reloads_replaced_segments() -> Result<(), Box<dyn std::error::Error>> {
    let directory = TempSegmentDir::create()?;
    let path = directory.join("graph.pgctxseg");

    let first = write_segment_atomic(&path, SegmentKind::HnswGraph, b"first")?;
    assert_eq!(first.payload(), b"first");

    let second = write_segment_atomic(&path, SegmentKind::HnswGraph, b"second")?;
    assert_eq!(second.payload(), b"second");

    let reloaded = reload_segment_file(&path)?;
    assert_eq!(reloaded.payload(), b"second");
    assert_eq!(load_segment_file(&path)?.payload(), b"second");
    assert_eq!(directory.entries()?.len(), 1);
    Ok(())
}

#[test]
fn atomic_write_hooks_preserve_the_prior_generation_at_every_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    for stage in [
        SegmentWriteStage::BeforeWrite,
        SegmentWriteStage::BeforeFileSync,
        SegmentWriteStage::BeforeRename,
        SegmentWriteStage::BeforeDirectorySync,
    ] {
        let directory = TempSegmentDir::create()?;
        let path = directory.join("graph.pgctxseg");
        write_segment_atomic(&path, SegmentKind::HnswGraph, b"previous")?;
        let failed_path = path.clone();
        let result = write_segment_atomic_with_hook(
            &path,
            SegmentKind::HnswGraph,
            b"replacement",
            |observed| {
                if observed == stage {
                    Err(SegmentFileError::InvalidPath {
                        path: failed_path.clone(),
                    })
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(SegmentFileError::InvalidPath { .. })));
        let expected = if stage == SegmentWriteStage::BeforeDirectorySync {
            b"replacement".as_slice()
        } else {
            b"previous".as_slice()
        };
        assert_eq!(load_segment_file(&path)?.payload(), expected);
        assert_eq!(directory.entries()?.len(), 1);
    }
    Ok(())
}

#[test]
fn file_loader_rejects_malformed_reloaded_segments() -> Result<(), Box<dyn std::error::Error>> {
    let directory = TempSegmentDir::create()?;
    let path = directory.join("bad.pgctxseg");
    fs::write(&path, b"bad")?;

    assert!(matches!(
        load_segment_file(&path),
        Err(SegmentFileError::Format(
            SegmentError::TruncatedHeader { .. }
        ))
    ));
    Ok(())
}

#[test]
fn export_and_import_round_trip_validated_artifacts() -> Result<(), Box<dyn std::error::Error>> {
    let directory = TempSegmentDir::create()?;
    let source = directory.join("source.pgctxseg");
    let exported = directory.join("exported.pgctxseg");
    let imported = directory.join("imported.pgctxseg");

    write_segment_atomic(&source, SegmentKind::HnswGraph, b"portable")?;

    let export = export_segment_file(&source, &exported)?;
    assert_eq!(export.payload(), b"portable");

    let import = import_segment_file(&exported, &imported)?;
    assert_eq!(import.header().kind(), SegmentKind::HnswGraph);
    assert_eq!(import.payload(), b"portable");
    assert_eq!(load_segment_file(&imported)?.payload(), b"portable");
    Ok(())
}

#[test]
fn loader_and_import_accept_v1_compatibility_fixture() -> SegmentTestResult {
    let decoded = decode_segment(V1_HNSW_GRAPH_PORTABLE_SEGMENT)?;
    assert_eq!(decoded.header().version(), SegmentVersion::V1);
    assert_eq!(decoded.header().kind(), SegmentKind::HnswGraph);
    assert_eq!(decoded.payload(), b"portable");

    let mmap_view = validate_mmap_segment(V1_HNSW_GRAPH_PORTABLE_SEGMENT)?;
    assert_eq!(mmap_view.payload(), b"portable");

    let directory = TempSegmentDir::create()?;
    let source = directory.join("v1-fixture.pgctxseg");
    let destination = directory.join("imported-v1.pgctxseg");
    fs::write(&source, V1_HNSW_GRAPH_PORTABLE_SEGMENT)?;

    let imported = import_segment_file(&source, &destination)?;
    assert_eq!(imported.payload(), b"portable");
    assert_eq!(load_segment_file(&destination)?.payload(), b"portable");
    Ok(())
}

#[test]
fn import_rejects_future_version_fixture_and_preserves_destination() -> SegmentTestResult {
    let directory = TempSegmentDir::create()?;
    let source = directory.join("future-fixture.pgctxseg");
    let destination = directory.join("destination.pgctxseg");
    let mut future = V1_HNSW_GRAPH_PORTABLE_SEGMENT.to_vec();
    future[8..12].copy_from_slice(&(CURRENT_SEGMENT_FORMAT_VERSION + 1).to_le_bytes());
    fs::write(&source, future)?;
    write_segment_atomic(&destination, SegmentKind::HnswGraph, b"existing")?;

    assert!(matches!(
        import_segment_file(&source, &destination),
        Err(SegmentFileError::Format(SegmentError::UnknownVersion { version }))
            if version == CURRENT_SEGMENT_FORMAT_VERSION + 1
    ));
    assert_eq!(load_segment_file(&destination)?.payload(), b"existing");
    Ok(())
}

#[test]
fn import_rejects_corrupted_source_and_preserves_destination() -> SegmentTestResult {
    let directory = TempSegmentDir::create()?;
    let corrupted_source = directory.join("corrupted-source.pgctxseg");
    let destination = directory.join("destination.pgctxseg");

    let mut corrupted = encode_segment(SegmentKind::HnswGraph, b"corrupted")?;
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x55;
    fs::write(&corrupted_source, corrupted)?;
    write_segment_atomic(&destination, SegmentKind::HnswGraph, b"existing")?;

    assert!(matches!(
        import_segment_file(&corrupted_source, &destination),
        Err(SegmentFileError::Format(SegmentError::ChecksumMismatch))
    ));
    assert_eq!(load_segment_file(&destination)?.payload(), b"existing");
    Ok(())
}

#[test]
fn export_rejects_corrupted_source_without_creating_destination() -> SegmentTestResult {
    let directory = TempSegmentDir::create()?;
    let corrupted_source = directory.join("corrupted-source.pgctxseg");
    let destination = directory.join("destination.pgctxseg");

    let mut corrupted = encode_segment(SegmentKind::HnswGraph, b"corrupted")?;
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x55;
    fs::write(&corrupted_source, corrupted)?;

    assert!(matches!(
        export_segment_file(&corrupted_source, &destination),
        Err(SegmentFileError::Format(SegmentError::ChecksumMismatch))
    ));
    assert!(!destination.exists());
    Ok(())
}
