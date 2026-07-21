#![no_main]

use core::str::FromStr;

use context_core::{BitVector, DenseVector, HalfVector, SparseVector};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = core::str::from_utf8(data) {
        round_trip::<DenseVector>(input);
        round_trip::<HalfVector>(input);
        round_trip::<SparseVector>(input);
        round_trip::<BitVector>(input);
    }
});

fn round_trip<T>(input: &str)
where
    T: FromStr + ToString + PartialEq + core::fmt::Debug,
    T::Err: core::fmt::Debug,
{
    if let Ok(parsed) = input.parse::<T>() {
        let formatted = parsed.to_string();
        let Ok(reparsed) = formatted.parse::<T>() else {
            panic!("formatted vector text should parse: {formatted}");
        };
        assert_eq!(reparsed, parsed);
    }
}
