//! The encoder's property bodies, one function per property.
#![allow(
    clippy::cast_possible_truncation,
    reason = "test-sized lengths and ranks"
)]

use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};

use super::enc_oracle::{Encoder, check_matches_libbz2, compression};
use super::inputs::{self, Rng, mixed};
use super::oracle::{Decoder, check_exact};

/// A count in `0..bound` from the whole of `u32`.
fn below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
}

/// Every encoder writes the same stream for `data` at `level`; it is
/// libbz2's (up to the origPtr of exactly periodic blocks), and our decoder
/// reads it back.
pub fn check_all(encoders: &[Encoder], data: &[u8], level: u32) {
    let Some((first, rest)) = encoders.split_first() else {
        return;
    };
    let stream = first.run(data, level);
    for e in rest {
        assert!(
            e.run(data, level) == stream,
            "{} and {} write different streams at level {level}",
            e.name(),
            first.name()
        );
    }
    check_matches_libbz2(&first.name(), &stream, data, level);
    check_exact(Decoder::New, &stream, data);
}

/// Property E1: data of a given shape, at any level.
pub fn matches_libbz2(tc: &TestCase, encoders: &[Encoder], data: impl PrintableGenerator<Vec<u8>>) {
    let data = tc.draw(data);
    let level = tc.draw(inputs::level());
    check_all(encoders, &data, level);
}

/// Property E1, around the first block boundary: libbz2's block-full rule,
/// and the pending run that joins a full last block.
pub fn matches_libbz2_boundary(tc: &TestCase, encoders: &[Encoder]) {
    let case = tc.draw(inputs::block_boundary());
    check_all(encoders, &case.data, case.level);
}

/// A level and data that fill its first block at or near the input's last
/// byte: `nblock_max + delta` bytes with no two equal neighbours (every byte
/// a unit of its own, written when the next one arrives), then a run of 1 to
/// 300 bytes. With `delta = 0` and a run of one byte, the byte that fills the
/// block is the last one, and libbz2 adds it to that full block.
#[derive(Debug, Clone)]
pub struct FullAtEnd {
    pub level: u32,
    pub data: Vec<u8>,
}

#[hegel::composite]
pub fn full_at_end(tc: &TestCase) -> FullAtEnd {
    let level = tc.draw(gs::integers::<u32>().min_value(1).max_value(9));
    let seed = tc.draw(gs::integers::<u64>());
    let alphabet = tc.draw(gs::integers::<usize>().min_value(2).max_value(256));
    let delta = tc.draw(gs::integers::<isize>().min_value(-8).max_value(8));
    let run = 1 + if tc.draw(gs::booleans()) {
        below(tc, 4)
    } else {
        below(tc, 300)
    };
    let literals = (100_000 * level as usize - 19).saturating_add_signed(delta);
    let mut data = mixed(seed, literals, alphabet);
    for i in 1..data.len() {
        if data[i] == data[i - 1] {
            data[i] = data[i].wrapping_add(1);
        }
    }
    let byte = data.last().map_or(0, |b| b.wrapping_add(1));
    data.resize(data.len() + run, byte);
    FullAtEnd { level, data }
}

/// Property E1 where the first block fills at or near the end of the input.
pub fn matches_libbz2_full_at_end(tc: &TestCase, encoders: &[Encoder]) {
    let case = tc.draw(full_at_end());
    check_all(encoders, &case.data, case.level);
}

/// Property E1, many blocks: up to 2.5 MB of seeded data at levels 1 to 3.
pub fn matches_libbz2_many_blocks(tc: &TestCase, encoders: &[Encoder]) {
    let level = tc.draw(gs::integers::<u32>().min_value(1).max_value(3));
    let seed = tc.draw(gs::integers::<u64>());
    let alphabet = tc.draw(gs::integers::<usize>().min_value(1).max_value(256));
    let len = below(tc, 2_500_000);
    check_all(encoders, &mixed(seed, len, alphabet), level);
}

/// A pattern of 1 to 12 bytes repeated over up to 300 kB, so that level 1
/// cuts it into several exactly periodic blocks, with up to 3 bytes changed
/// (which makes a block primitive again).
#[hegel::composite]
pub fn long_periodic(tc: &TestCase) -> Vec<u8> {
    let pattern = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(12));
    let len = below(tc, 300_000);
    let mut data: Vec<u8> = pattern.iter().copied().cycle().take(len).collect();
    for _ in 0..below(tc, 4) {
        if !data.is_empty() {
            let at = below(tc, data.len());
            data[at] = tc.draw(gs::integers::<u8>());
        }
    }
    data
}

/// Property E1, periodic blocks across block boundaries, at level 1.
pub fn matches_libbz2_long_periodic(tc: &TestCase, encoders: &[Encoder]) {
    let data = tc.draw(long_periodic());
    let level = tc.draw(gs::integers::<u32>().min_value(1).max_value(2));
    check_all(encoders, &data, level);
}

/// A text whose block needs libbz2's 17-bit code-length limit (bz2-ng's
/// `huff_limit_test.c`). The Huffman stage codes the MTF ranks of the BWT;
/// here rank r (1 to `ranks`) occurs 2^(ranks - r) times, so each count
/// exceeds the sum of all smaller ones, the tree is a chain, and the rarest
/// rank's code is about `ranks` bits long. The ranks are shuffled so every
/// 50-symbol group sees the same mixture, turned into a BWT by inverse MTF,
/// and the BWT into a text by following its LF cycles.
pub fn chain_text(seed: u64, ranks: u32) -> Vec<u8> {
    let n = (1usize << ranks) - 1;
    let mut rank: Vec<u8> = (1..=n).map(|v| 1 + v.trailing_zeros() as u8).collect();
    let mut rng = Rng(seed);
    for i in (1..n).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        rank.swap(i, j);
    }
    let mut list: Vec<u8> = (0..=ranks as u8).collect();
    let bwt: Vec<u8> = rank
        .iter()
        .map(|&r| {
            let c = list.remove(usize::from(r));
            list.insert(0, c);
            c
        })
        .collect();
    let mut next = [0usize; 256];
    for &c in &bwt {
        next[usize::from(c)] += 1;
    }
    let mut sum = 0;
    for x in next.iter_mut() {
        let count = *x;
        *x = sum;
        sum += count;
    }
    let mut lf: Vec<usize> = bwt
        .iter()
        .map(|&c| {
            let row = next[usize::from(c)];
            next[usize::from(c)] += 1;
            row
        })
        .collect();
    // Each cycle of the LF mapping is the BWT of one word; the text is the
    // words one after another, each read backwards from its cycle.
    let mut text = vec![0u8; n];
    let mut pos = n;
    let mut seen = vec![false; n];
    for start in 0..n {
        let mut row = start;
        while !seen[row] {
            seen[row] = true;
            pos -= 1;
            text[pos] = bwt[row];
            row = std::mem::take(&mut lf[row]);
        }
    }
    text
}

/// Property E1 where libbz2 limits code lengths.
pub fn matches_libbz2_code_length_limit(tc: &TestCase, encoders: &[Encoder]) {
    let seed = tc.draw(gs::integers::<u64>());
    let ranks = tc.draw(gs::integers::<u32>().min_value(18).max_value(19));
    check_all(encoders, &chain_text(seed, ranks), 9);
}

/// Property E2: an encoder that has compressed other data gives the same
/// stream as a new one.
pub fn reuse_independent(tc: &TestCase, encoders: &[Encoder]) {
    let level = tc.draw(inputs::level());
    let earlier = tc.draw(gs::vecs(inputs::any_bytes()).max_size(3));
    let data = tc.draw(inputs::runs());
    for e in encoders {
        for d in &earlier {
            e.run(d, level);
        }
        assert!(
            e.run(&data, level) == e.run_fresh(&data, level),
            "{}: depends on earlier calls",
            e.name()
        );
    }
}

/// Property E3: `compress` appends to `out` and leaves what was there.
pub fn compress_appends(tc: &TestCase, encoders: &[Encoder]) {
    let level = tc.draw(inputs::level());
    let prefix = tc.draw(gs::binary().max_size(100));
    let data = tc.draw(inputs::any_bytes());
    for e in encoders {
        let stream = e.run(&data, level);
        let mut enc = match e {
            Encoder::New => bz2ngrs::Encoder::new(compression(level)),
            Encoder::At(simd) => bz2ngrs::Encoder::__with_level(compression(level), *simd),
        };
        let mut out = prefix.clone();
        enc.compress(&data, &mut out);
        assert!(
            out[..prefix.len()] == prefix,
            "{}: prefix changed",
            e.name()
        );
        assert!(
            out[prefix.len()..] == stream,
            "{}: appended stream differs",
            e.name()
        );
    }
}
