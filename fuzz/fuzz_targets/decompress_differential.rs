#![no_main]

//! Arbitrary bytes as a bzip2 stream: at every SIMD level, `__decompress_at`
//! and `decompress` against libbz2 (the `bzip2` crate). Same rule as the hegel
//! property `*_agree_with_reference`: if the oracle decodes it within the cap,
//! we return the same bytes; if it errors or overflows the cap, we return an
//! error. A mismatch or a panic is a crash.

use bz2ngrs_fuzz::oracle::{all_decoders, check_against_reference};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cap = 4 * data.len() + (1 << 20);
    for decoder in all_decoders() {
        check_against_reference(decoder, data, cap);
    }
});
