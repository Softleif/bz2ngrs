//! The block sorter's recursion on a reduced string of names: libsais's
//! `libsais_main_32s_recursion`, its first (6k) form, which every level of
//! CRAM-like data takes. Levels whose free space is short of six ints per
//! symbol go to the older recursion in `sort.rs`, and so does everything
//! below a repetitive top level (`REG`), where that recursion's register
//! cursor measured faster on Zen 2.
//!
//! Ported from libsais, Copyright (c) 2021-2025 Ilya Grebnov, under the
//! Apache License 2.0 with LLVM Exceptions (`LICENSE-libsais`), from the copy
//! at commit 1a696e6 that bz2-ng vendors: the single-threaded path of the 6k
//! branch of `libsais_main_32s_recursion` and what it calls (counting and
//! gathering, the LMS radix sort and markers, the 6k partial-order scans,
//! renumbering, the compaction of unique names and its reconstruction, and the
//! final induction). Altered: written in Rust, without the OpenMP paths, the
//! local buffer, the bucket alignment and the prefetches, with the 4k, 2k and
//! 1k branches replaced by `sort.rs`'s recursion.
//!
//! The work array `sa` has `n + fs` ints. A level keeps its 6k bucket ints at
//! the end of it; renumbering and compaction reuse that space once the
//! partial sort is done, and the buckets are counted again afterwards.

use super::sais_top::renumber_and_gather;

const MIN: i32 = i32::MIN;
const MAX: i32 = i32::MAX;

#[inline(always)]
fn i2(c: i32, s: usize) -> usize {
    ((c as usize) << 1) + s
}

#[inline(always)]
fn i4(c: i32, s: usize) -> usize {
    ((c as usize) << 2) + s
}

/// Whether `c0`, followed by `c1` of type `f1` (1 for L), is L-type.
#[inline(always)]
fn l_type(c0: i32, c1: i32, f1: usize) -> usize {
    usize::from(c0 > c1 - f1 as i32)
}

/// The suffix array of `t` (symbols below `k`) into `sa[..t.len()]`; `sa`
/// has `t.len() + fs` ints. `t` is used as scratch and restored. `REG` and
/// `P` pick the form of `sort.rs`'s recursion where that one runs.
pub(super) fn sort<const REG: bool, const P: bool>(
    t: &mut [i32],
    sa: &mut [i32],
    fs: usize,
    k: usize,
) {
    if REG || k == 0 || fs / k < 6 {
        super::sort::sort_main::<REG, P>(t, sa, fs, k);
        return;
    }
    let n = t.len();
    let bk_at = n + fs - 6 * k;
    let m = {
        let (s, bk) = sa.split_at_mut(bk_at);
        count_and_gather_4k(t, &mut s[..n], &mut bk[..4 * k])
    };
    if m <= 1 {
        let (s, bk) = sa.split_at_mut(bk_at);
        let s = &mut s[..n];
        s[0] = s[n - 1];
        start_and_end_6k(k, bk);
        place_lms_histogram(s, k, m, bk, 4, 5 * k);
        let (start, end) = bk[4 * k..6 * k].split_at_mut(k);
        induce_final_order(t, s, start, end);
        return;
    }
    // libsais renumbers with group markers and compacts away the unique
    // names only when the alphabet is not tiny for the string.
    let big = n / 8192 < k;
    {
        let (s, bk) = sa.split_at_mut(bk_at);
        let s = &mut s[..n];
        s[..n - m].fill(0);
        let first_lms = s[n - m] as usize;
        let left = buckets_for_radix_sort_6k(t, k, bk, first_lms);
        radix_sort_lms_6k(t, s, &mut bk[4 * k..5 * k], n - m + 1, n);
        if big {
            for &b in &bk[4 * k..5 * k - 1] {
                s[b as usize] |= MIN;
            }
        }
        buckets_for_partial_sorting_6k(t, k, bk, first_lms, left);
        induce_partial_order_6k(t, s, k, bk, first_lms, left);
    }
    let names = if big {
        renumber_and_mark_distinct_4k(sa, n, m)
    } else {
        renumber_and_gather(sa, n, m, fs)
    };
    if names < m {
        let f = if big { compact(t, sa, n, m, fs) } else { 0 };
        {
            // The level below has [0, n + fs - m) as its work array and the
            // names of the non-unique substrings after the f unique ones.
            let (rec, rest) = sa.split_at_mut(n + fs - m);
            sort::<REG, P>(&mut rest[f..], rec, fs + n - 2 * m + f, names - f);
        }
        if f > 0 {
            sa.copy_within(n + fs - m..n + fs - m + f, n - m - 1);
            {
                let (s, bk) = sa.split_at_mut(bk_at);
                count_and_gather_compacted_2k(t, &mut s[..n], &mut bk[..2 * k]);
            }
            reconstruct_lms(&mut sa[..n], m - f);
            sa.copy_within(..m - f, n - m - 1 + f);
            sa[..m].fill(0);
            merge_unique(t, &mut sa[..n], m);
            merge_nonunique(&mut sa[..n], m, f);
        } else {
            {
                let (s, bk) = sa.split_at_mut(bk_at);
                count_and_gather_2k(t, &mut s[..n], &mut bk[..2 * k]);
            }
            reconstruct_lms(&mut sa[..n], m);
        }
    } else {
        count_lms_2k(t, &mut sa[bk_at..bk_at + 2 * k]);
    }
    let (s, bk) = sa.split_at_mut(bk_at);
    let s = &mut s[..n];
    start_and_end_4k(k, bk);
    place_lms_histogram(s, k, m, bk, 2, 3 * k);
    let (start, end) = bk[2 * k..4 * k].split_at_mut(k);
    induce_final_order(t, s, start, end);
}

/// Counts the suffixes by symbol and the types of the suffix and its
/// predecessor (`4c + 2t(p) + t(p-1)`, L = 1) into `bk`, and gathers the LMS
/// positions, ascending, into `sa[n - m..n]`; returns `m`.
fn count_and_gather_4k(t: &[i32], sa: &mut [i32], bk: &mut [i32]) -> usize {
    bk.fill(0);
    let n = t.len();
    let mut m = n - 1;
    let (mut c0, mut f0) = (t[n - 1], 1);
    for i in (0..n - 1).rev() {
        let c1 = t[i];
        let f1 = l_type(c1, c0, f0);
        sa[m] = (i + 1) as i32;
        m -= f1 & !f0 & 1;
        bk[i4(c0, f0 + f0 + f1)] += 1;
        (c0, f0) = (c1, f1);
    }
    sa[m] = 0;
    bk[i4(c0, f0 + f0)] += 1;
    n - 1 - m
}

/// Counts the suffixes by symbol and whether they are LMS (`2c + lms`) into
/// `bk`, and gathers the LMS positions into the end of `sa`.
fn count_and_gather_2k(t: &[i32], sa: &mut [i32], bk: &mut [i32]) {
    bk.fill(0);
    let n = t.len();
    let mut m = n - 1;
    let (mut c0, mut f0) = (t[n - 1], 1);
    for i in (0..n - 1).rev() {
        let c1 = t[i];
        let f1 = l_type(c1, c0, f0);
        let lms = f1 & !f0 & 1;
        sa[m] = (i + 1) as i32;
        m -= lms;
        bk[i2(c0, lms)] += 1;
        (c0, f0) = (c1, f1);
    }
    sa[m] = 0;
    bk[i2(c0, 0)] += 1;
}

/// [`count_and_gather_2k`] for a string whose unique LMS positions carry the
/// sign bit: they are counted but not gathered. Types are read off the raw
/// symbols as libsais does; a marked symbol starts an LMS suffix, so it
/// compares the same way unmarked.
fn count_and_gather_compacted_2k(t: &[i32], sa: &mut [i32], bk: &mut [i32]) {
    bk.fill(0);
    let n = t.len();
    let mut m = n - 1;
    let (mut c0, mut f0) = (t[n - 1], 1);
    for i in (0..n - 1).rev() {
        let c1 = t[i];
        let f1 = l_type(c1, c0, f0);
        let lms = f1 & !f0 & 1;
        sa[m] = (i + 1) as i32;
        m -= lms & usize::from(c0 >= 0);
        bk[i2(c0 & MAX, lms)] += 1;
        (c0, f0) = (c1, f1);
    }
    sa[m] = 0;
    bk[i2(c0 & MAX, 0)] += 1;
}

/// [`count_and_gather_2k`]'s counts alone.
fn count_lms_2k(t: &[i32], bk: &mut [i32]) {
    bk.fill(0);
    let n = t.len();
    let (mut c0, mut f0) = (t[n - 1], 1);
    for i in (0..n - 1).rev() {
        let c1 = t[i];
        let f1 = l_type(c1, c0, f0);
        bk[i2(c0, f1 & !f0 & 1)] += 1;
        (c0, f0) = (c1, f1);
    }
    bk[i2(c0, 0)] += 1;
}

/// Drops the suffixes before the first LMS suffix from the counts and puts
/// the LMS radix sort's bucket ends in `bk[4k..5k]`; returns the number of
/// L-type suffixes from the first LMS suffix on.
fn buckets_for_radix_sort_6k(t: &[i32], k: usize, bk: &mut [i32], first_lms: usize) -> usize {
    let (mut c0, mut f0) = (t[first_lms], 0);
    for p in (0..first_lms).rev() {
        let (c1, f1) = (c0, f0);
        c0 = t[p];
        f0 = l_type(c0, c1, f1);
        bk[i4(c1, f1 + f1 + f0)] -= 1;
    }
    bk[i4(c0, f0 + f0)] -= 1;
    let (counts, temp) = bk.split_at_mut(4 * k);
    let mut sum = 0;
    for (t, c) in temp[..k].iter_mut().zip(counts.as_chunks::<4>().0) {
        sum += c[1] + c[3];
        *t = sum;
    }
    sum as usize
}

/// Moves the LMS suffixes `sa[from..to]` to the ends of their buckets, right
/// to left.
fn radix_sort_lms_6k(t: &[i32], sa: &mut [i32], ends: &mut [i32], from: usize, to: usize) {
    for i in (from..to).rev() {
        let p = sa[i];
        let e = &mut ends[t[p as usize] as usize];
        *e -= 1;
        sa[*e as usize] = p;
    }
}

/// The partial sorts' induction heads (`bk[4c]`, `bk[4c + 1]`) and
/// distinct-name counters (`bk[4c + 2]`, `bk[4c + 3]`), and the right to
/// left scan's heads, to be moved into place later, in `bk[4k..6k]`.
fn buckets_for_partial_sorting_6k(
    t: &[i32],
    k: usize,
    bk: &mut [i32],
    first_lms: usize,
    left: usize,
) {
    let first = t[first_lms] as usize;
    let (counts, temp) = bk.split_at_mut(4 * k);
    let (mut sum0, mut sum1, mut sum2) = (left as i32 + 1, 0, 0);
    for (c, (b, tb)) in counts
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(temp.as_chunks_mut::<2>().0)
        .enumerate()
    {
        if c == first {
            sum1 += 1;
        }
        let [ss, ls, sl, ll] = *b;
        *b = [sum0, sum2, 0, 0];
        sum0 += ss + sl;
        sum1 += ls;
        sum2 += ls + ll;
        *tb = [sum0, sum1];
    }
}

/// Sorts the LMS substrings: the left-to-right scan over the L-type part,
/// the move of the group marks, and the right-to-left scan over the S-type
/// part. Each induced suffix carries the sign bit when it starts a new name
/// group.
fn induce_partial_order_6k(
    t: &[i32],
    sa: &mut [i32],
    k: usize,
    bk: &mut [i32],
    first_lms: usize,
    left: usize,
) {
    let n = t.len();
    let mut d = 1;
    let v = i4(t[n - 1], usize::from(t[n - 2] >= t[n - 1]));
    sa[bk[v] as usize] = (n - 1) as i32 | MIN;
    bk[v] += 1;
    bk[v + 2] = d;
    for i in 0..left {
        let mut p = sa[i];
        d += i32::from(p < 0);
        p &= MAX;
        let p = p as usize;
        let v = i4(t[p - 1], usize::from(t[p - 2] >= t[p - 1]));
        let h = bk[v];
        sa[h as usize] = (p - 1) as i32 | (i32::from(bk[v + 2] != d) << 31);
        bk[v] = h + 1;
        bk[v + 2] = d;
    }

    // The marks move from the start of each group of an L-type bucket to
    // its end.
    let (heads, temp) = bk.split_at_mut(4 * k);
    for c in (1..k).rev() {
        let mut s = MIN;
        let (lo, hi) = (temp[2 * (c - 1)] as usize, heads[4 * c] as usize);
        for x in sa[lo.min(hi)..hi].iter_mut().rev() {
            let p = *x;
            let q = (p & MIN) ^ s;
            s ^= q;
            *x = p ^ q;
        }
    }
    for (h, tb) in heads
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(temp.as_chunks::<2>().0)
    {
        h[0] = tb[0];
        h[1] = tb[1];
    }

    for i in (left + 1..n - first_lms).rev() {
        let mut p = sa[i];
        d += i32::from(p < 0);
        p &= MAX;
        let p = p as usize;
        let v = i4(t[p - 1], usize::from(t[p - 2] > t[p - 1]));
        let h = bk[v] - 1;
        bk[v] = h;
        sa[h as usize] = (p - 1) as i32 | (i32::from(bk[v + 2] != d) << 31);
        bk[v + 2] = d;
    }
}

/// Names the sorted LMS substrings `sa[..m]` (a sign bit where a group
/// ends) into `sa[m + p / 2]`, the name carrying the sign bit when its group
/// has one member; when the names are not unique, also marks each name
/// that differs from the one before it in text order. Returns the number of
/// names.
fn renumber_and_mark_distinct_4k(sa: &mut [i32], n: usize, m: usize) -> usize {
    sa[m..m + n / 2].fill(0);
    let (mut name, mut prev) = (1, 0);
    for i in 0..m {
        let p = sa[i];
        let q = p & MAX;
        sa[i] = q;
        sa[m + (q >> 1) as usize] = name | (p & prev & MIN);
        name += i32::from(p < 0);
        prev = p;
    }
    let names = (name - 1) as usize;
    if names < m {
        let mut prev = 0;
        for x in &mut sa[m..m + n / 2] {
            let p = *x;
            *x = p & (prev | MAX);
            if p != 0 {
                prev = p;
            }
        }
    }
    names
}

/// Takes the unique names out of the reduced string: marks their positions
/// in `t` with the sign bit, gathers the sorted rank of each, in text order,
/// into `sa[n + fs - m..][..f]`, and the other names, renumbered from 0 and
/// in text order, into `sa[n + fs - (m - f)..n + fs]`. Returns `f`.
fn compact(t: &mut [i32], sa: &mut [i32], n: usize, m: usize, fs: usize) -> usize {
    let mut f = 0;
    for i in 0..m {
        let p = sa[i] as usize;
        let mut s = sa[m + (p >> 1)];
        if s < 0 {
            t[p] |= MIN;
            f += 1;
            s = (i as i32 | MIN) + f;
        }
        sa[m + (p >> 1)] = s - f;
    }
    let (mut l, mut r) = (m, n + fs);
    for i in (m..m + n / 2).rev() {
        let p = sa[i];
        sa[l - 1] = p & MAX;
        l -= usize::from(p < 0);
        sa[r - 1] = p.wrapping_sub(1);
        r -= usize::from(p > 0);
    }
    let f = f as usize;
    sa.copy_within(m - f..m, n + fs - m);
    f
}

/// `sa[i] = sa[n - m + sa[i]]` for the first `m` entries: ranks of the
/// reduced string's suffixes to the LMS positions gathered at the end.
fn reconstruct_lms(sa: &mut [i32], m: usize) {
    let (head, gathered) = sa.split_at_mut(sa.len() - m);
    for v in &mut head[..m] {
        *v = gathered[*v as usize];
    }
}

/// Places each unique LMS position (marked in `t`, cleared here) at its
/// rank, the ranks listed in text order from `sa[n - m - 1]` on.
fn merge_unique(t: &mut [i32], sa: &mut [i32], m: usize) {
    let n = t.len();
    let mut q = n - m - 1;
    let mut rank = sa[q] as usize;
    q += 1;
    let mut i = 0;
    while i < n {
        let c = t[i];
        if c < 0 {
            t[i] = c & MAX;
            sa[rank] = i as i32;
            // Two LMS positions are never adjacent.
            i += 1;
            rank = sa[q] as usize;
            q += 1;
        }
        i += 1;
    }
}

/// Fills the ranks left empty by [`merge_unique`] with the sorted non-unique
/// LMS positions, listed from `sa[n - m - 1 + f]` on.
fn merge_nonunique(sa: &mut [i32], m: usize, f: usize) {
    let mut q = sa.len() - m - 1 + f;
    let mut next = sa[q];
    q += 1;
    for i in 0..m {
        if sa[i] == 0 {
            sa[i] = next;
            next = sa[q];
            q += 1;
        }
    }
}

/// Bucket starts into `bk[4k..5k]` and ends into `bk[5k..6k]` from the 4k
/// counts.
fn start_and_end_6k(k: usize, bk: &mut [i32]) {
    let (counts, rest) = bk.split_at_mut(4 * k);
    let (start, end) = rest.split_at_mut(k);
    let mut sum = 0;
    for ((s, e), c) in start
        .iter_mut()
        .zip(&mut end[..k])
        .zip(counts.as_chunks::<4>().0)
    {
        *s = sum;
        sum += c[0] + c[1] + c[2] + c[3];
        *e = sum;
    }
}

/// Bucket starts into `bk[2k..3k]` and ends into `bk[3k..4k]` from the 2k
/// counts.
fn start_and_end_4k(k: usize, bk: &mut [i32]) {
    let (counts, rest) = bk.split_at_mut(2 * k);
    let (start, end) = rest.split_at_mut(k);
    let mut sum = 0;
    for ((s, e), c) in start
        .iter_mut()
        .zip(&mut end[..k])
        .zip(counts[..2 * k].as_chunks::<2>().0)
    {
        *s = sum;
        sum += c[0] + c[1];
        *e = sum;
    }
}

/// Places the sorted LMS suffixes `sa[..m]` at the ends of their buckets,
/// zeros elsewhere. The LMS count of symbol `c` is `bk[stride * c + 1]`,
/// its bucket end `bk[ends + c]`.
fn place_lms_histogram(
    sa: &mut [i32],
    k: usize,
    mut m: usize,
    bk: &[i32],
    stride: usize,
    ends: usize,
) {
    let mut j = sa.len();
    for c in (0..k - 1).rev() {
        let l = bk[stride * c + 1];
        if l > 0 {
            let l = l as usize;
            let i = bk[ends + c] as usize;
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

/// The final induction: L-type suffixes left to right from the bucket
/// starts, then S-type ones right to left from the ends. A seed or induced
/// entry carries the sign bit while its predecessor is for the other scan.
fn induce_final_order(t: &[i32], sa: &mut [i32], start: &mut [i32], end: &mut [i32]) {
    let n = t.len();
    let c = t[n - 1] as usize;
    sa[start[c] as usize] = (n - 1) as i32 | (i32::from(t[n - 2] < t[n - 1]) << 31);
    start[c] += 1;
    for i in 0..n {
        let p = sa[i];
        sa[i] = p ^ MIN;
        if p > 0 {
            let p = (p - 1) as usize;
            let c = t[p];
            let h = &mut start[c as usize];
            sa[*h as usize] = p as i32 | (i32::from(t[p.saturating_sub(1)] < c) << 31);
            *h += 1;
        }
    }
    for i in (0..n).rev() {
        let p = sa[i];
        sa[i] = p & MAX;
        if p > 0 {
            let p = (p - 1) as usize;
            let c = t[p];
            let h = &mut end[c as usize];
            *h -= 1;
            sa[*h as usize] = p as i32 | (i32::from(t[p.saturating_sub(1)] > c) << 31);
        }
    }
}
