#![no_main]

//! Arbitrary data at an arbitrary level: every encoder (`Encoder::new()` and
//! every SIMD level, each long-lived) writes the same stream, it is libbz2's
//! up to the origPtr of exactly periodic blocks, and our decoder reads it
//! back. `repeat` tiles the data so that small inputs reach several blocks
//! and long runs.

use arbitrary::Arbitrary;
use bz2ngrs_fuzz::enc_oracle::{all_encoders, check_matches_libbz2};
use bz2ngrs_fuzz::oracle::{Decoder, check_exact};
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
    let encoders = all_encoders();
    let stream = encoders[0].run(&data, level);
    for e in &encoders[1..] {
        assert!(
            e.run(&data, level) == stream,
            "{} differs from {}",
            e.name(),
            encoders[0].name()
        );
    }
    check_matches_libbz2(&encoders[0].name(), &stream, &data, level);
    check_exact(Decoder::New, &stream, &data);
});
