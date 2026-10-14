#![no_main]

//! Arbitrary data compressed by libbz2 at an arbitrary level must decode to
//! the same bytes at every SIMD level, into an `out` of exactly that size.
//! `repeat` tiles the data so that small inputs reach multi-block streams and
//! long runs.

use arbitrary::Arbitrary;
use bz2ngrs_fuzz::oracle::{all_decoders, check_exact, compress};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    level: u8,
    repeat: u16,
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let level = u32::from(input.level % 9) + 1;
    let times = 1 + usize::from(input.repeat) % 2048;
    // 8 MB at most: a handful of level-1 blocks.
    let data: Vec<u8> = input.data.iter().copied().cycle().take((input.data.len() * times).min(8 << 20)).collect();
    let stream = compress(&data, level);
    for decoder in all_decoders() {
        check_exact(decoder, &stream, &data);
    }
});
