#![no_main]

//! Arbitrary data at an arbitrary level: a fresh `Encoder::new()` writes
//! libbz2's stream up to the origPtr of exactly periodic blocks, our decoder
//! reads it back, and a long-lived encoder at every SIMD level writes the
//! same (`enc_oracle::check_all`). `repeat` tiles the data so that small inputs reach several blocks
//! and long runs.

use arbitrary::Arbitrary;
use bz2ngrs_fuzz::enc_oracle::{all_encoders, check_all};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    level: u8,
    repeat: u16,
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let level = u32::from(input.level % 9) + 1;
    let times = 1 + usize::from(input.repeat) % 512;
    // 2 MB at most: a few level-1 blocks.
    let data: Vec<u8> = input
        .data
        .iter()
        .copied()
        .cycle()
        .take((input.data.len() * times).min(2 << 20))
        .collect();
    check_all(&all_encoders(), &data, level);
});
