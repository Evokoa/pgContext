#![no_main]

use context_index::{GraphPageEnvelope, GraphPageKind};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = GraphPageEnvelope::decode(data);

    let generation = nonzero_u64(data.get(..8).unwrap_or(data));
    let page_id = nonzero_u64(data.get(8..16).unwrap_or_default());
    let kind = match data.get(16).copied().unwrap_or_default() % 4 {
        0 => GraphPageKind::Directory,
        1 => GraphPageKind::Node,
        2 => GraphPageKind::Adjacency,
        _ => GraphPageKind::MutationDescriptor,
    };
    if let Ok(envelope) = GraphPageEnvelope::new(kind, generation, page_id) {
        let Ok(bytes) = envelope.encode() else {
            return;
        };
        assert_eq!(GraphPageEnvelope::decode(&bytes), Ok(envelope));
    }
});

fn nonzero_u64(bytes: &[u8]) -> u64 {
    let mut encoded = [0_u8; 8];
    let count = bytes.len().min(encoded.len());
    encoded[..count].copy_from_slice(&bytes[..count]);
    u64::from_le_bytes(encoded).max(1)
}
