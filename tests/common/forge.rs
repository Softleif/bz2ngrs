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
    /// A few distinct symbols in a fixed cycle: no two neighbours are equal,
    /// so every row is a literal symbol of its own (a block of 900 000 rows
    /// needs 18 001 selectors).
    Cycle,
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
    /// Selectors written beyond the ones the symbols need. libbz2 1.0.8 reads
    /// up to 32767 and keeps at most 18002.
    pub extra_selectors: u32,
    /// Code lengths that differ within a table (canonical Huffman codes),
    /// rather than one length for every symbol.
    pub varied: bool,
    /// A defect written into the block's header or symbols; the stream is
    /// otherwise valid.
    pub defect: Option<Defect>,
}

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;

/// What is wrong with a block. Each one breaks exactly one format rule, at a
/// place libbz2 reads before any later rule can fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defect {
    /// This 48-bit value, neither magic, where the block magic belongs.
    Magic(u64),
    /// An empty symbol map.
    NoSymbols,
    /// This table count (0, 1 or 7), outside 2 to 6.
    TableCount(u8),
    /// A selector count of 0.
    NoSelectors,
    /// A selector whose unary code has `tables` or more ones.
    SelectorBeyondTables,
    /// A first code length of 0 or 21 to 31.
    CodeStart(u8),
    /// A code length stepped down from 1 to 0.
    CodeBelow,
    /// A code length stepped up from 20 to 21.
    CodeAbove,
    /// A symbol coded with a bit pattern no code of its table has.
    UnassignedCode,
    /// The end-of-block symbol left out, and the symbols padded to fill the
    /// last selector's group of 50.
    NoEndOfBlock,
    /// This many RUNB symbols in a row at the start of the block.
    Run(u32),
}

/// The rule a defect breaks (the `Corruption` kinds that forged streams can
/// reach), named here so that this module does not depend on the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    OrigPtr,
    BlockTooLong,
    BlockMagic,
    NoSymbols,
    GroupCount,
    SelectorCount,
    Selector,
    CodeLength,
    HuffmanCode,
    NoEndOfBlock,
    RunTooLong,
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
            Shape::Cycle => {
                let period = 2 + (self.seed >> 8) as usize % 7;
                (0..self.n)
                    .map(|i| (((i % period) * stride + offset) % 256) as u8)
                    .collect()
            }
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
    naive_block_with(l, orig_ptr, randomised, None)
}

/// [`naive_block`] with the count byte of a run of four at the very end of the
/// block taken to be `past_end` instead of the byte libbz2 reads there.
pub fn naive_block_with(
    l: &[u8],
    orig_ptr: usize,
    randomised: bool,
    past_end: Option<u8>,
) -> (Vec<u8>, bool) {
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
    let past_end = past_end.unwrap_or(text.pop().unwrap_or_default());
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

    /// Bits written so far.
    fn pos(&self) -> u64 {
        self.out.len() as u64 * 8 + u64::from(self.n)
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

/// Code lengths (1 to 20) of a prefix code over `alpha` symbols: the leaves
/// of a random binary tree, so a complete code, a quarter of the time with
/// some lengthened by one, so an incomplete one. Shuffled over the symbols.
fn varied_lens(rng: &mut Rng, alpha: usize) -> Vec<u32> {
    let mut lens = vec![1u32, 1];
    while lens.len() < alpha {
        let i = (rng.next() >> 8) as usize % lens.len();
        if lens[i] < 20 {
            lens[i] += 1;
            lens.push(lens[i]);
        }
    }
    for i in (1..lens.len()).rev() {
        lens.swap(i, (rng.next() >> 8) as usize % (i + 1));
    }
    if rng.next() & 3 == 0 {
        for l in &mut lens {
            if *l < 20 && rng.next().is_multiple_of(8) {
                *l += 1;
            }
        }
    }
    lens
}

/// The canonical code of each symbol (libbz2's `BZ2_hbAssignCodes`): by
/// length, then by symbol, each code one more than the last, shifted up at
/// every step in length.
fn canonical(lens: &[u32]) -> Vec<u32> {
    let mut codes = vec![0u32; lens.len()];
    let mut code = 0u32;
    for len in 1..=20 {
        for (sym, &l) in lens.iter().enumerate() {
            if l == len {
                codes[sym] = code;
                code += 1;
            }
        }
        code <<= 1;
    }
    codes
}

/// What [`write_block`] did.
struct Written {
    /// Whether the recipe's defect took effect (some need a block of a
    /// certain shape and are not written otherwise).
    applied: bool,
    /// Where in the stream the defect sits: the bit it starts at and the bit
    /// after its last.
    field: Option<(u64, u64)>,
}

/// Writes one block. `crc` is its stored CRC.
fn write_block(bits: &mut Bits, r: &Recipe, l: &[u8], crc: u32) -> Written {
    let (used, mut syms) = mtf_symbols(l);
    let alpha = used.len() as u32 + 2;
    let min_len = (alpha - 1).ilog2() + 1;
    let tables = r.table_extra.len() as u32;
    let mut applied = true;
    let mut field = None;

    // The run goes where it cannot join a run of the block's own.
    let mut run_at = 0;
    match r.defect {
        Some(Defect::Run(k)) => {
            let at = (0..syms.len()).find(|&p| syms[p] >= 2 && (p == 0 || syms[p - 1] >= 2));
            applied = at.is_some();
            run_at = at.unwrap_or_default();
            syms.splice(run_at..run_at, std::iter::repeat_n(1u16, k as usize));
        }
        Some(Defect::NoEndOfBlock) => {
            syms.pop();
            applied = used.len() >= 2;
            // Literals (MTF index 1) up to a whole last group.
            while applied && (syms.len() % 50 != 0 || syms.is_empty()) {
                syms.push(2);
            }
        }
        _ => {}
    }

    let at = bits.pos();
    bits.put48(match r.defect {
        Some(Defect::Magic(m)) => m,
        _ => BLOCK_MAGIC,
    });
    if matches!(r.defect, Some(Defect::Magic(_))) {
        field = Some((at, bits.pos()));
    }
    bits.put(crc, 32);
    bits.put(u32::from(r.randomised), 1);
    bits.put(r.orig_ptr, 24);

    let at = bits.pos();
    let mut groups = 0u32;
    for &u in &used {
        groups |= 1 << (15 - u / 16);
    }
    if r.defect == Some(Defect::NoSymbols) {
        groups = 0;
        field = Some((at, at + 16));
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

    // Per table and symbol, the code length: all the same (a flat code), or
    // varied. A flat code longer than needed leaves bit patterns unassigned.
    let varied = r.varied && r.defect != Some(Defect::UnassignedCode);
    let lens: Vec<Vec<u32>> = r
        .table_extra
        .iter()
        .enumerate()
        .map(|(t, &x)| {
            if varied {
                varied_lens(
                    &mut Rng(r.seed ^ ((t as u64 + 1) << 40) ^ u64::from(x)),
                    alpha as usize,
                )
            } else {
                let mut len = min_len;
                if x >= 128 {
                    len += u32::from(x) % (21 - min_len);
                }
                if r.defect == Some(Defect::UnassignedCode) {
                    len = len.max(min_len + 1);
                }
                vec![len; alpha as usize]
            }
        })
        .collect();
    let codes: Vec<Vec<u32>> = lens.iter().map(|l| canonical(l)).collect();

    let n_sel = syms.len().div_ceil(50);
    let n_written = (n_sel + r.extra_selectors as usize).min(32767);
    let at = bits.pos();
    bits.put(
        match r.defect {
            Some(Defect::TableCount(t)) => u32::from(t),
            _ => tables,
        },
        3,
    );
    if matches!(r.defect, Some(Defect::TableCount(_))) {
        field = Some((at, bits.pos()));
    }
    let at = bits.pos();
    let declared = if r.defect == Some(Defect::NoSelectors) {
        0
    } else {
        n_written as u32
    };
    bits.put(declared, 15);
    if r.defect == Some(Defect::NoSelectors) {
        field = Some((at, bits.pos()));
    }
    let bad_selector = (r.selector_seed >> 33) as usize % n_written;
    let mut rng = Rng(r.selector_seed);
    let mut order: Vec<u32> = (0..tables).collect();
    let mut selectors = Vec::with_capacity(n_written);
    for i in 0..n_written {
        let s = (rng.next() >> 8) as u32 % tables;
        let pos = order.iter().position(|&x| x == s).unwrap_or_default();
        order.remove(pos);
        order.insert(0, s);
        let at = bits.pos();
        let ones = if r.defect == Some(Defect::SelectorBeyondTables) && i == bad_selector {
            tables + (r.selector_seed >> 20) as u32 % 3
        } else {
            pos as u32
        };
        for _ in 0..ones {
            bits.put(1, 1);
        }
        bits.put(0, 1);
        if ones >= tables {
            field = Some((at, bits.pos()));
        }
        selectors.push(s as usize);
    }

    for (t, tl) in lens.iter().enumerate() {
        let at = bits.pos();
        let bad = match (t, r.defect) {
            (0, Some(Defect::CodeStart(v))) => Some((u32::from(v), 5, None)),
            (0, Some(Defect::CodeBelow)) => Some((1, 5, Some(0b11))),
            (0, Some(Defect::CodeAbove)) => Some((20, 5, Some(0b10))),
            _ => None,
        };
        if let Some((start, width, step)) = bad {
            bits.put(start, width);
            if let Some(step) = step {
                bits.put(step, 2);
            }
            for _ in 0..alpha {
                bits.put(0, 1);
            }
            field = Some((at, bits.pos()));
            continue;
        }
        // Starting away from the target exercises the delta coding.
        let mut cur = 1 + (u32::from(r.table_extra[t]) * 7 + 3) % 20;
        bits.put(cur, 5);
        for &len in tl {
            while cur != len {
                bits.put(if cur < len { 0b10 } else { 0b11 }, 2);
                cur = if cur < len { cur + 1 } else { cur - 1 };
            }
            bits.put(0, 1);
        }
    }

    let bad_symbol = (r.selector_seed >> 7) as usize % syms.len();
    let mut last_group_at = 0;
    let (mut run_start, mut run_end) = (0, 0);
    for (i, &s) in syms.iter().enumerate() {
        let at = bits.pos();
        let t = selectors[i / 50];
        if i == (n_sel - 1) * 50 {
            last_group_at = at;
        }
        if r.defect == Some(Defect::UnassignedCode) && i == bad_symbol {
            let len = lens[t][0];
            let v = alpha + (r.selector_seed >> 40) as u32 % ((1 << len) - alpha);
            bits.put(v, len);
            field = Some((at, bits.pos()));
        } else {
            bits.put(codes[t][usize::from(s)], lens[t][usize::from(s)]);
        }
        if let Some(Defect::Run(k)) = r.defect {
            if i == run_at {
                run_start = at;
            }
            if i == run_at + k as usize {
                run_end = bits.pos();
            }
        }
    }
    match r.defect {
        Some(Defect::NoEndOfBlock) => field = Some((last_group_at, bits.pos())),
        // The error comes at the 22nd symbol of the run, or at the symbol
        // after it.
        Some(Defect::Run(_)) => field = Some((run_start, run_end)),
        _ => {}
    }
    Written { applied, field }
}

/// A forged stream and what both oracles must make of it.
pub struct Forged {
    pub stream: Vec<u8>,
    /// The decoded bytes, or `None` when the stream is invalid.
    pub expected: Option<Vec<u8>>,
    /// An output capacity that no byte libbz2 writes before failing exceeds.
    pub cap: usize,
    /// Whether every defect in the spec took effect.
    pub applied: bool,
    /// The bit the last block begins at.
    pub last_block_bit: u64,
    /// The bit range of the last defect written.
    pub field: Option<(u64, u64)>,
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
    let (mut applied, mut last_block_bit, mut field) = (true, 0, None);
    for r in &spec.blocks {
        let l = r.column();
        let (out, ok) = naive_block(&l, r.orig_ptr as usize, r.randomised);
        valid &= ok && l.len() <= max_rows(spec.digit) && r.defect.is_none();
        // For a block that ends in four equal bytes, half the time the CRC is
        // that of the bytes before the missing count, so that a decoder that
        // took the count to be 0 would see a CRC that fits; the other half it
        // is that of what libbz2 writes, count and all.
        let count = (r.seed & 1 == 1).then_some(0);
        let crc =
            block_crc(&naive_block_with(&l, r.orig_ptr as usize % l.len(), r.randomised, count).0);
        last_block_bit = bits.pos();
        let w = write_block(&mut bits, r, &l, crc);
        applied &= w.applied;
        field = w.field.or(field);
        combined = combined.rotate_left(1) ^ crc;
        cap += out.len() + 260;
        data.extend(out);
    }
    bits.put48(END_MAGIC);
    bits.put(combined, 32);
    Forged {
        stream: bits.finish(),
        expected: valid.then_some(data),
        cap,
        applied,
        last_block_bit,
        field,
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
        Shape::Cycle,
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
    // Selectors past what the symbols need: none, a few, or enough to pass the
    // 18002 libbz2 keeps.
    let extra_selectors = match tc.draw(gs::integers::<u8>().max_value(7)) {
        0..=4 => 0,
        5 => 1 + below(tc, 100) as u32,
        _ => 18_003 + below(tc, 14_000) as u32,
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
        extra_selectors,
        varied: tc.draw(gs::booleans()),
        defect: None,
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

/// One block of 899 950 to 900 000 rows at digit 9, every row a literal
/// symbol, so 18 000 to 18 001 selectors: the most a block can need.
#[hegel::composite]
pub fn largest_block(tc: &TestCase) -> Spec {
    let mut r = tc.draw(recipe(9, 3000, OrigClass::Any));
    r.shape = Shape::Cycle;
    r.n = max_rows(9) - below(tc, 51);
    r.orig_ptr = origin(tc, r.n, max_rows(9), OrigClass::InRange);
    r.extra_selectors = 0;
    Spec {
        digit: 9,
        blocks: vec![r],
    }
}

/// A valid block with more selectors than libbz2 keeps: every one past 18002
/// is read and ignored.
#[hegel::composite]
pub fn excess_selectors(tc: &TestCase) -> Spec {
    let digit = digit(tc);
    let mut r = tc.draw(recipe(digit, 3000, OrigClass::InRange));
    r.extra_selectors = 18_003 + below(tc, 14_000) as u32;
    Spec {
        digit,
        blocks: vec![r],
    }
}

/// A spec whose last block breaks `rule`, and that rule.
#[derive(Debug, Clone)]
pub struct Broken {
    pub spec: Spec,
    pub rule: Rule,
}

/// Zero to three valid blocks, then one whose `origPtr` is out of range.
#[hegel::composite]
pub fn bad_orig_ptr_last(tc: &TestCase) -> Broken {
    let digit = digit(tc);
    let mut blocks: Vec<Recipe> = (0..below(tc, 4))
        .map(|_| tc.draw(recipe(digit, 3000, OrigClass::InRange)))
        .collect();
    blocks.push(tc.draw(recipe(digit, usize::MAX, OrigClass::OutOfRange)));
    Broken {
        spec: Spec { digit, blocks },
        rule: Rule::OrigPtr,
    }
}

/// Zero to two valid blocks, then one of one to three rows more than the
/// digit allows.
#[hegel::composite]
pub fn oversize_block(tc: &TestCase) -> Broken {
    let digit = digit(tc);
    let mut blocks: Vec<Recipe> = (0..below(tc, 3))
        .map(|_| tc.draw(recipe(digit, 3000, OrigClass::InRange)))
        .collect();
    let mut last = tc.draw(recipe(digit, 1, OrigClass::InRange));
    last.n = max_rows(digit) + 1 + below(tc, 3);
    if last.shape == Shape::Text {
        last.shape = Shape::Runs;
    }
    blocks.push(last);
    Broken {
        spec: Spec { digit, blocks },
        rule: Rule::BlockTooLong,
    }
}

/// A 48-bit value that is neither the block nor the end-of-stream magic: one
/// of them with a bit flipped (so its first bytes may still match), or any.
fn wrong_magic(tc: &TestCase) -> u64 {
    let m = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => BLOCK_MAGIC ^ (1 << below(tc, 48)),
        1 => END_MAGIC ^ (1 << below(tc, 48)),
        _ => tc.draw(gs::integers::<u64>()) & ((1 << 48) - 1),
    };
    if m == BLOCK_MAGIC || m == END_MAGIC {
        m ^ 1
    } else {
        m
    }
}

/// Zero to two valid blocks, then one that breaks `rule` in its header or
/// symbols and is otherwise valid. `rule` is one of the header rules (not
/// `OrigPtr`, `BlockTooLong` or `RunTooLong`, which have their own
/// generators).
#[hegel::composite]
pub fn defective(tc: &TestCase, rule: Rule) -> Broken {
    let digit = digit(tc);
    let mut blocks: Vec<Recipe> = (0..below(tc, 3))
        .map(|_| tc.draw(recipe(digit, 3000, OrigClass::InRange)))
        .collect();
    let mut last = tc.draw(recipe(digit, 3000, OrigClass::InRange));
    if rule == Rule::NoEndOfBlock {
        // An extra selector gives the decoder another group to find it in.
        last.extra_selectors = 0;
    }
    last.defect = Some(match rule {
        Rule::BlockMagic => Defect::Magic(wrong_magic(tc)),
        Rule::NoSymbols => Defect::NoSymbols,
        Rule::GroupCount => Defect::TableCount(tc.draw(gs::sampled_from(vec![0u8, 1, 7]))),
        Rule::SelectorCount => Defect::NoSelectors,
        Rule::Selector => Defect::SelectorBeyondTables,
        Rule::CodeLength => match tc.draw(gs::integers::<u8>().max_value(2)) {
            0 => Defect::CodeStart(0),
            1 => Defect::CodeStart(21 + below(tc, 11) as u8),
            _ => tc.draw(gs::sampled_from(vec![Defect::CodeBelow, Defect::CodeAbove])),
        },
        Rule::HuffmanCode => Defect::UnassignedCode,
        Rule::NoEndOfBlock => Defect::NoEndOfBlock,
        Rule::OrigPtr | Rule::BlockTooLong | Rule::RunTooLong => {
            unreachable!("has its own generator")
        }
    });
    blocks.push(last);
    Broken {
        spec: Spec { digit, blocks },
        rule,
    }
}

/// A run of RUNB symbols around the length libbz2 refuses: from 19 symbols
/// the run passes the largest block, from 22 it passes 2^21 and is `RunTooLong`
/// instead of `BlockTooLong`.
#[hegel::composite]
pub fn run_boundary(tc: &TestCase) -> Broken {
    let digit = digit(tc);
    let mut blocks: Vec<Recipe> = (0..below(tc, 3))
        .map(|_| tc.draw(recipe(digit, 3000, OrigClass::InRange)))
        .collect();
    let mut last = tc.draw(recipe(digit, 3000, OrigClass::InRange));
    let symbols = 19 + below(tc, 12) as u32;
    last.defect = Some(Defect::Run(symbols));
    blocks.push(last);
    Broken {
        spec: Spec { digit, blocks },
        rule: if symbols >= 22 {
            Rule::RunTooLong
        } else {
            Rule::BlockTooLong
        },
    }
}

/// A spec with any one of the defects above, or a bad `origPtr`.
#[hegel::composite]
pub fn any_defective(tc: &TestCase) -> Broken {
    match tc.draw(gs::integers::<u8>().max_value(9)) {
        0 => tc.draw(bad_orig_ptr_last()),
        1 => tc.draw(run_boundary()),
        2 => tc.draw(defective(Rule::BlockMagic)),
        3 => tc.draw(defective(Rule::NoSymbols)),
        4 => tc.draw(defective(Rule::GroupCount)),
        5 => tc.draw(defective(Rule::SelectorCount)),
        6 => tc.draw(defective(Rule::Selector)),
        7 => tc.draw(defective(Rule::CodeLength)),
        8 => tc.draw(defective(Rule::HuffmanCode)),
        _ => tc.draw(defective(Rule::NoEndOfBlock)),
    }
}
