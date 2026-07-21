#![no_main]

use context_filter::parse_filter_json;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = core::str::from_utf8(data) {
        let _ = parse_filter_json(input);
    }
});
