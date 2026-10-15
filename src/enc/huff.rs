//! The encoder's Huffman stage: table selection, code lengths, and emission
//! of everything that follows the symbol map in a block (selectors,
//! code-length tables, coded MTF values). bz2-ng's `enc_huff.c`.
//!
//! libbz2's algorithm (`compress.c` `sendMTFValues`): 2 to 6 tables by the
//! number of MTF values, the equal-frequency seed partition, 4 iterations of
//! "pick the cheapest table per 50-symbol group (lowest index on ties),
//! rebuild every table with `make_code_lengths` (at most 17 bits)", and
//! canonical codes. The selectors sent are those of the last iteration and
//! the lengths those rebuilt after it, so the output is libbz2's bit for bit.
//!
//! `make_code_lengths` and `assign_codes` are libbzip2 1.0.8's
//! `BZ2_hbMakeCodeLengths` and `BZ2_hbAssignCodes` (`huffman.c`), and
//! `seed_tables` and `selector_mtf` follow `compress.c`; libbzip2 is
//! Copyright (C) 1996-2019 Julian Seward, under the licence in
//! `LICENSE-bzip2`. Altered: names and types are changed and the rest of
//! the stage is rewritten.

use super::BitWriter;
use super::mtf::MAX_ALPHA;

/// libbz2 1.0.8 limits encoder code lengths to 17 bits (`compress.c`).
const MAX_CODE_LEN: u8 = 17;
const N_GROUPS: usize = 6;
const G_SIZE: usize = 50;
const N_ITERS: usize = 4;
const LESSER_ICOST: u8 = 0;
const GREATER_ICOST: u8 = 15;

/// The stage's tables, reused between blocks.
pub(crate) struct Huff {
    len: [[u8; MAX_ALPHA]; N_GROUPS],
    rfreq: [[[i32; MAX_ALPHA]; N_GROUPS]; 2],
    /// Every table's length of a symbol in one word (six 10-bit fields).
    pk: [u64; MAX_ALPHA],
    /// `code << 5 | length` per table and symbol.
    ce: [[u32; MAX_ALPHA]; N_GROUPS],
    selector: Vec<u8>,
    prev_sel: Vec<u8>,
    lengths: Box<Lengths>,
}

impl Huff {
    pub(crate) fn new_boxed() -> Box<Huff> {
        Box::new(Huff {
            len: [[0; MAX_ALPHA]; N_GROUPS],
            rfreq: [[[0; MAX_ALPHA]; N_GROUPS]; 2],
            pk: [0; MAX_ALPHA],
            ce: [[0; MAX_ALPHA]; N_GROUPS],
            selector: Vec::new(),
            prev_sel: Vec::new(),
            lengths: Lengths::new_boxed(),
        })
    }
}

/// A heap entry is `weight << 16 | node`, so a weight is read without a
/// second load and `a < b & HI` is "a's weight is below b's".
const HI: u64 = !0xffff;
const HEAP_LEN: usize = MAX_ALPHA + 3;

/// Working memory of [`make_code_lengths_all`].
pub(crate) struct Lengths {
    heap: [[u64; HEAP_LEN]; N_GROUPS],
    parent: [[u16; MAX_ALPHA * 2]; N_GROUPS],
    /// The weight of each table's leaves (index 1 to `alpha`).
    leaf: [[u32; MAX_ALPHA + 1]; N_GROUPS],
}

impl Lengths {
    pub(crate) fn new_boxed() -> Box<Lengths> {
        Box::new(Lengths {
            heap: [[0; HEAP_LEN]; N_GROUPS],
            parent: [[0; MAX_ALPHA * 2]; N_GROUPS],
            leaf: [[0; MAX_ALPHA + 1]; N_GROUPS],
        })
    }
}

/// Inserts `tmp` as entry `n` of the heap `h`.
#[inline(always)]
fn upheap(h: &mut [u64; HEAP_LEN], n: usize, tmp: u64) {
    let mut zz = n;
    while tmp < h[zz >> 1] & HI {
        h[zz] = h[zz >> 1];
        zz >>= 1;
    }
    h[zz] = tmp;
}

/// Takes the smallest entry off each heap of `m + 1` entries. Each heap's
/// last entry is cut off and refills it from the root, as libbz2's
/// `heap[1] = heap[nHeap]; nHeap--; DOWNHEAP(1)`; `m` is the size after.
///
/// A sift-down walks down the smaller children to a leaf and the displaced
/// entry climbs back from there: it ends where libbz2's top-down walk stops,
/// below every entry of at most its weight. The walk takes the same number
/// of steps in every heap, so the heaps advance side by side, which hides
/// the latency of each one's chain of loads. `heap[m + 1]` is an entry no
/// weight is below, so a missing right child never wins.
#[inline(always)]
fn pop<const L: usize>(heap: &mut [[u64; HEAP_LEN]; N_GROUPS], m: usize) -> [u64; L] {
    let mut top = [0; L];
    let mut last = [0; L];
    let mut zz = [1usize; L];
    for l in 0..L {
        top[l] = heap[l][1];
        last[l] = heap[l][m + 1];
        heap[l][m + 1] = u64::MAX;
    }
    // Every node above the last level of the heap has two children.
    for _ in 1..m.checked_ilog2().unwrap_or(0) {
        for l in 0..L {
            let h = &mut heap[l];
            let y = zz[l] * 2;
            let y = y + usize::from(h[y + 1] & HI < h[y] & HI);
            h[zz[l]] = h[y];
            zz[l] = y;
        }
    }
    for l in 0..L {
        let h = &mut heap[l];
        let a = zz[l] * 2;
        let has = a <= m;
        let a = if has { a } else { 0 };
        let y = a + usize::from(h[a + 1] & HI < h[a] & HI);
        let y = if has { y } else { zz[l] };
        h[zz[l]] = h[y];
        zz[l] = y;
    }
    for l in 0..L {
        let h = &mut heap[l];
        let mut z = zz[l];
        while last[l] < h[z >> 1] & HI {
            h[z] = h[z >> 1];
            z >>= 1;
        }
        h[z] = last[l];
    }
    top
}

/// One batch of `make_code_lengths_all`: the lengths of the tables `ts`,
/// into `len`, from the leaf weights in `w.leaf`; returns which exceed
/// `max_len`.
#[inline(always)]
fn build<const L: usize>(
    w: &mut Lengths,
    len: &mut [[u8; MAX_ALPHA]; N_GROUPS],
    ts: [usize; L],
    alpha: usize,
    max_len: u8,
) -> [bool; L] {
    let mut n = 0;
    for i in 1..=alpha {
        n += 1;
        for (h, &t) in w.heap.iter_mut().zip(&ts) {
            upheap(h, n, u64::from(w.leaf[t][i]) << 16 | i as u64);
        }
    }
    let mut n_nodes = alpha;
    while n > 1 {
        let e1 = pop::<L>(&mut w.heap, n - 1);
        let e2 = pop::<L>(&mut w.heap, n - 2);
        n_nodes += 1;
        for l in 0..L {
            w.parent[l][(e1[l] & 0xffff) as usize] = n_nodes as u16;
            w.parent[l][(e2[l] & 0xffff) as usize] = n_nodes as u16;
            let (w1, w2) = ((e1[l] >> 16) as u32, (e2[l] >> 16) as u32);
            let sum = ((w1 & !0xff) + (w2 & !0xff)) | (1 + (w1 & 0xff).max(w2 & 0xff));
            upheap(&mut w.heap[l], n - 1, u64::from(sum) << 16 | n_nodes as u64);
        }
        n -= 1;
    }
    let mut too_long = [false; L];
    for l in 0..L {
        // A node's parent is made after it, so one pass down from the root
        // gives every depth (libbz2 walks from each leaf to the root).
        let mut depth = [0u8; MAX_ALPHA * 2];
        for k in (1..n_nodes).rev() {
            depth[k] = depth[usize::from(w.parent[l][k])] + 1;
        }
        for (out, &d) in len[ts[l]][..alpha].iter_mut().zip(&depth[1..=alpha]) {
            *out = d;
            too_long[l] |= d > max_len;
        }
    }
    too_long
}

/// libbz2's `BZ2_hbMakeCodeLengths` for each of the tables `ts`: code
/// lengths `len[t][..alpha]` for the frequencies `freq[t]`, limited to
/// `max_len` bits by flattening the weights and rebuilding until they fit.
///
/// The heap and its tie-breaking are libbz2's, since which of several equal
/// weights merges first decides the lengths. The tables are built a few at a
/// time, in lock step (they all merge `alpha - 1` times).
pub(super) fn make_code_lengths_all(
    w: &mut Lengths,
    len: &mut [[u8; MAX_ALPHA]; N_GROUPS],
    freq: &[[i32; MAX_ALPHA]; N_GROUPS],
    ts: &[usize],
    alpha: usize,
    max_len: u8,
) {
    for &t in ts {
        for (l, &f) in w.leaf[t][1..=alpha].iter_mut().zip(&freq[t]) {
            *l = (f.max(1) as u32) << 8;
        }
    }
    let mut todo = [0usize; N_GROUPS];
    let mut n_todo = ts.len();
    todo[..n_todo].copy_from_slice(ts);
    while n_todo > 0 {
        let mut too_long = [false; N_GROUPS];
        match todo[..n_todo] {
            [a] => too_long[..1].copy_from_slice(&build(w, len, [a], alpha, max_len)),
            [a, b] => too_long[..2].copy_from_slice(&build(w, len, [a, b], alpha, max_len)),
            [a, b, c] => too_long[..3].copy_from_slice(&build(w, len, [a, b, c], alpha, max_len)),
            [a, b, c, d] => {
                too_long[..4].copy_from_slice(&build(w, len, [a, b, c, d], alpha, max_len));
            }
            [a, b, c, d, e] => {
                too_long[..5].copy_from_slice(&build(w, len, [a, b, c, d, e], alpha, max_len));
            }
            [a, b, c, d, e, f] => {
                too_long.copy_from_slice(&build(w, len, [a, b, c, d, e, f], alpha, max_len));
            }
            _ => unreachable!("one to N_GROUPS tables"),
        }
        let mut n_next = 0;
        for i in 0..n_todo {
            if too_long[i] {
                let t = todo[i];
                for l in &mut w.leaf[t][1..=alpha] {
                    *l = (1 + (*l >> 8) / 2) << 8;
                }
                todo[n_next] = t;
                n_next += 1;
            }
        }
        n_todo = n_next;
    }
}

/// The canonical codes of libbz2's `BZ2_hbAssignCodes` (shortest first, then
/// in symbol order), as `code << 5 | length` per symbol, from the count of
/// each length instead of a pass over the symbols per length.
pub(super) fn assign_codes(ce: &mut [u32], length: &[u8]) {
    let mut count = [0u32; MAX_CODE_LEN as usize + 2];
    for &l in length {
        count[usize::from(l)] += 1;
    }
    let mut next = [0u32; MAX_CODE_LEN as usize + 2];
    for n in 1..=MAX_CODE_LEN as usize {
        next[n + 1] = (next[n] + count[n]) << 1;
    }
    for (e, &l) in ce.iter_mut().zip(length) {
        *e = next[usize::from(l)] << 5 | u32::from(l);
        next[usize::from(l)] += 1;
    }
}

/// The number of coding tables for `nmtf` MTF values.
fn n_groups(nmtf: usize) -> usize {
    match nmtf {
        0..200 => 2,
        200..600 => 3,
        600..1200 => 4,
        1200..2400 => 5,
        _ => 6,
    }
}

/// Initial tables: `n_groups` ranges of roughly equal total frequency, each
/// table cheap (0) inside its range and dear (15) outside.
fn seed_tables(
    len: &mut [[u8; MAX_ALPHA]; N_GROUPS],
    n_groups: usize,
    nmtf: usize,
    freq: &[i32],
    alpha: usize,
) {
    for t in len.iter_mut() {
        t[..alpha].fill(GREATER_ICOST);
    }
    let (mut n_part, mut rem_f, mut gs) = (n_groups, nmtf as i32, 0usize);
    while n_part > 0 {
        let t_freq = rem_f / n_part as i32;
        let mut ge = gs as isize - 1;
        let mut a_freq = 0;
        while a_freq < t_freq && ge < alpha as isize - 1 {
            ge += 1;
            a_freq += freq[ge as usize];
        }
        if ge > gs as isize && n_part != n_groups && n_part != 1 && (n_groups - n_part) % 2 == 1 {
            a_freq -= freq[ge as usize];
            ge -= 1;
        }
        for (v, l) in len[n_part - 1][..alpha].iter_mut().enumerate() {
            *l = if v >= gs && v as isize <= ge {
                LESSER_ICOST
            } else {
                GREATER_ICOST
            };
        }
        n_part -= 1;
        gs = (ge + 1) as usize;
        rem_f -= a_freq;
    }
}

/// A table's code lengths as bzip2 sends them: the first in 5 bits, then for
/// each symbol the steps from the previous length (`10` up, `11` down) and a
/// `0`, the whole change in one write (a change of 16 takes two).
pub(super) fn put_lengths(bw: &mut BitWriter<'_>, lens: &[u8]) {
    let mut curr = lens.first().copied().unwrap_or(0);
    bw.put(5, u64::from(curr));
    for &target in lens {
        let mut d = i32::from(target) - i32::from(curr);
        curr = target;
        while d.abs() > 15 {
            let step = if d > 0 { 15 } else { -15 };
            let (n, v) = delta_bits(step);
            bw.put(n - 1, v >> 1);
            d -= step;
        }
        let (n, v) = delta_bits(d);
        bw.put(n, v);
    }
}

/// The bits of a change `d` (at most 15 either way) of the code length: its
/// count and value.
#[inline(always)]
fn delta_bits(d: i32) -> (u32, u64) {
    let k = d.unsigned_abs();
    let steps = if d > 0 {
        0xAAAA_AAAA_AAAA_AAAAu64 >> (64 - 2 * k)
    } else {
        (1u64 << (2 * k)) - 1
    };
    (2 * k + 1, steps << 1)
}

/// Selector MTF: each selector's position in a move-to-front list of the
/// tables.
fn selector_mtf(sel: u8, pos: &mut [u8; N_GROUPS]) -> u32 {
    let mut tmp = pos[0];
    let mut j = 0;
    while sel != tmp {
        j += 1;
        std::mem::swap(&mut tmp, &mut pos[j]);
    }
    pos[0] = tmp;
    j as u32
}

/// Table selection, code lengths, and emission of selectors, code-length
/// tables and the coded MTF values `mtfv` (counts `freq`) over an alphabet
/// of `alpha` symbols.
///
/// libbz2's algorithm, with the work done differently: all table costs of a
/// symbol are packed into one word, so that a group's costs for every table
/// are one load and one add per symbol (a group costs at most 50 * 17 < 1024
/// per table); and after the first iteration only groups whose selector
/// changed move their symbols between the tables' counts, which gives the
/// counts libbz2 gets by recounting from zero.
pub(crate) fn encode(
    h: &mut Huff,
    mtfv: &[u16],
    freq: &[i32],
    alpha: usize,
    bw: &mut BitWriter<'_>,
) {
    let nmtf = mtfv.len();
    let n_groups = n_groups(nmtf);
    seed_tables(&mut h.len, n_groups, nmtf, freq, alpha);
    let n_sel = nmtf.div_ceil(G_SIZE);
    h.selector.resize(n_sel, 0);
    h.prev_sel.resize(n_sel, 0);

    for iter in 0..N_ITERS {
        let recount = iter == 0;
        if recount {
            for copy in &mut h.rfreq {
                for row in &mut copy[..n_groups] {
                    row[..alpha].fill(0);
                }
            }
        }
        // A table whose counts no group moved into or out of keeps its
        // lengths, so only the others are rebuilt.
        let mut dirty = [recount; N_GROUPS];
        h.pk[..alpha].fill(0);
        for (t, row) in h.len[..n_groups].iter().enumerate() {
            for (p, &l) in h.pk[..alpha].iter_mut().zip(row) {
                *p |= u64::from(l) << (10 * t);
            }
        }
        for (grp, g) in mtfv.chunks(G_SIZE).enumerate() {
            let (mut s0, mut s1) = (0u64, 0u64);
            let (pairs, last) = g.as_chunks::<2>();
            for p in pairs {
                s0 += h.pk[usize::from(p[0])];
                s1 += h.pk[usize::from(p[1])];
            }
            if let Some(&x) = last.first() {
                s0 += h.pk[usize::from(x)];
            }
            let s = s0 + s1;
            let (mut bc, mut bt) = ((s & 1023) as u32, 0);
            for t in 1..n_groups {
                let c = ((s >> (10 * t)) & 1023) as u32;
                if c < bc {
                    bc = c;
                    bt = t;
                }
            }
            h.selector[grp] = bt as u8;
            if recount {
                // Two interleaved copies break the store-to-load chain on
                // runs of one MTF value.
                let [ra, rb] = &mut h.rfreq;
                for p in pairs {
                    ra[bt][usize::from(p[0])] += 1;
                    rb[bt][usize::from(p[1])] += 1;
                }
                if let Some(&x) = last.first() {
                    ra[bt][usize::from(x)] += 1;
                }
            } else {
                let from = usize::from(h.prev_sel[grp]);
                if bt != from {
                    dirty[from] = true;
                    dirty[bt] = true;
                    for &x in g {
                        h.rfreq[0][from][usize::from(x)] -= 1;
                        h.rfreq[0][bt][usize::from(x)] += 1;
                    }
                }
            }
        }
        let mut ts = [0usize; N_GROUPS];
        let mut n_ts = 0;
        for t in 0..n_groups {
            if recount {
                let [ra, rb] = &mut h.rfreq;
                for (a, &b) in ra[t][..alpha].iter_mut().zip(&rb[t][..alpha]) {
                    *a += b;
                }
            }
            if dirty[t] {
                ts[n_ts] = t;
                n_ts += 1;
            }
        }
        make_code_lengths_all(
            &mut h.lengths,
            &mut h.len,
            &h.rfreq[0],
            &ts[..n_ts],
            alpha,
            MAX_CODE_LEN,
        );
        h.prev_sel.copy_from_slice(&h.selector);
    }

    for t in 0..n_groups {
        assign_codes(&mut h.ce[t][..alpha], &h.len[t][..alpha]);
    }

    bw.put(3, n_groups as u64);
    bw.put(15, n_sel as u64);
    let mut pos = [0, 1, 2, 3, 4, 5];
    for &sel in &h.selector {
        // j ones then a zero
        let j = selector_mtf(sel, &mut pos);
        bw.put(j + 1, u64::from(((1u32 << j) - 1) << 1));
    }
    for t in 0..n_groups {
        put_lengths(bw, &h.len[t][..alpha]);
    }

    // The coded MTF values: three per flush, which the at most 7 bits a
    // flush leaves pending allow (7 + 3 * 17 <= 63), with no check per code;
    // the caller reserved their bound.
    bw.flush();
    let (mut acc, mut nbits, out, mut pos) = bw.parts();
    let mut flush = |acc: u64, nbits: &mut u32| {
        super::store(out, pos, acc << (64 - *nbits));
        pos += (*nbits / 8) as usize;
        *nbits &= 7;
    };
    for (g, &sel) in mtfv.chunks(G_SIZE).zip(&h.selector) {
        let tab = &h.ce[usize::from(sel)];
        let (triples, rest) = g.as_chunks::<3>();
        for t3 in triples {
            for &x in t3 {
                let e = tab[usize::from(x)];
                acc = (acc << (e & 31)) | u64::from(e >> 5);
                nbits += e & 31;
            }
            flush(acc, &mut nbits);
        }
        for &x in rest {
            let e = tab[usize::from(x)];
            acc = (acc << (e & 31)) | u64::from(e >> 5);
            nbits += e & 31;
            flush(acc, &mut nbits);
        }
    }
    bw.set_parts(acc, nbits, pos);
}
