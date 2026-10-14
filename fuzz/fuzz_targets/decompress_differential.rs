#![no_main]

//! Arbitrary bytes as a bzip2 stream, decoded by one long-lived `Decoder` per
//! SIMD level (so state left by earlier inputs reaches later ones) against
//! libbz2 (the `bzip2` crate). Same rule as the hegel property
//! `*_agree_with_reference`: if the oracle decodes it within the cap, we
//! return the same bytes; if it errors or overflows the cap, we return an
//! error. The result must also equal that of a decoder made for this input.
//! A mismatch or a panic is a crash.

use bz2ngrs_fuzz::oracle::{all_decoders, check_against_reference, check_fresh_agrees};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cap = 4 * data.len() + (1 << 20);
    for decoder in all_decoders() {
        check_against_reference(decoder, data, cap);
        check_fresh_agrees(decoder, data, cap);
    }
});
