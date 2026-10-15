//! The encoder's property bodies, one function per property.
#![allow(
    clippy::cast_possible_truncation,
    reason = "test-sized lengths and ranks"
)]

use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};

pub use super::enc_oracle::check_all;
use super::enc_oracle::{Encoder, check_matches_libbz2, compression, periodic_blocks};
use super::inputs::{self, Rng, mixed};

/// A count in `0..bound` from the whole of `u32`.
fn below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
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

/// Property E1, many blocks: seeded data at any level, up to 2.5 MB (half
/// the draws) or up to three blocks' worth.
pub fn matches_libbz2_many_blocks(tc: &TestCase, encoders: &[Encoder]) {
    let level = tc.draw(inputs::level());
    let seed = tc.draw(gs::integers::<u64>());
    let alphabet = tc.draw(gs::integers::<usize>().min_value(1).max_value(256));
    let bound = if tc.draw(gs::booleans()) {
        2_500_000
    } else {
        300_000 * level as usize
    };
    let len = below(tc, bound);
    check_all(encoders, &mixed(seed, len, alphabet), level);
}

/// A pattern of 1 to 40 bytes repeated over up to 300 kB, so that level 1
/// cuts it into several blocks, with up to 3 bytes changed (which makes a
/// block primitive again). Exactly periodic blocks come from
/// [`exact_periodic`].
#[hegel::composite]
pub fn long_periodic(tc: &TestCase) -> Vec<u8> {
    let pattern = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(40));
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

/// Property E1, periodic blocks across block boundaries, at any level.
pub fn matches_libbz2_long_periodic(tc: &TestCase, encoders: &[Encoder]) {
    let data = tc.draw(long_periodic());
    let level = tc.draw(inputs::level());
    check_all(encoders, &data, level);
}

/// A level and the data to compress at it.
#[derive(Debug, Clone)]
pub struct LevelData {
    pub level: u32,
    pub data: Vec<u8>,
}

/// `raw` with every byte different from its neighbours, the last from the
/// first too (for 2 bytes or more), so that RLE1 leaves a repeated pattern
/// as it is.
pub fn run_free(raw: &[u8]) -> Vec<u8> {
    let mut p = raw.to_vec();
    for i in 1..p.len() {
        if p[i] == p[i - 1] {
            p[i] = p[i].wrapping_add(1);
        }
    }
    let first = p.first().copied();
    if let [.., before, last] = p.as_mut_slice() {
        while Some(*last) == first || last == before {
            *last = last.wrapping_add(1);
        }
    }
    p
}

/// Data at `level` whose RLE1 blocks are exactly periodic. Half the draws
/// repeat a run-free pattern whose length divides the block size, so that
/// every full block is the pattern over and over, the last cut anywhere; the
/// others repeat a few runs and literals (the runs become RLE1 units) two
/// times or more within one block.
fn exact_periodic_at(tc: &TestCase, level: u32) -> Vec<u8> {
    let nblock_max = 100_000 * level as usize - 19;
    let divisors: Vec<usize> = (2..=20_000)
        .filter(|&d| nblock_max.is_multiple_of(d))
        .collect();
    let mut rng = Rng(tc.draw(gs::integers::<u64>()));
    let alphabet = tc.draw(gs::integers::<u64>().min_value(2).max_value(256));
    if !divisors.is_empty() && tc.draw(gs::booleans()) {
        let p = tc.draw(gs::sampled_from(divisors));
        let raw: Vec<u8> = (0..p).map(|_| (rng.next() % alphabet) as u8).collect();
        let pattern = run_free(&raw);
        let blocks = if level <= 3 { 1 + below(tc, 3) } else { 1 };
        // A single byte past the full blocks joins the last of them (libbz2's
        // rule at the end of the input), which makes it primitive.
        let extra = match below(tc, 3 * p + 1) {
            1 => 0,
            x => x,
        };
        let len = blocks * nblock_max + extra;
        return pattern.into_iter().cycle().take(len).collect();
    }
    let units = 2 + below(tc, 7);
    let raw: Vec<u8> = (0..units).map(|_| (rng.next() % alphabet) as u8).collect();
    let bytes = run_free(&raw);
    let mut unit = Vec::new();
    for &b in &bytes {
        let n = if tc.draw(gs::booleans()) {
            tc.draw(gs::sampled_from(vec![
                1usize, 2, 3, 4, 5, 6, 254, 255, 256, 259, 300,
            ]))
        } else {
            1 + below(tc, 40)
        };
        unit.extend(std::iter::repeat_n(b, n));
    }
    // RLE1 grows the input by at most 5/4, so this many bytes fit one block.
    let most = nblock_max * 4 / 5 / unit.len();
    unit.repeat(2 + below(tc, most - 1))
}

#[hegel::composite]
pub fn exact_periodic(tc: &TestCase) -> LevelData {
    let level = tc.draw(inputs::level());
    LevelData {
        level,
        data: exact_periodic_at(tc, level),
    }
}

/// Property E1 where blocks are exactly periodic: the origPtr is the last
/// tied row (checked against the block itself, whatever libbz2 writes), and
/// everything else is libbz2's.
pub fn matches_libbz2_exact_periodic(tc: &TestCase, encoders: &[Encoder]) {
    let case = tc.draw(exact_periodic());
    assert!(
        periodic_blocks(&case.data, case.level) > 0,
        "the generator made no exactly periodic block"
    );
    check_all(encoders, &case.data, case.level);
}

/// The number of MTF values (zero-run digits, ranks, end of block) of the
/// BWT of `block`, from the rotations sorted by direct comparison and an MTF
/// list.
fn naive_mtf_values(block: &[u8]) -> usize {
    let n = block.len();
    let mut rows: Vec<usize> = (0..n).collect();
    rows.sort_by(|&a, &b| {
        (0..n)
            .map(|k| block[(a + k) % n])
            .cmp((0..n).map(|k| block[(b + k) % n]))
    });
    let mut list: Vec<u8> = (0..=255u8).filter(|b| block.contains(b)).collect();
    let digits = |z: usize| (usize::BITS - 1 - (z + 1).leading_zeros()) as usize;
    let (mut count, mut zeros) = (1, 0);
    for r in rows {
        let c = block[(r + n - 1) % n];
        let rank = list.iter().position(|&x| x == c).unwrap_or(0);
        list.remove(rank);
        list.insert(0, c);
        if rank == 0 {
            zeros += 1;
        } else {
            count += digits(zeros) + 1;
            zeros = 0;
        }
    }
    count + digits(zeros)
}

/// Bytes with no four equal in a row (RLE1 leaves them alone) whose block has
/// exactly `want` MTF values, end of block included, if a search over their
/// length finds them (for most seeds). libbz2 picks the number of Huffman
/// tables from that count: 2 below 200, 3 below 600, 4 below 1200, 5 below
/// 2400, 6 from there.
fn text_with_mtf_values(rng: &mut Rng, alphabet: u64, want: usize) -> Option<Vec<u8>> {
    let mut t: Vec<u8> = Vec::new();
    let mut len = want;
    for _ in 0..40 {
        t.truncate(len);
        while t.len() < len {
            let mut b = (rng.next() % alphabet) as u8;
            if let [.., x, y, z] = t.as_slice()
                && x == y
                && y == z
                && *z == b
            {
                b = b.wrapping_add(1);
            }
            t.push(b);
        }
        let got = naive_mtf_values(&t);
        if got == want {
            return Some(t);
        }
        len = len
            .saturating_add_signed(want.cast_signed() - got.cast_signed())
            .clamp(1, 20_000);
    }
    None
}

/// A block with 200, 600, 1200 or 2400 MTF values, give or take 3.
#[hegel::composite]
pub fn near_table_count_threshold(tc: &TestCase) -> Vec<u8> {
    let threshold = tc.draw(gs::sampled_from(vec![200usize, 600, 1200, 2400]));
    let delta = tc.draw(gs::integers::<isize>().min_value(-3).max_value(3));
    let alphabet = tc.draw(gs::integers::<u64>().min_value(2).max_value(256));
    let mut rng = Rng(tc.draw(gs::integers::<u64>()));
    match text_with_mtf_values(&mut rng, alphabet, threshold.saturating_add_signed(delta)) {
        Some(t) => t,
        None => tc.reject(),
    }
}

/// Property E1 where the number of MTF values is at a table-count threshold.
pub fn matches_libbz2_table_count_threshold(tc: &TestCase, encoders: &[Encoder]) {
    matches_libbz2(tc, encoders, near_table_count_threshold());
}

/// One byte value repeated: a lone symbol in the BWT, run lengths at RLE1's
/// boundaries (multiples of 255, a few over) or anywhere.
#[hegel::composite]
pub fn single_byte(tc: &TestCase) -> Vec<u8> {
    let byte = tc.draw(gs::integers::<u8>());
    let len = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => 255 * below(tc, 2000) + below(tc, 8),
        1 => below(tc, 300),
        _ => below(tc, 300_000),
    };
    vec![byte; len]
}

/// Property E1 for data of one distinct byte.
pub fn matches_libbz2_single_byte(tc: &TestCase, encoders: &[Encoder]) {
    matches_libbz2(tc, encoders, single_byte());
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
    let level = tc.draw(inputs::level());
    check_all(encoders, &chain_text(seed, ranks), level);
}

/// An input that grows an encoder's buffers or leaves them in an odd state,
/// for `level`: several blocks, exactly periodic blocks, long runs, noise.
#[hegel::composite]
pub fn big_input(tc: &TestCase, level: u32) -> Vec<u8> {
    match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => {
            let seed = tc.draw(gs::integers::<u64>());
            let alphabet = tc.draw(gs::integers::<usize>().min_value(1).max_value(256));
            let len = below(tc, (200_000 * level as usize).min(1_200_000));
            mixed(seed, len, alphabet)
        }
        1 => exact_periodic_at(tc, level),
        2 => tc.draw(long_periodic()),
        3 => tc.draw(inputs::runs()),
        4 => tc.draw(inputs::expanding(21)),
        _ => tc.draw(inputs::any_bytes()),
    }
}

/// Empty, one byte, a short run, a few bytes.
#[hegel::composite]
pub fn tiny_input(tc: &TestCase) -> Vec<u8> {
    let byte = tc.draw(gs::integers::<u8>());
    match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => Vec::new(),
        1 => vec![byte],
        2 => vec![byte; 2 + below(tc, 5)],
        _ => tc.draw(gs::binary().max_size(8)),
    }
}

/// Property E2: an encoder that has compressed big inputs gives, for the
/// tiny and ordinary inputs after them, the stream a new one gives, and
/// libbz2's.
pub fn reuse_independent(tc: &TestCase, encoders: &[Encoder]) {
    let level = tc.draw(inputs::level());
    let earlier = tc.draw(gs::vecs(big_input(level)).max_size(2));
    let mut later = tc.draw(gs::vecs(tiny_input()).min_size(1).max_size(3));
    later.push(tc.draw(inputs::runs()));
    for e in encoders {
        let mut enc = e.make(level);
        for d in &earlier {
            enc.compress_to_vec(d);
        }
        for d in &later {
            let stream = enc.compress_to_vec(d);
            assert!(
                stream == e.run_fresh(d, level),
                "{}: depends on earlier calls ({} bytes after {} big inputs)",
                e.name(),
                d.len(),
                earlier.len()
            );
            check_matches_libbz2(&e.name(), &stream, d, level);
        }
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
