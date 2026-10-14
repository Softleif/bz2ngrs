//! Forged streams: valid bzip2 framing around an arbitrary BWT last column
//! `L` and an arbitrary `origPtr`, plus a naive reference decoder.
//!
//! Nothing here uses `bz2ngrs`: the writer follows the format (header, block
//! header, symbol map, selectors, delta-coded code lengths, RUNA/RUNB and MTF
//! symbols, end-of-stream), and the reference decoder follows libbz2's
//! semantics (inverse BWT through the T vector, derandomisation, then the
//! first run-length stage undone). The CRC and the `RAND_NUMS` table are
//! libbz2's (`libbz2-rs-sys` 0.2.5 `randtable.rs`, bzip2 license).
#![allow(
    clippy::cast_possible_truncation,
    reason = "values are reduced modulo a bound first"
)]

use hegel::TestCase;
use hegel::generators::{self as gs};

use super::inputs::Rng;

/// Rows a block of a stream with block-size digit `digit` may hold.
pub fn max_rows(digit: u8) -> usize {
    100_000 * usize::from(digit)
}

/// Largest text the `Text` shape builds: its forward BWT sorts rotations.
const TEXT_CAP: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Independent symbols.
    Random,
    /// Sorted: the permutation is the identity, every row a fixed point.
    Sorted,
    /// Runs of equal symbols, as in real BWT output.
    Runs,
    /// The BWT of a run-heavy text, so `L` looks like real data.
    Text,
}

/// Everything needed to write one block and to rebuild its `L`.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub n: usize,
    pub shape: Shape,
    /// Distinct symbols, 1 to 256.
    pub alphabet: usize,
    pub seed: u64,
    pub orig_ptr: u32,
    pub randomised: bool,
    /// Per Huffman table, how far its code length lies above the minimum.
    pub table_extra: Vec<u8>,
    pub selector_seed: u64,
}

impl Recipe {
    /// The last column: `n` symbols, spread over the byte range so the symbol
    /// map has bits in many of its 16 groups.
    pub fn column(&self) -> Vec<u8> {
        let mut rng = Rng(self.seed);
        let stride = ((rng.next() as u8) | 1) as usize;
        let offset = rng.next() as usize;
        let alphabet = self.alphabet.clamp(1, 256) as u64;
        let sym = |rng: &mut Rng| {
            ((((rng.next() >> 8) % alphabet) as usize * stride + offset) % 256) as u8
        };
        let runs = |rng: &mut Rng, n: usize| {
            let mut out = Vec::with_capacity(n);
            while out.len() < n {
                let x = rng.next();
                let len = 1 + ((x >> 20) as usize) % (1 << ((x >> 16) % 12));
                let s = sym(rng);
                out.resize(n.min(out.len() + len), s);
            }
            out
        };
        match self.shape {
            Shape::Random => (0..self.n).map(|_| sym(&mut rng)).collect(),
            Shape::Sorted => {
                let mut l: Vec<u8> = (0..self.n).map(|_| sym(&mut rng)).collect();
                l.sort_unstable();
                l
            }
            Shape::Runs => runs(&mut rng, self.n),
            Shape::Text => {
                let text = runs(&mut rng, self.n);
                let mut starts: Vec<usize> = (0..text.len()).collect();
                starts.sort_by(|&a, &b| {
                    let t = &text;
                    let rot = |s: usize| (0..t.len()).map(move |k| t[(s + k) % t.len()]);
                    rot(a).cmp(rot(b))
                });
                starts
                    .iter()
                    .map(|&s| text[(s + text.len() - 1) % text.len()])
                    .collect()
            }
        }
    }
}

/// libbz2's CRC-32: MSB first, polynomial 0x04C11DB7, inverted at both ends.
pub fn block_crc(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    !crc
}

/// Decodes one block the way libbz2 does. Returns the bytes produced and
/// whether the block is valid: `origPtr` out of range and a run of four equal
/// bytes at the very end of the block (no count byte) are not. For the second
/// case libbz2 first emits the run and a count byte read from past the end of
/// the block, so the bytes are a prefix of what libbz2 writes before failing.
pub fn naive_block(l: &[u8], orig_ptr: usize, randomised: bool) -> (Vec<u8>, bool) {
    let n = l.len();
    if orig_ptr >= n {
        return (Vec::new(), false);
    }
    // t[cf[c] + k] is the row of the k-th occurrence of c in L.
    let mut next = vec![0usize; n];
    let mut cf = [0usize; 256];
    for &c in l {
        cf[usize::from(c)] += 1;
    }
    let mut sum = 0;
    for c in &mut cf {
        (*c, sum) = (sum, sum + *c);
    }
    for (row, &c) in l.iter().enumerate() {
        next[cf[usize::from(c)]] = row;
        cf[usize::from(c)] += 1;
    }
    let mut text = Vec::with_capacity(n + 1);
    let mut row = next[orig_ptr];
    for _ in 0..=n {
        text.push(l[row]);
        row = next[row];
    }
    // The extra symbol is the one libbz2 reads past the block's end.
    let past_end = text.pop().unwrap_or_default();
    if randomised {
        let mut at = 0usize;
        for &step in RAND_NUMS.iter().cycle() {
            at += usize::from(step);
            if at - 2 >= n {
                break;
            }
            text[at - 2] ^= 1;
        }
    }
    let mut out = Vec::new();
    let (mut run, mut last) = (0, None);
    let mut i = 0;
    while i < n {
        let b = text[i];
        i += 1;
        out.push(b);
        run = if last == Some(b) { run + 1 } else { 1 };
        last = Some(b);
        if run == 4 {
            let count = if i < n { text[i] } else { past_end };
            out.resize(out.len() + usize::from(count), b);
            i += 1;
            (run, last) = (0, None);
        }
    }
    (out, i == n)
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// Appends the low `bits` (at most 32) bits of `v`, most significant first.
    fn put(&mut self, v: u32, bits: u32) {
        self.acc = (self.acc << bits) | u64::from(v & (u32::MAX >> (32 - bits)));
        self.n += bits;
        while self.n >= 8 {
            self.out.push((self.acc >> (self.n - 8)) as u8);
            self.n -= 8;
        }
    }

    fn put48(&mut self, v: u64) {
        self.put((v >> 24) as u32, 24);
        self.put(v as u32 & 0xFF_FFFF, 24);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
        self.out
    }
}

/// MTF and RUNA/RUNB coding of `l`: the symbols used, and the symbol stream
/// (RUNA = 0, RUNB = 1, MTF position + 1, then end of block).
fn mtf_symbols(l: &[u8]) -> (Vec<u8>, Vec<u16>) {
    let mut present = [false; 256];
    for &c in l {
        present[usize::from(c)] = true;
    }
    let used: Vec<u8> = (0..=255u8).filter(|&c| present[usize::from(c)]).collect();
    let mut list = used.clone();
    let mut syms = Vec::new();
    let mut run = 0usize;
    let flush = |syms: &mut Vec<u16>, run: &mut usize| {
        while *run > 0 {
            syms.push(if *run % 2 == 1 { 0 } else { 1 });
            *run = (*run - 1) / 2;
        }
    };
    for &c in l {
        let pos = list.iter().position(|&x| x == c).unwrap_or_default();
        if pos == 0 {
            run += 1;
        } else {
            flush(&mut syms, &mut run);
            list.remove(pos);
            list.insert(0, c);
            syms.push(pos as u16 + 1);
        }
    }
    flush(&mut syms, &mut run);
    syms.push(used.len() as u16 + 1);
    (used, syms)
}

/// libbz2's table of run lengths for the randomised-block mask.
const RAND_NUMS: [u16; 512] = [
    619, 720, 127, 481, 931, 816, 813, 233, 566, 247, 985, 724, 205, 454, 863, 491, 741, 242, 949,
    214, 733, 859, 335, 708, 621, 574, 73, 654, 730, 472, 419, 436, 278, 496, 867, 210, 399, 680,
    480, 51, 878, 465, 811, 169, 869, 675, 611, 697, 867, 561, 862, 687, 507, 283, 482, 129, 807,
    591, 733, 623, 150, 238, 59, 379, 684, 877, 625, 169, 643, 105, 170, 607, 520, 932, 727, 476,
    693, 425, 174, 647, 73, 122, 335, 530, 442, 853, 695, 249, 445, 515, 909, 545, 703, 919, 874,
    474, 882, 500, 594, 612, 641, 801, 220, 162, 819, 984, 589, 513, 495, 799, 161, 604, 958, 533,
    221, 400, 386, 867, 600, 782, 382, 596, 414, 171, 516, 375, 682, 485, 911, 276, 98, 553, 163,
    354, 666, 933, 424, 341, 533, 870, 227, 730, 475, 186, 263, 647, 537, 686, 600, 224, 469, 68,
    770, 919, 190, 373, 294, 822, 808, 206, 184, 943, 795, 384, 383, 461, 404, 758, 839, 887, 715,
    67, 618, 276, 204, 918, 873, 777, 604, 560, 951, 160, 578, 722, 79, 804, 96, 409, 713, 940,
    652, 934, 970, 447, 318, 353, 859, 672, 112, 785, 645, 863, 803, 350, 139, 93, 354, 99, 820,
    908, 609, 772, 154, 274, 580, 184, 79, 626, 630, 742, 653, 282, 762, 623, 680, 81, 927, 626,
    789, 125, 411, 521, 938, 300, 821, 78, 343, 175, 128, 250, 170, 774, 972, 275, 999, 639, 495,
    78, 352, 126, 857, 956, 358, 619, 580, 124, 737, 594, 701, 612, 669, 112, 134, 694, 363, 992,
    809, 743, 168, 974, 944, 375, 748, 52, 600, 747, 642, 182, 862, 81, 344, 805, 988, 739, 511,
    655, 814, 334, 249, 515, 897, 955, 664, 981, 649, 113, 974, 459, 893, 228, 433, 837, 553, 268,
    926, 240, 102, 654, 459, 51, 686, 754, 806, 760, 493, 403, 415, 394, 687, 700, 946, 670, 656,
    610, 738, 392, 760, 799, 887, 653, 978, 321, 576, 617, 626, 502, 894, 679, 243, 440, 680, 879,
    194, 572, 640, 724, 926, 56, 204, 700, 707, 151, 457, 449, 797, 195, 791, 558, 945, 679, 297,
    59, 87, 824, 713, 663, 412, 693, 342, 606, 134, 108, 571, 364, 631, 212, 174, 643, 304, 329,
    343, 97, 430, 751, 497, 314, 983, 374, 822, 928, 140, 206, 73, 263, 980, 736, 876, 478, 430,
    305, 170, 514, 364, 692, 829, 82, 855, 953, 676, 246, 369, 970, 294, 750, 807, 827, 150, 790,
    288, 923, 804, 378, 215, 828, 592, 281, 565, 555, 710, 82, 896, 831, 547, 261, 524, 462, 293,
    465, 502, 56, 661, 821, 976, 991, 658, 869, 905, 758, 745, 193, 768, 550, 608, 933, 378, 286,
    215, 979, 792, 961, 61, 688, 793, 644, 986, 403, 106, 366, 905, 644, 372, 567, 466, 434, 645,
    210, 389, 550, 919, 135, 780, 773, 635, 389, 707, 100, 626, 958, 165, 504, 920, 176, 193, 713,
    857, 265, 203, 50, 668, 108, 645, 990, 626, 197, 510, 357, 358, 850, 858, 364, 936, 638,
];

/// Writes one block. `crc` is its stored CRC.
fn write_block(bits: &mut Bits, r: &Recipe, l: &[u8], crc: u32) {
    let (used, syms) = mtf_symbols(l);
    bits.put48(0x3141_5926_5359);
    bits.put(crc, 32);
    bits.put(u32::from(r.randomised), 1);
    bits.put(r.orig_ptr, 24);

    let mut groups = 0u32;
    for &u in &used {
        groups |= 1 << (15 - u / 16);
    }
    bits.put(groups, 16);
    for g in 0..16u8 {
        if groups >> (15 - g) & 1 == 1 {
            let map = used
                .iter()
                .filter(|&&u| u / 16 == g)
                .fold(0u32, |m, &u| m | 1 << (15 - u % 16));
            bits.put(map, 16);
        }
    }

    let alpha = used.len() as u32 + 2;
    let min_len = (alpha - 1).ilog2() + 1;
    let tables = r.table_extra.len() as u32;
    let lens: Vec<u32> = r
        .table_extra
        .iter()
        .map(|&x| {
            min_len
                + if x < 128 {
                    0
                } else {
                    u32::from(x) % (21 - min_len)
                }
        })
        .collect();
    let n_sel = syms.len().div_ceil(50);
    bits.put(tables, 3);
    bits.put(n_sel as u32, 15);
    let mut rng = Rng(r.selector_seed);
    let mut order: Vec<u32> = (0..tables).collect();
    let mut selectors = Vec::with_capacity(n_sel);
    for _ in 0..n_sel {
        let s = (rng.next() >> 8) as u32 % tables;
        let pos = order.iter().position(|&x| x == s).unwrap_or_default();
        order.remove(pos);
        order.insert(0, s);
        for _ in 0..pos {
            bits.put(1, 1);
        }
        bits.put(0, 1);
        selectors.push(s as usize);
    }
    for (t, &len) in lens.iter().enumerate() {
        // Starting away from the target exercises the delta coding.
        let mut cur = 1 + (u32::from(r.table_extra[t]) * 7 + 3) % 20;
        bits.put(cur, 5);
        for _ in 0..alpha {
            while cur != len {
                bits.put(if cur < len { 0b10 } else { 0b11 }, 2);
                cur = if cur < len { cur + 1 } else { cur - 1 };
            }
            bits.put(0, 1);
        }
    }
    for (i, &s) in syms.iter().enumerate() {
        bits.put(u32::from(s), lens[selectors[i / 50]]);
    }
}

/// A forged stream and what both oracles must make of it.
pub struct Forged {
    pub stream: Vec<u8>,
    /// The decoded bytes, or `None` when the stream is invalid.
    pub expected: Option<Vec<u8>>,
    /// An output capacity that no byte libbz2 writes before failing exceeds.
    pub cap: usize,
}

/// Writes the blocks of `spec`, each with a correct CRC for what it decodes
/// to. An invalid block (see [`naive_block`], or more rows than the digit
/// allows) gets the CRC of its `origPtr` taken modulo its rows, so that a
/// decoder that ignored the defect would see a plausible CRC.
pub fn forge(spec: &Spec) -> Forged {
    let mut bits = Bits {
        out: b"BZh".to_vec(),
        acc: 0,
        n: 0,
    };
    bits.put(u32::from(b'0' + spec.digit), 8);
    let (mut combined, mut data, mut valid, mut cap) = (0u32, Vec::new(), true, 1);
    for r in &spec.blocks {
        let l = r.column();
        let (out, ok) = naive_block(&l, r.orig_ptr as usize, r.randomised);
        valid &= ok && l.len() <= max_rows(spec.digit);
        let crc = block_crc(&naive_block(&l, r.orig_ptr as usize % l.len(), r.randomised).0);
        write_block(&mut bits, r, &l, crc);
        combined = combined.rotate_left(1) ^ crc;
        cap += out.len() + 260;
        data.extend(out);
    }
    bits.put48(0x1772_4538_5090);
    bits.put(combined, 32);
    Forged {
        stream: bits.finish(),
        expected: valid.then_some(data),
        cap,
    }
}

/// A block-size digit and the blocks of one stream.
#[derive(Debug, Clone)]
pub struct Spec {
    pub digit: u8,
    pub blocks: Vec<Recipe>,
}

#[derive(Debug, Clone, Copy)]
pub enum OrigClass {
    InRange,
    OutOfRange,
    Any,
}

fn below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
}

/// Row counts: small, at the decoder's lane-count thresholds (16384 and
/// 65536) and at the block limit, or anywhere. Capped at materialisation.
fn rows(tc: &TestCase, cap: usize) -> usize {
    let n = match tc.draw(gs::integers::<u8>().max_value(5)) {
        0..=2 => 1 + below(tc, 40),
        3 => {
            let edges = [
                1,
                2,
                3,
                2047,
                2048,
                2049,
                16383,
                16384,
                16385,
                65535,
                65536,
                65537,
                cap,
                cap.saturating_sub(1),
            ];
            edges[below(tc, edges.len())]
        }
        4 => 1 + below(tc, cap),
        _ => 1 + below(tc, 3000),
    };
    n.clamp(1, cap)
}

fn origin(tc: &TestCase, n: usize, max: usize, class: OrigClass) -> u32 {
    let valid = match class {
        OrigClass::InRange => true,
        OrigClass::OutOfRange => false,
        OrigClass::Any => tc.draw(gs::integers::<u8>().max_value(7)) < 6,
    };
    let at = if valid {
        if tc.draw(gs::booleans()) {
            n - 1
        } else {
            below(tc, n)
        }
    } else {
        match tc.draw(gs::integers::<u8>().max_value(2)) {
            0 => n + below(tc, 3),
            1 => max + 10 + below(tc, 2),
            _ => n + below(tc, 0x100_0000 - n),
        }
    };
    at as u32
}

/// One block of at most `cap` rows (and at most the digit's limit).
#[hegel::composite]
pub fn recipe(tc: &TestCase, digit: u8, cap: usize, class: OrigClass) -> Recipe {
    let shape = tc.draw(gs::sampled_from(vec![
        Shape::Random,
        Shape::Sorted,
        Shape::Runs,
        Shape::Text,
    ]));
    let mut n = rows(tc, cap.min(max_rows(digit)));
    if shape == Shape::Text {
        n = n.min(TEXT_CAP);
    }
    let alphabet = match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => 1 + below(tc, 3),
        1 => tc.draw(gs::sampled_from(vec![4usize, 16, 17, 255, 256])),
        _ => 1 + below(tc, 256),
    };
    Recipe {
        n,
        shape,
        alphabet,
        seed: tc.draw(gs::integers::<u64>()),
        orig_ptr: origin(tc, n, max_rows(digit), class),
        randomised: tc.draw(gs::booleans()),
        table_extra: tc.draw(gs::vecs(gs::integers::<u8>()).min_size(2).max_size(6)),
        selector_seed: tc.draw(gs::integers::<u64>()),
    }
}

fn digit(tc: &TestCase) -> u8 {
    tc.draw(gs::integers::<u8>().min_value(1).max_value(9))
}

/// One block of any size the digit allows.
#[hegel::composite]
pub fn one_block(tc: &TestCase, class: OrigClass) -> Spec {
    let digit = digit(tc);
    let blocks = vec![tc.draw(recipe(digit, usize::MAX, class))];
    Spec { digit, blocks }
}

/// Two to four blocks of up to 70000 rows each.
#[hegel::composite]
pub fn several_blocks(tc: &TestCase) -> Spec {
    let digit = digit(tc);
    let count = 2 + below(tc, 3);
    let blocks = (0..count)
        .map(|_| tc.draw(recipe(digit, 70_000, OrigClass::Any)))
        .collect();
    Spec { digit, blocks }
}

/// One block of one to three rows more than the digit allows.
#[hegel::composite]
pub fn oversize_block(tc: &TestCase) -> Spec {
    let digit = digit(tc);
    let mut r = tc.draw(recipe(digit, 1, OrigClass::InRange));
    r.n = max_rows(digit) + 1 + below(tc, 3);
    if r.shape == Shape::Text {
        r.shape = Shape::Runs;
    }
    Spec {
        digit,
        blocks: vec![r],
    }
}
