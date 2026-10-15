//! The block's least rotation into the block sorter (bz2-ng's `enc_sort.c`
//! and the least-rotation kernel of `kern_template.h`, scalar tier).
//!
//! The least rotation T' of a primitive block is a Lyndon word, whose suffix
//! order equals its rotation order, so the suffix sorter on T' yields
//! bzip2's rotation order. For T = u^m (m > 1), T' = u'^m with u' Lyndon;
//! its rotation order is the suffix order of u' with each row repeated m
//! times, and any of the m tied rows is a valid origPtr (here: the last, as
//! bz2-ng writes). The sorter writes the BWT over the rotated block in place
//! and returns the row of the rotation that is the original block, which is
//! origPtr.

use super::sort;

/// Bytes of zeros after the block's copy, so that the 8-byte prefix
/// comparisons may read past it.
pub(crate) const PAD: usize = 64;

/// Candidate-list refinement stops below `n / SPARSE_DIV` candidates, after
/// `KMAX` bytes, or when it stalls; a smallest byte rarer than one in
/// `DENSE_DIV` is searched for directly.
const DENSE_DIV: usize = 32;
const SPARSE_DIV: usize = 1024;
const KMAX: usize = 64;

/// Sorts the block `tt[..n]`: writes its BWT to `tt[r..r + n]` and its byte
/// frequencies to `freq`, and returns `(origPtr, r)`. `tt` holds at least
/// `2 * n + PAD` bytes; `work` is the sorter's scratch, which also holds the
/// least rotation's candidates.
pub(crate) fn sort_block(
    tt: &mut [u8],
    n: usize,
    freq: &mut [i32; 256],
    work: &mut Vec<i32>,
) -> (usize, usize) {
    tt.copy_within(..n, n);
    tt[2 * n..2 * n + PAD].fill(0);
    if work.len() < sort::work_len(n) {
        work.resize(sort::work_len(n), 0);
    }
    let (r, periodic) = least_rotation(tt, n, work);
    // A least rotation of a periodic block is a power of a Lyndon word, so
    // the root is found; the fallback only keeps a broken invariant from
    // reaching an index out of bounds.
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
/// positions of the smallest byte (found directly when sparse, refined byte
/// by byte while that keeps shrinking the set when dense).
pub(super) fn least_rotation(tt: &[u8], n: usize, cand: &mut [i32]) -> (usize, bool) {
    if n <= 1 {
        return (0, false);
    }
    let c0 = tt[..n].iter().copied().min().unwrap_or(0);
    let count = tt[..n].iter().filter(|&&b| b == c0).count();
    if count <= n / DENSE_DIV {
        return sparse(tt, n, c0);
    }
    let (mut m, mut cm) = (0, 255u8);
    for x in 0..n {
        cand[m] = x as i32;
        let hit = tt[x] == c0;
        m += usize::from(hit);
        let f = if hit { tt[x + 1] } else { 255 };
        cm = cm.min(f);
    }
    refine(tt, n, cand, m, 1, cm)
}

/// Length of the common prefix of `tt[a..]` and `tt[b..]`, starting the
/// comparison at `k` and capped at `n` (bytes beyond `tt[2n]` are padding).
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

/// First position `>= pos` holding `c0`, or `n`.
fn next_cand(tt: &[u8], pos: usize, n: usize, c0: u8) -> usize {
    if pos >= n {
        return n;
    }
    tt[pos..n]
        .iter()
        .position(|&b| b == c0)
        .map_or(n, |i| pos + i)
}

/// Two-pointer search over all positions holding `c0`.
fn sparse(tt: &[u8], n: usize, c0: u8) -> (usize, bool) {
    let mut periodic = false;
    let mut i = next_cand(tt, 0, n, c0);
    let mut j = next_cand(tt, i + 1, n, c0);
    while i < n && j < n {
        let k = common_prefix(tt, i, j, 1, n);
        if k >= n {
            periodic = true;
            break;
        }
        if tt[i + k] > tt[j + k] {
            i = next_cand(tt, i + k + 1, n, c0);
        } else {
            j = next_cand(tt, j + k + 1, n, c0);
        }
        if i == j {
            j = next_cand(tt, j + 1, n, c0);
        }
    }
    (i.min(j), periodic)
}

/// Refines a sorted candidate list `cand[..m]` whose members all share their
/// first `k` bytes, and whose next byte has minimum `cm`, then runs the
/// two-pointer search over the list (list indices only move forward).
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
    while m > n / SPARSE_DIV && k < KMAX && budget > 0 {
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
