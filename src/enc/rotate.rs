//! The block's least rotation into the block sorter (bz2-ng's `enc_sort.c`
//! and the least-rotation kernel of `kern_template.h`, vector tier).
//!
//! The least rotation T' of a primitive block is a Lyndon word, whose suffix
//! order equals its rotation order, so the suffix sorter on T' yields
//! bzip2's rotation order. For T = u^m (m > 1), T' = u'^m with u' Lyndon;
//! its rotation order is the suffix order of u' with each row repeated m
//! times, and any of the m tied rows is a valid origPtr (here: the last, as
//! bz2-ng writes). The sorter writes the BWT over the rotated block in place
//! and returns the row of the rotation that is the original block, which is
//! origPtr.

use fearless_simd::{Select, Simd, SimdBase, u8x16};

use super::{load16, sort};

/// Candidate-list refinement stops below `n / SPARSE_DIV` candidates, after
/// `KMAX` bytes, or when it stalls.
const SPARSE_DIV: usize = 1024;
const KMAX: usize = 64;

/// Sorts the block `tt[..n]`: writes its BWT to `tt[r..r + n]` and its byte
/// frequencies to `freq`, and returns `(origPtr, r)`. `tt` holds at least
/// `2 * n` bytes; `work` is the sorter's scratch, which also holds the
/// least rotation's candidates.
#[inline(always)]
pub(crate) fn sort_block<S: Simd>(
    simd: S,
    tt: &mut [u8],
    n: usize,
    freq: &mut [i32; 256],
    work: &mut Vec<i32>,
) -> (usize, usize) {
    tt.copy_within(..n, n);
    if work.len() < sort::work_len(n) {
        work.resize(sort::work_len(n), 0);
    }
    let (r, periodic) = least_rotation(simd, tt, n, work);
    // A least rotation is a power of a Lyndon word, so the root is found.
    let p = if periodic {
        lyndon_root_len(&tt[r..r + n]).unwrap_or(n)
    } else {
        n
    };
    debug_assert!(p < n || !periodic, "periodic block without a root");
    let m = n / p;
    let q = (n - r) % n;
    let rank = sort::bwt(&mut tt[r..r + p], q % p, freq, work);
    if m > 1 {
        let l = &mut tt[r..r + n];
        for i in (0..p).rev() {
            let b = l[i];
            l[i * m..(i + 1) * m].fill(b);
        }
        for f in freq.iter_mut() {
            *f *= m as i32;
        }
    }
    (rank * m + m - 1, r)
}

/// Length of the primitive root of a least rotation `tr` (one Duval scan);
/// `None` if `tr` is not a power of a Lyndon word.
fn lyndon_root_len(tr: &[u8]) -> Option<usize> {
    let n = tr.len();
    let mut k = 0;
    for j in 1..n {
        if tr[k] < tr[j] {
            k = 0;
        } else if tr[k] == tr[j] {
            k += 1;
        } else {
            return None;
        }
    }
    let p = n - k;
    n.is_multiple_of(p).then_some(p)
}

/// The start of a least rotation of `tt[..n]` (whose copy is in
/// `tt[n..2n]`), and whether the block is exactly periodic. Two-pointer
/// minimum-rotation search over candidate start positions, with periodicity
/// read off its exit: a minimal rotation is never skipped, so for T = u^m
/// the two pointers end on two equal rotations and the prefix comparison
/// reaches n; for a primitive T it never does. The candidates are the
/// positions of the smallest 2-byte window, found with vector passes, then
/// refined byte by byte while that keeps shrinking the set.
#[inline(always)]
pub(super) fn least_rotation<S: Simd>(
    simd: S,
    tt: &[u8],
    n: usize,
    cand: &mut [i32],
) -> (usize, bool) {
    if n <= 1 {
        return (0, false);
    }
    let (chunks, end) = (n / 16, n / 16 * 16);

    let mut least = u8x16::splat(simd, 255);
    for k in 0..chunks {
        least = least.min(load16(simd, tt, 16 * k));
    }
    let c0 = tt[end..n]
        .iter()
        .copied()
        .fold(simd.reduce_min_u8x16(least), u8::min);

    // The smallest byte after a c0 (tt[n] = tt[0] continues the rotation).
    let (v0, none) = (u8x16::splat(simd, c0), u8x16::splat(simd, 255));
    let mut least = none;
    for k in 0..chunks {
        let hit = load16(simd, tt, 16 * k).simd_eq(v0);
        least = least.min(hit.select(load16(simd, tt, 16 * k + 1), none));
    }
    let c1 = (end..n)
        .filter(|&x| tt[x] == c0)
        .map(|x| tt[x + 1])
        .fold(simd.reduce_min_u8x16(least), u8::min);

    // Their positions, and the smallest byte after them.
    let v1 = u8x16::splat(simd, c1);
    let (mut m, mut cm) = (0, 255u8);
    let mut take = |x: usize, m: &mut usize| {
        cand[*m] = x as i32;
        *m += 1;
        cm = cm.min(tt[x + 2]);
    };
    for k in 0..chunks {
        let x = 16 * k;
        let hit = load16(simd, tt, x).simd_eq(v0) & load16(simd, tt, x + 1).simd_eq(v1);
        let mut bits = simd.to_bitmask_mask8x16(hit);
        while bits != 0 {
            take(x + bits.trailing_zeros() as usize, &mut m);
            bits &= bits - 1;
        }
    }
    for x in end..n {
        if tt[x] == c0 && tt[x + 1] == c1 {
            take(x, &mut m);
        }
    }
    refine(tt, n, cand, m, 2, cm)
}

/// Length of the common prefix of the rotations at `a` and `b` (both below
/// `n`), starting the comparison at `k` and capped at `n`.
#[inline(always)]
fn common_prefix(tt: &[u8], a: usize, b: usize, mut k: usize, n: usize) -> usize {
    while k + 8 <= n {
        let x = load(tt, a + k) ^ load(tt, b + k);
        if x != 0 {
            return k + (x.trailing_zeros() >> 3) as usize;
        }
        k += 8;
    }
    while k < n && tt[a + k] == tt[b + k] {
        k += 1;
    }
    k
}

#[inline(always)]
fn load(tt: &[u8], i: usize) -> u64 {
    let mut w = [0; 8];
    w.copy_from_slice(&tt[i..i + 8]);
    u64::from_le_bytes(w)
}

/// Refines a sorted candidate list `cand[..m]` whose members all share their
/// first `k` bytes, and whose next byte has minimum `cm`, then runs the
/// two-pointer search over the list (list indices only move forward).
/// Refinement stops before `k` reaches `n`, which keeps its reads of
/// `tt[pos + k + 1]` inside the copy, where they continue the rotation.
fn refine(
    tt: &[u8],
    n: usize,
    cand: &mut [i32],
    mut m: usize,
    mut k: usize,
    mut cm: u8,
) -> (usize, bool) {
    let mut stalled = 0;
    let mut budget = 2 * n as i64 - m as i64;
    while m > n / SPARSE_DIV && k < KMAX.min(n) && budget > 0 {
        let (mut w, mut next) = (0, 255u8);
        for i in 0..m {
            let pos = cand[i] as usize;
            let hit = tt[pos + k] == cm;
            cand[w] = pos as i32;
            w += usize::from(hit);
            let f = if hit { tt[pos + k + 1] } else { 255 };
            next = next.min(f);
        }
        budget -= m as i64;
        stalled = if w < m - m / 16 { 0 } else { stalled + 1 };
        m = w;
        cm = next;
        k += 1;
        if stalled >= 2 {
            break;
        }
    }
    if m == 1 {
        return (cand[0] as usize, false);
    }
    let mut periodic = false;
    let (mut a, mut b) = (0, 1);
    while a < m && b < m {
        let (i, j) = (cand[a] as usize, cand[b] as usize);
        let kk = common_prefix(tt, i, j, k, n);
        if kk >= n {
            periodic = true;
            break;
        }
        if tt[i + kk] > tt[j + kk] {
            let lim = (i + kk + 1) as i32;
            while a < m && cand[a] < lim {
                a += 1;
            }
        } else {
            let lim = (j + kk + 1) as i32;
            while b < m && cand[b] < lim {
                b += 1;
            }
        }
        if a == b {
            b += 1;
        }
    }
    let pi = if a < m { cand[a] as usize } else { n };
    let pj = if b < m { cand[b] as usize } else { n };
    (pi.min(pj), periodic)
}
