//! The block sorter's top level: SA-IS stages 1 and 2 on the block's bytes,
//! and the placement of the sorted LMS suffixes for the final induction.
//!
//! Ported from libsais, Copyright (c) 2021-2025 Ilya Grebnov, under the
//! Apache License 2.0 with LLVM Exceptions (`LICENSE-libsais`), from the copy
//! at commit 1a696e6 that bz2-ng vendors: the single-threaded path of
//! `libsais_main_8u` up to the final induction:
//! `count_and_gather_lms_suffixes_8u`, the LMS radix sort, the partial-order
//! scans with their 2-symbol buckets and sign-bit name marks, renumbering and
//! gathering, and `place_lms_suffixes_interval_8u`. Altered: written in Rust,
//! without the OpenMP paths and the GSA flag, with fewer prefetches, with the bucket
//! arrays in fixed-size arrays indexed by bytes (so no bounds check survives
//! on them), and with this crate's own recursion and final BWT induction
//! (`sort.rs`) on either side.

/// The bucket arrays of the top level, as libsais lays them out in one
/// array of 8 × 256 ints.
pub(super) struct Buckets {
    /// Counts by symbol and the types of a suffix and its predecessor
    /// (`4c + 2t(p) + t(p-1)`, L = 1); later the right-to-left induction
    /// heads by `2c + t` in `[..512]` and the distinct-name counters in
    /// `[512..]`.
    b4: [i32; 1024],
    /// The left-to-right induction heads (and before them the radix sort's),
    /// by `2c + t`.
    temp: [i32; 512],
    pub(super) start: [i32; 256],
    pub(super) end: [i32; 256],
}

impl Buckets {
    pub(super) fn new() -> Buckets {
        Buckets {
            b4: [0; 1024],
            temp: [0; 512],
            start: [0; 256],
            end: [0; 256],
        }
    }
}

use super::sort::{PF, prefetch};

const MIN: i32 = i32::MIN;
const MAX: i32 = i32::MAX;

#[inline(always)]
fn idx2(c: u8, t: bool) -> usize {
    2 * usize::from(c) + usize::from(t)
}

#[inline(always)]
fn idx4(c: u8, t: usize) -> usize {
    4 * usize::from(c) + (t & 3)
}

/// The type step of libsais: whether `c0`, followed by `c1` of type `f1`,
/// is L-type (`c0 > c1`, or equal and `c1` L-type).
#[inline(always)]
fn l_type(c0: u8, c1: u8, f1: bool) -> bool {
    i32::from(c0) > i32::from(c1) - i32::from(f1)
}

/// Counts the suffixes by symbol and type pair into `bk.b4`, and gathers the
/// LMS positions, ascending, into `sa[n - m..n]`; returns `m`.
pub(super) fn count_and_gather(t: &[u8], sa: &mut [i32], bk: &mut Buckets) -> usize {
    bk.b4.fill(0);
    let n = t.len();
    let mut m = n - 1;
    // The last suffix is L-type: the implicit sentinel after it is smaller.
    let (mut c0, mut f0) = (t[n - 1], true);
    for i in (0..n - 1).rev() {
        let c1 = t[i];
        let f1 = l_type(c1, c0, f0);
        sa[m] = (i + 1) as i32;
        m -= usize::from(f1 && !f0);
        bk.b4[idx4(c0, 2 * usize::from(f0) + usize::from(f1))] += 1;
        (c0, f0) = (c1, f1);
    }
    // Suffix 0 has no predecessor; libsais counts it with an S-type one.
    sa[m] = 0;
    bk.b4[idx4(c0, 2 * usize::from(f0))] += 1;
    n - 1 - m
}

/// Bucket starts and ends from the counts, and the byte histogram.
pub(super) fn start_and_end(bk: &mut Buckets, freq: &mut [i32; 256]) {
    let mut sum = 0;
    for (c, counts) in bk.b4.as_chunks::<4>().0.iter().enumerate() {
        let total: i32 = counts.iter().sum();
        bk.start[c] = sum;
        sum += total;
        bk.end[c] = sum;
        freq[c] = total;
    }
}

/// Drops the suffixes before the first LMS suffix from the counts and sets
/// the radix sort's bucket heads; returns the number of L-type suffixes
/// from the first LMS suffix on.
pub(super) fn buckets_for_radix_sort(t: &[u8], bk: &mut Buckets, first_lms: usize) -> i32 {
    let (mut c0, mut f0) = (t[first_lms], false);
    for p in (0..first_lms).rev() {
        let (c1, f1) = (c0, f0);
        c0 = t[p];
        f0 = l_type(c0, c1, f1);
        bk.b4[idx4(c1, 2 * usize::from(f1) + usize::from(f0))] -= 1;
    }
    bk.b4[idx4(c0, 2 * usize::from(f0))] -= 1;
    let mut sum = 0;
    for c in 0..=255u8 {
        bk.temp[idx2(c, true)] = sum;
        sum += bk.b4[idx4(c, 1)] + bk.b4[idx4(c, 3)];
        bk.temp[idx2(c, false)] = sum;
    }
    sum
}

/// Moves the LMS suffixes `sa[from..to]` to the ends of their buckets
/// (in the radix sort's bucket heads), right to left.
pub(super) fn radix_sort_lms<const P: bool>(
    t: &[u8],
    sa: &mut [i32],
    bk: &mut Buckets,
    from: usize,
    to: usize,
) {
    for i in (from..to).rev() {
        if P && i >= PF {
            prefetch(t, sa[i - PF] as usize);
        }
        let p = sa[i];
        let h = &mut bk.temp[idx2(t[p as usize], false)];
        *h -= 1;
        sa[*h as usize] = p;
    }
}

/// The induction heads of the partial sorts.
pub(super) fn buckets_for_partial_sorting(
    t: &[u8],
    bk: &mut Buckets,
    first_lms: usize,
    left_count: i32,
) {
    bk.b4[idx4(t[first_lms], 1)] += 1;
    let (mut sum0, mut sum1) = (left_count + 1, 0);
    for c in 0..=255u8 {
        bk.temp[idx2(c, false)] = sum0;
        sum0 += bk.b4[idx4(c, 0)] + bk.b4[idx4(c, 2)];
        sum1 += bk.b4[idx4(c, 1)];
        // These overwrite counts of symbols at most c / 2, already read.
        bk.b4[idx2(c, false)] = sum0;
        bk.b4[idx2(c, true)] = sum1;
    }
}

/// Sorts the LMS substrings: the left-to-right scan, the move of the group
/// marks, and the right-to-left scan. Each induced suffix carries the sign
/// bit when it starts a new name group. Returns whether the left-to-right
/// scan changed bucket on fewer than one step in ten, which picks the
/// bucket-cursor mode of the steps after it.
///
/// Every suffix `p` the scans meet has `p >= 2`, so `t[p - 2]` is in bounds:
/// the radix sort leaves the first LMS suffix out, so nothing before it is
/// induced, and in a Lyndon word that suffix starts at 2 or later (suffix 0
/// is the least, so S-type, and neither 0 nor 1 is LMS).
pub(super) fn induce_partial_order<const P: bool>(
    t: &[u8],
    sa: &mut [i32],
    bk: &mut Buckets,
    first_lms: usize,
    left_count: i32,
) -> bool {
    let n = t.len();
    bk.b4[512..].fill(0);
    let temp = &mut bk.temp;
    let (ind, distinct) = bk.b4.split_at_mut(512);

    // Left to right over the L-type part.
    let mut d = 0;
    let v = idx2(t[n - 1], t[n - 2] >= t[n - 1]);
    sa[temp[v] as usize] = (n - 1) as i32 | MIN;
    temp[v] += 1;
    d += 1;
    distinct[v] = d;
    let (mut changes, mut prev) = (0u32, usize::MAX);
    for i in 0..left_count as usize {
        if P && let Some(&f) = sa.get(i + PF) {
            prefetch(t, ((f & MAX) as usize).wrapping_sub(2));
        }
        let mut p = sa[i];
        d += i32::from(p < 0);
        p &= MAX;
        let p = p as usize;
        let v = idx2(t[p - 1], t[p - 2] >= t[p - 1]);
        changes += u32::from(v != prev);
        prev = v;
        let h = &mut temp[v];
        sa[*h as usize] = (p - 1) as i32 | (i32::from(distinct[v] != d) << 31);
        *h += 1;
        distinct[v] = d;
    }
    let repetitive = 10 * i64::from(changes) < i64::from(left_count);

    // The marks move from the start of each group of an L-type bucket to
    // its end.
    for c in (1..=255u8).rev() {
        let mut s = MIN;
        let lo = ind[idx2(c - 1, false)] as usize;
        let hi = temp[idx2(c, false)] as usize;
        for x in sa[lo..hi].iter_mut().rev() {
            let p = *x;
            let q = (p & MIN) ^ s;
            s ^= q;
            *x = p ^ q;
        }
    }

    // Right to left over the S-type part.
    let (from, to) = (left_count as usize + 1, n - first_lms);
    for i in (from..to).rev() {
        if P && i >= PF {
            prefetch(t, ((sa[i - PF] & MAX) as usize).wrapping_sub(2));
        }
        let mut p = sa[i];
        d += i32::from(p < 0);
        p &= MAX;
        let p = p as usize;
        let v = idx2(t[p - 1], t[p - 2] > t[p - 1]);
        let h = &mut ind[v];
        *h -= 1;
        sa[*h as usize] = (p - 1) as i32 | (i32::from(distinct[v] != d) << 31);
        distinct[v] = d;
    }
    repetitive
}

/// Names the sorted LMS substrings (`sa[..m]`, a sign bit where a group
/// ends) into `sa[m + p / 2]`; when the names are not unique, gathers them in
/// text order into `sa[n + fs - m..n + fs]` (the reduced string) and returns
/// their number, else clears the marks and returns `m`.
pub(super) fn renumber_and_gather(sa: &mut [i32], n: usize, m: usize, fs: usize) -> usize {
    sa[m..m + n / 2].fill(0);
    let mut name = 0;
    for i in 0..m {
        let p = sa[i];
        sa[m + ((p & MAX) >> 1) as usize] = name | MIN;
        name += i32::from(p < 0);
    }
    let name = name as usize;
    if name < m {
        let mut l = n + fs - 1;
        for i in (m..m + n / 2).rev() {
            let s = sa[i];
            sa[l] = s & MAX;
            l -= usize::from(s < 0);
        }
    } else {
        for v in &mut sa[..m] {
            *v &= MAX;
        }
    }
    name
}

/// Places the sorted LMS suffixes `sa[..m]` at the ends of their buckets,
/// zeros elsewhere: the seeds of the final induction.
pub(super) fn place_lms(sa: &mut [i32], n: usize, mut m: usize, bk: &Buckets) {
    let mut j = n;
    // A byte-255 suffix is never S-type, so never LMS.
    for c in (0..=254u8).rev() {
        let l = bk.b4[idx2(c + 1, true)] - bk.b4[idx2(c, true)];
        if l > 0 {
            let l = l as usize;
            let i = bk.end[usize::from(c)] as usize;
            if j > i {
                sa[i..j].fill(0);
            }
            m -= l;
            j = i - l;
            sa.copy_within(m..m + l, j);
        }
    }
    sa[..j].fill(0);
}
