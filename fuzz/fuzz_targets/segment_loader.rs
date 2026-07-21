#![no_main]

use context_storage::{SegmentKind, decode_segment, encode_segment, validate_mmap_segment};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode_segment(data);
    let _ = validate_mmap_segment(data);

    if data.len() <= context_storage::MAX_SEGMENT_PAYLOAD_BYTES {
        let _ = encode_segment(SegmentKind::HnswGraph, data);
    }

    let valid_seed = encode_segment(SegmentKind::HnswGraph, b"fuzz-seed");
    if let Ok(encoded) = valid_seed {
        let _ = decode_segment(&encoded);
        let _ = validate_mmap_segment(&encoded);
    }
});
