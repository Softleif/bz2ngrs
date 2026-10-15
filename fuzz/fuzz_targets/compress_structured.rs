#![no_main]

//! Data built from segments that reach the encoder's edges quickly: runs
//! around the RLE1 lengths (4, 255), repeated patterns that make exactly
//! periodic blocks, and literals, at levels 1 and 2 so that a few hundred
//! kilobytes cross block boundaries. The checks are those of
//! `compress_differential`.

use arbitrary::Arbitrary;
use bz2ngrs_fuzz::enc_oracle::{all_encoders, check_matches_libbz2};
use bz2ngrs_fuzz::oracle::{Decoder, check_exact};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum Segment {
    /// `len` copies of `byte`; `len` near 4 or 255 most of the time.
    Run {
        byte: u8,
        len: u16,
    },
    /// `pattern` repeated `times` times.
    Pattern {
        pattern: Vec<u8>,
        times: u16,
    },
    Literal(Vec<u8>),
}

#[derive(Debug, Arbitrary)]
struct Input {
    level: bool,
    segments: Vec<Segment>,
}

fuzz_target!(|input: Input| {
    let level = 1 + u32::from(input.level);
    let mut data = Vec::new();
    for s in &input.segments {
        match s {
            Segment::Run { byte, len } => {
                let len = match len % 8 {
                    0..=2 => 1 + usize::from(len >> 3) % 8,
                    3..=5 => 250 + usize::from(len >> 3) % 12,
                    _ => usize::from(*len) * 4,
                };
                data.resize(data.len() + len, *byte);
            }
            Segment::Pattern { pattern, times } => {
                for _ in 0..usize::from(*times) * 8 {
                    data.extend_from_slice(pattern);
                }
            }
            Segment::Literal(bytes) => data.extend_from_slice(bytes),
        }
        if data.len() > 600_000 {
            data.truncate(600_000);
            break;
        }
    }
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
