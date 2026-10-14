//! Generators: raw data, valid streams, mutated streams and hostile bytes.
//!
//! Draws are over the full documented domain (any bytes, any position, any
//! length). Resource limits are applied where the value is materialised, by
//! `%` or `take`, never by narrowing what is drawn. The one exception is the
//! size of a drawn byte vector, which hegel itself needs bounded.
#![allow(
    clippy::cast_possible_truncation,
    reason = "values are reduced modulo a bound first"
)]

use hegel::TestCase;
use hegel::generators::{self as gs, Generator};

use super::oracle::{compress_pieces, first_block_crc_bit, flip_bit, stream_crc_bit};

/// Largest drawn byte vector. hegel allows 2^20 choices per case.
const BYTES: usize = 20_000;

/// SplitMix64: cheap deterministic bytes for the large inputs, where drawing
/// every byte from hegel would cost more than the property.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `len` bytes from `seed` over an alphabet of `alphabet` symbols (1 to 256),
/// one byte in 1024 starting a run of 4 to 303 equal bytes.
pub fn mixed(seed: u64, len: usize, alphabet: usize) -> Vec<u8> {
    let alphabet = alphabet.clamp(1, 256) as u64;
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let x = rng.next();
        let b = ((x >> 8) % alphabet) as u8;
        if x & 1023 == 0 {
            let n = (4 + (x >> 40) % 300) as usize;
            out.resize(len.min(out.len() + n), b);
        } else {
            out.push(b);
        }
    }
    out
}

pub fn level() -> gs::IntegerGenerator<u32> {
    gs::integers::<u32>().min_value(1).max_value(9)
}

/// A length in `0..bound`, drawn from the whole of `u32`.
fn length_below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
}

#[hegel::composite]
pub fn any_bytes(tc: &TestCase) -> Vec<u8> {
    tc.draw(gs::binary().max_size(BYTES))
}

/// A run length: at or near the RLE1 boundaries, or anything.
#[hegel::composite]
fn run_length(tc: &TestCase) -> usize {
    if tc.draw(gs::booleans()) {
        let boundary = tc.draw(gs::sampled_from(vec![4usize, 5, 255, 256, 259]));
        let times = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
        let delta = tc.draw(gs::integers::<isize>().min_value(-3).max_value(3));
        (boundary * times).saturating_add_signed(delta)
    } else {
        length_below(tc, 3000)
    }
}

/// Runs of bytes (lengths around 4, 5, 255, 256, 259, and multiples), the
/// RLE1 stage's boundaries.
#[hegel::composite]
pub fn runs(tc: &TestCase) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(24)) {
        let byte = tc.draw(gs::integers::<u8>());
        let len = tc.draw(run_length());
        out.resize(out.len() + len, byte);
    }
    out
}

/// One repeating pattern of 1 to 12 bytes ("aaaa", "abab", all zeros, ...).
#[hegel::composite]
pub fn periodic(tc: &TestCase) -> Vec<u8> {
    let pattern = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(12));
    let len = length_below(tc, 70_000);
    pattern.iter().copied().cycle().take(len).collect()
}

/// Random bytes over 1 to 4 distinct symbols.
#[hegel::composite]
pub fn tiny_alphabet(tc: &TestCase) -> Vec<u8> {
    let alphabet = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(4));
    let picks = tc.draw(gs::binary().max_size(BYTES));
    picks
        .iter()
        .map(|&b| alphabet[usize::from(b) % alphabet.len()])
        .collect()
}

/// Pieces that `compress_pieces` puts into separate blocks: cheap multi-block
/// streams with ragged block sizes, empty pieces included.
#[hegel::composite]
pub fn pieces(tc: &TestCase) -> Vec<Vec<u8>> {
    tc.draw(
        gs::vecs(gs::binary().max_size(300))
            .min_size(2)
            .max_size(40),
    )
}

/// A seeded input around the first block boundary of `level`: filler up to
/// within 300 bytes of the 100000 * level - 19 bytes a block holds, then a run
/// that may cross it, then a tail. Big, so it costs one draw per field.
#[derive(Debug, Clone)]
pub struct BoundaryData {
    pub level: u32,
    pub data: Vec<u8>,
}

#[hegel::composite]
pub fn block_boundary(tc: &TestCase) -> BoundaryData {
    let level = tc.draw(level());
    let seed = tc.draw(gs::integers::<u64>());
    let alphabet = tc.draw(gs::integers::<usize>().min_value(1).max_value(256));
    let short_by = length_below(tc, 301);
    let run_len = length_below(tc, 700);
    let run_byte = tc.draw(gs::integers::<u8>());
    let tail = length_below(tc, 60);
    let filler = 100_000 * level as usize - 19 - short_by;
    let mut data = mixed(seed, filler, alphabet);
    data.resize(data.len() + run_len, run_byte);
    data.extend(mixed(seed ^ 1, tail, alphabet));
    BoundaryData { level, data }
}

/// One byte repeated 2^e (e up to `max_exponent`) plus or minus a few times:
/// the largest possible expansion per compressed byte, across block counts.
#[hegel::composite]
pub fn expanding(tc: &TestCase, max_exponent: u32) -> Vec<u8> {
    let exponent = tc.draw(gs::integers::<u32>().max_value(max_exponent));
    let delta = tc.draw(gs::integers::<isize>().min_value(-2).max_value(2));
    let len = (1usize << exponent).saturating_add_signed(delta);
    vec![tc.draw(gs::integers::<u8>()); len]
}

/// A valid stream and the data it decodes to.
#[derive(Debug, Clone)]
pub struct Valid {
    pub data: Vec<u8>,
    pub stream: Vec<u8>,
    pub level: u32,
}

#[hegel::composite]
pub fn valid_stream(tc: &TestCase) -> Valid {
    let level = tc.draw(level());
    let pieces: Vec<Vec<u8>> = match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => vec![tc.draw(any_bytes())],
        1 => vec![tc.draw(runs())],
        2 => vec![tc.draw(periodic())],
        3 => vec![tc.draw(tiny_alphabet())],
        4 => tc.draw(pieces()),
        _ => vec![tc.draw(expanding(20))],
    };
    let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    Valid {
        data: pieces.concat(),
        stream: compress_pieces(&refs, level),
        level,
    }
}

/// A byte position: near the start (headers, tables, selectors) or anywhere.
/// Used modulo the current length.
#[hegel::composite]
fn position(tc: &TestCase) -> usize {
    if tc.draw(gs::booleans()) {
        tc.draw(gs::integers::<usize>().max_value(80))
    } else {
        tc.draw(gs::integers::<usize>())
    }
}

#[derive(Debug, Clone)]
pub enum Mutation {
    FlipBit {
        at: usize,
        bit: u8,
    },
    SetByte {
        at: usize,
        value: u8,
    },
    Truncate {
        at: usize,
    },
    Insert {
        at: usize,
        bytes: Vec<u8>,
    },
    Delete {
        at: usize,
        count: usize,
    },
    /// The head of the stream up to `cut_a`, the tail of `other` from `cut_b`.
    Splice {
        cut_a: usize,
        other: Vec<u8>,
        cut_b: usize,
    },
    /// Byte 3 (the block size digit) becomes `value`.
    BlockSizeDigit {
        value: u8,
    },
    /// A bit of the first block's stored CRC.
    BlockCrc {
        bit: usize,
    },
    /// A bit of the stream's combined CRC.
    StreamCrc {
        bit: usize,
    },
}

#[hegel::composite]
pub fn mutation(tc: &TestCase) -> Mutation {
    match tc.draw(gs::integers::<u8>().max_value(8)) {
        0 => Mutation::FlipBit {
            at: tc.draw(position()),
            bit: tc.draw(gs::integers::<u8>()),
        },
        1 => Mutation::SetByte {
            at: tc.draw(position()),
            value: tc.draw(gs::integers::<u8>()),
        },
        2 => Mutation::Truncate {
            at: tc.draw(position()),
        },
        3 => Mutation::Insert {
            at: tc.draw(position()),
            bytes: tc.draw(gs::binary().max_size(64)),
        },
        4 => Mutation::Delete {
            at: tc.draw(position()),
            count: tc.draw(position()),
        },
        5 => Mutation::Splice {
            cut_a: tc.draw(position()),
            other: tc.draw(valid_stream()).stream,
            cut_b: tc.draw(position()),
        },
        6 => Mutation::BlockSizeDigit {
            value: tc.draw(gs::one_of(vec![
                gs::integers::<u8>().min_value(b'0').max_value(b'9').boxed(),
                gs::integers::<u8>().boxed(),
            ])),
        },
        7 => Mutation::BlockCrc {
            bit: tc.draw(gs::integers::<usize>()),
        },
        _ => Mutation::StreamCrc {
            bit: tc.draw(gs::integers::<usize>()),
        },
    }
}

pub fn apply(stream: &mut Vec<u8>, m: &Mutation) {
    let len = stream.len();
    match m {
        Mutation::FlipBit { at, bit } if len > 0 => stream[at % len] ^= 1 << (bit % 8),
        Mutation::SetByte { at, value } if len > 0 => stream[at % len] = *value,
        Mutation::Truncate { at } => stream.truncate(at % (len + 1)),
        Mutation::Insert { at, bytes } => {
            let at = at % (len + 1);
            stream.splice(at..at, bytes.iter().copied());
        }
        Mutation::Delete { at, count } if len > 0 => {
            let at = at % len;
            stream.drain(at..len.min(at.saturating_add(*count)));
        }
        Mutation::Splice {
            cut_a,
            other,
            cut_b,
        } => {
            let head = cut_a % (len + 1);
            let tail = cut_b % (other.len() + 1);
            stream.truncate(head);
            stream.extend_from_slice(&other[tail..]);
        }
        Mutation::BlockSizeDigit { value } if len > 3 => stream[3] = *value,
        Mutation::BlockCrc { bit } => {
            if let Some(start) = first_block_crc_bit(stream) {
                flip_bit(stream, start + bit % 32);
            }
        }
        Mutation::StreamCrc { bit } => {
            if let Some(start) = stream_crc_bit(stream) {
                flip_bit(stream, start + bit % 32);
            }
        }
        _ => {}
    }
}

/// A valid stream with one to three mutations applied.
#[derive(Debug, Clone)]
pub struct Mutated {
    pub original: Valid,
    pub mutations: Vec<Mutation>,
    pub stream: Vec<u8>,
}

#[hegel::composite]
pub fn mutated_stream(tc: &TestCase) -> Mutated {
    let original = tc.draw(valid_stream());
    let mutations = tc.draw(gs::vecs(mutation()).min_size(1).max_size(3));
    let mut stream = original.stream.clone();
    for m in &mutations {
        apply(&mut stream, m);
    }
    Mutated {
        original,
        mutations,
        stream,
    }
}

/// The output capacity for the differential rule: 4 times the original plus
/// 1 MB, or sometimes an arbitrary small one so that overflow is reached
/// from the inside of a block.
#[hegel::composite]
pub fn output_cap(tc: &TestCase, original_len: usize) -> usize {
    match tc.draw(gs::optional(gs::integers::<usize>())) {
        None => 4 * original_len + (1 << 20),
        Some(n) => n % (2 * original_len + 64),
    }
}

/// Fully arbitrary bytes.
#[hegel::composite]
pub fn hostile_bytes(tc: &TestCase) -> Vec<u8> {
    tc.draw(gs::binary().max_size(4096))
}

/// "BZh1" to "BZh9", then arbitrary bytes.
#[hegel::composite]
pub fn hostile_bzh(tc: &TestCase) -> Vec<u8> {
    let digit = tc.draw(gs::integers::<u8>().min_value(b'1').max_value(b'9'));
    let mut out = vec![b'B', b'Z', b'h', digit];
    out.extend(tc.draw(gs::binary().max_size(4096)));
    out
}

/// "BZh1" to "BZh9", the block magic, then arbitrary bytes: reaches the block
/// header parser (CRC, randomised bit, origPtr, symbol map, selectors, tables)
/// without a valid stream to start from.
#[hegel::composite]
pub fn hostile_block(tc: &TestCase) -> Vec<u8> {
    let digit = tc.draw(gs::integers::<u8>().min_value(b'1').max_value(b'9'));
    let mut out = vec![b'B', b'Z', b'h', digit];
    out.extend([0x31, 0x41, 0x59, 0x26, 0x53, 0x59]);
    out.extend(tc.draw(gs::binary().max_size(4096)));
    out
}
