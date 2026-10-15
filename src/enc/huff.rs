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
        })
    }
}

/// libbz2's `BZ2_hbMakeCodeLengths`: code lengths `len[..alpha]` for the
/// frequencies `freq`, limited to `max_len` bits by flattening the weights
/// and rebuilding until they fit.
#[allow(
    clippy::needless_range_loop,
    reason = "libbz2's loops, kept recognisable"
)]
pub(super) fn make_code_lengths(len: &mut [u8], freq: &[i32], alpha: usize, max_len: u8) {
    let mut heap = [0usize; MAX_ALPHA + 2];
    let mut weight = [0i32; MAX_ALPHA * 2];
    let mut parent = [0i32; MAX_ALPHA * 2];

    for i in 0..alpha {
        weight[i + 1] = freq[i].max(1) << 8;
    }

    loop {
        let mut n_nodes = alpha;
        let mut n_heap = 0;
        heap[0] = 0;
        weight[0] = 0;
        parent[0] = -2;

        let upheap = |heap: &mut [usize], weight: &[i32], z: usize| {
            let mut zz = z;
            let tmp = heap[zz];
            while weight[tmp] < weight[heap[zz >> 1]] {
                heap[zz] = heap[zz >> 1];
                zz >>= 1;
            }
            heap[zz] = tmp;
        };
        let downheap = |heap: &mut [usize], weight: &[i32], n_heap: usize, z: usize| {
            let mut zz = z;
            let tmp = heap[zz];
            loop {
                let mut yy = zz << 1;
                if yy > n_heap {
                    break;
                }
                if yy < n_heap && weight[heap[yy + 1]] < weight[heap[yy]] {
                    yy += 1;
                }
                if weight[tmp] < weight[heap[yy]] {
                    break;
                }
                heap[zz] = heap[yy];
                zz = yy;
            }
            heap[zz] = tmp;
        };

        for i in 1..=alpha {
            parent[i] = -1;
            n_heap += 1;
            heap[n_heap] = i;
            upheap(&mut heap, &weight, n_heap);
        }
        while n_heap > 1 {
            let n1 = heap[1];
            heap[1] = heap[n_heap];
            n_heap -= 1;
            downheap(&mut heap, &weight, n_heap, 1);
            let n2 = heap[1];
            heap[1] = heap[n_heap];
            n_heap -= 1;
            downheap(&mut heap, &weight, n_heap, 1);
            n_nodes += 1;
            parent[n1] = n_nodes as i32;
            parent[n2] = n_nodes as i32;
            let (w1, w2) = (weight[n1], weight[n2]);
            weight[n_nodes] = ((w1 & !0xff) + (w2 & !0xff)) | (1 + (w1 & 0xff).max(w2 & 0xff));
            parent[n_nodes] = -1;
            n_heap += 1;
            heap[n_heap] = n_nodes;
            upheap(&mut heap, &weight, n_heap);
        }

        // A node's parent is made after it, so one pass down from the root
        // gives every depth (libbz2 walks from each leaf to the root).
        let mut depth = [0u8; MAX_ALPHA * 2];
        for k in (1..n_nodes).rev() {
            depth[k] = depth[parent[k] as usize] + 1;
        }
        let mut too_long = false;
        for (l, &d) in len[..alpha].iter_mut().zip(&depth[1..=alpha]) {
            *l = d;
            too_long |= d > max_len;
        }
        if !too_long {
            break;
        }
        for w in &mut weight[1..=alpha] {
            *w = (1 + (*w >> 8) / 2) << 8;
        }
    }
}

/// libbz2's `BZ2_hbAssignCodes`: canonical codes from the lengths.
fn assign_codes(code: &mut [u32], length: &[u8], min_len: u8, max_len: u8) {
    let mut vec = 0;
    for n in min_len..=max_len {
        for (c, &l) in code.iter_mut().zip(length) {
            if l == n {
                *c = vec;
                vec += 1;
            }
        }
        vec <<= 1;
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
            h.rfreq = [[[0; MAX_ALPHA]; N_GROUPS]; 2];
        }
        // A table whose counts no group moved into or out of keeps its
        // lengths, so only the others are rebuilt.
        let mut dirty = [recount; N_GROUPS];
        for v in 0..alpha {
            let mut w = 0u64;
            for t in 0..n_groups {
                w |= u64::from(h.len[t][v]) << (10 * t);
            }
            h.pk[v] = w;
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
        for t in 0..n_groups {
            if recount {
                let [ra, rb] = &mut h.rfreq;
                for (a, &b) in ra[t][..alpha].iter_mut().zip(&rb[t][..alpha]) {
                    *a += b;
                }
            }
            if dirty[t] {
                make_code_lengths(&mut h.len[t], &h.rfreq[0][t], alpha, MAX_CODE_LEN);
            }
        }
        h.prev_sel.copy_from_slice(&h.selector);
    }

    let mut code = [0u32; MAX_ALPHA];
    for t in 0..n_groups {
        let lens = &h.len[t][..alpha];
        let min_len = lens.iter().copied().min().unwrap_or(0);
        let max_len = lens.iter().copied().max().unwrap_or(0);
        assign_codes(&mut code[..alpha], lens, min_len, max_len);
        for i in 0..alpha {
            h.ce[t][i] = (code[i] << 5) | u32::from(lens[i]);
        }
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
        let mut curr = h.len[t][0];
        bw.put(5, u64::from(curr));
        for &target in &h.len[t][..alpha] {
            while curr < target {
                bw.put(2, 2);
                curr += 1;
            }
            while curr > target {
                bw.put(2, 3);
                curr -= 1;
            }
            bw.put(1, 0);
        }
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
