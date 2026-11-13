//! Resolving the ties among the names of the LMS substrings before a
//! recursion: suffixes whose LMS substrings share a name are ordered by the
//! names of the LMS substrings that follow them in the text, within a step
//! budget. If every tie is broken, the sorted LMS suffixes are known and the
//! recursion is skipped.
//!
//! Ported from libsais 2.12.0, Copyright (c) 2021-2026 Ilya Grebnov, under
//! the Apache License 2.0 with LLVM Exceptions (`LICENSE-libsais`), from the
//! copy that bz2-ng vendors: `libsais_resolve_nonunique_lms_suffixes_omp`
//! and the functions it calls. Altered: written in Rust, single-threaded
//! only, without prefetches, the group gathers as plain loops, the scratch
//! records as `[name, cursor, index]` triples.

const MIN: i32 = i32::MIN;
const MAX: i32 = i32::MAX;

/// Groups at most this large are sorted by insertion.
const INSERTION_SORT: usize = 16;
/// A group may use this many steps per member past its share of the budget
/// before it is put off until the others are ranked.
const DEFERRED_STEPS: isize = 16;
/// The budget: this many steps per LMS suffix.
const BUDGET_STEPS: isize = 4;
/// Most tie pairs sampled to estimate the cost of a costly resolve.
const SAMPLES: usize = 512;
/// libsais's single-threaded bound on the steps per sampled pair.
const LIMIT: usize = 4;

/// After naming the `m` sorted LMS substrings `sa[..m]` into `sa[m + p / 2]`
/// (`name` distinct names), orders the suffixes within each group of equal
/// names where that is cheap enough. Returns `m` when all ties are broken
/// (then `sa[..m]` are the sorted LMS suffixes, marks cleared but for the
/// 8u marks, and the names in `sa[m..]` are stale), otherwise `name` with
/// the names as they were.
///
/// `marked`: the top level's naming, where a sign bit in `sa[..m]` ends a
/// group and every name carries the sign bit; otherwise the 32s naming,
/// names from 1 with the sign bit on unique ones and `sa[..m]` unmarked.
/// Uses `sa[m + n / 2..n]` as scratch.
pub(super) fn resolve(sa: &mut [i32], n: usize, m: usize, name: usize, marked: bool) -> usize {
    let slots = n >> 1;
    let ties = m - name;
    if ties == 0 || m + slots + 1 > n {
        return name;
    }
    let (head, records) = sa[..n].split_at_mut(m + slots + 1);
    let (lms, sam) = head.split_at_mut(m);
    // The walk to the next LMS substring stops here at the latest.
    sam[slots] = MIN;
    let cheap = ties <= (m >> 3) + (m >> 4)
        || (ties <= (m >> 3) * LIMIT && sample(lms, sam, slots, marked));
    if !cheap {
        return name;
    }
    let Some(groups) = gather(lms, sam, records, marked) else {
        return name;
    };
    if sort_groups(
        lms,
        sam,
        slots,
        records,
        groups,
        budget_steps() * m as isize,
        marked,
    ) {
        m
    } else {
        name
    }
}

#[cfg(test)]
thread_local! {
    /// The steps per LMS suffix, which a test may lower to reach the
    /// deferred and failing paths; the suffix array is the same.
    pub(super) static BUDGET: core::cell::Cell<isize> = const { core::cell::Cell::new(BUDGET_STEPS) };
}

#[inline(always)]
fn budget_steps() -> isize {
    #[cfg(test)]
    return BUDGET.get();
    #[cfg(not(test))]
    BUDGET_STEPS
}

/// The name of the LMS substring after the one at slot `c`, and its slot;
/// the end of the string is -1, below every name.
#[inline(always)]
fn next(sam: &[i32], slots: usize, c: usize) -> (i32, usize) {
    let mut s = c + 1;
    while sam[s] == 0 {
        s += 1;
    }
    (if s < slots { sam[s] & MAX } else { -1 }, s)
}

/// Whether ties look cheap to break: from up to [`SAMPLES`] evenly spaced
/// pairs of neighbours with the same name, the steps taken (at most
/// [`DEFERRED_STEPS`] a pair) and the slots walked.
fn sample(lms: &[i32], sam: &[i32], slots: usize, marked: bool) -> bool {
    let m = lms.len();
    let samples = (m >> 7).min(SAMPLES);
    let stride = (m - 1).checked_div(samples).unwrap_or(0);
    let (mut steps, mut scanned) = (0, 0);
    let mut i = 0;
    while i < samples * stride && scanned <= m && 8 * steps <= LIMIT * samples {
        let (p0, p1) = ((lms[i] & MAX) as usize, (lms[i + 1] & MAX) as usize);
        let tied = if marked {
            lms[i] >= 0
        } else {
            sam[p0 >> 1] & MAX == sam[p1 >> 1] & MAX
        };
        if tied {
            let (mut c0, mut c1, mut t) = (p0 >> 1, p1 >> 1, 0);
            loop {
                let (n0, s0) = next(sam, slots, c0);
                let (n1, s1) = next(sam, slots, c1);
                scanned += (s0 - c0) + (s1 - c1);
                (c0, c1) = (s0, s1);
                t += 1;
                if n0 != n1 || t >= DEFERRED_STEPS as usize {
                    break;
                }
            }
            steps += t;
        }
        i += stride;
    }
    scanned <= m && 8 * steps <= LIMIT * samples
}

/// Records each group of two or more equal names as `[start, size]` pairs
/// in `records`; returns the number of groups, or `None` if they do not
/// fit.
fn gather(lms: &[i32], sam: &[i32], records: &mut [i32], marked: bool) -> Option<usize> {
    let m = lms.len();
    let mut l = 0;
    let mut push = |start: usize, end: usize| {
        if end - start > 1 {
            let r = records.get_mut(l..l + 2)?;
            r[0] = start as i32;
            r[1] = (end - start) as i32;
            l += 2;
        }
        Some(())
    };
    let mut start = 0;
    if marked {
        for (i, &s) in lms.iter().enumerate() {
            if s < 0 {
                push(start, i + 1)?;
                start = i + 1;
            }
        }
        push(start, m)?;
    } else {
        let mut prev = -1;
        for (i, &p) in lms.iter().enumerate() {
            let q = sam[(p >> 1) as usize] & MAX;
            if q != prev {
                push(start, i)?;
                start = i;
                prev = q;
            }
        }
        push(start, m)?;
    }
    Some(l / 2)
}

/// Sorts the groups by the names that follow, deferring a group that needs
/// more than its share of the budget until all the others are ranked.
/// Returns whether every group was sorted within the budget; if not, the
/// names in `sam` are as they were.
fn sort_groups(
    lms: &mut [i32],
    sam: &mut [i32],
    slots: usize,
    records: &mut [i32],
    groups: usize,
    mut budget: isize,
    marked: bool,
) -> bool {
    let (records, scratch) = records.split_at_mut(2 * groups);
    let mut deferred = 0;
    for g in 0..groups {
        if budget < 0 {
            break;
        }
        let (start, size) = (records[2 * g] as usize, records[2 * g + 1] as usize);
        let limit = if (size as isize) < budget / DEFERRED_STEPS {
            budget - DEFERRED_STEPS * size as isize
        } else {
            0
        };
        let fits = 4 * size <= scratch.len();
        if fits {
            budget = sort_group(lms, sam, slots, scratch, start, size, budget, limit);
        }
        if !fits || (budget >= 0 && budget < limit) {
            records[2 * g + 1] = -(size as i32);
            deferred += 1;
        }
    }
    if budget < 0 {
        // The names are untouched so far.
        return false;
    }
    if deferred == 0 {
        return true;
    }

    // Rank every suffix by its place, a deferred group's members by the
    // group's first place, and sort the deferred groups by those ranks.
    rank(lms, sam, records);
    for g in 0..groups {
        if budget < 0 {
            break;
        }
        if records[2 * g + 1] >= 0 {
            continue;
        }
        let start = records[2 * g] as usize;
        let size = -records[2 * g + 1] as usize;
        records[2 * g + 1] = size as i32;
        budget = if 4 * size <= scratch.len() {
            sort_group(lms, sam, slots, scratch, start, size, budget, 0)
        } else {
            -1
        };
        if budget >= 0 {
            for i in start..start + size {
                sam[((lms[i] & MAX) >> 1) as usize] = (i as i32 + 1) | MIN;
            }
        }
    }
    restore(records);
    if budget < 0 {
        rename(lms, sam, records, marked);
    }
    budget >= 0
}

/// Makes every group size positive again.
fn restore(records: &mut [i32]) {
    for r in records.as_chunks_mut::<2>().0 {
        r[1] = r[1].abs();
    }
}

/// `sam` of each suffix: its place plus one, or for a member of a deferred
/// group (negative size), the group's first place plus one; sign bit set.
fn rank(lms: &[i32], sam: &mut [i32], records: &[i32]) {
    let mut i = 0;
    let ranks = |sam: &mut [i32], from: usize, to: usize, r: Option<usize>| {
        for (k, &p) in lms[from..to].iter().enumerate() {
            let v = r.unwrap_or(from + k) as i32 + 1;
            sam[((p & MAX) >> 1) as usize] = v | MIN;
        }
    };
    for g in records.as_chunks::<2>().0 {
        if g[1] < 0 {
            let (start, end) = (g[0] as usize, (g[0] - g[1]) as usize);
            ranks(sam, i, start, None);
            ranks(sam, start, end, Some(start));
            i = end;
        }
    }
    ranks(sam, i, lms.len(), None);
}

/// Names the suffixes again as the naming before [`resolve`] did, every
/// recorded group sharing one name.
fn rename(lms: &[i32], sam: &mut [i32], records: &[i32], marked: bool) {
    let m = lms.len();
    let (mut g, mut ties) = (0, 0);
    let groups = records.len() / 2;
    for (i, &p) in lms.iter().enumerate() {
        let (start, size) = if g < groups {
            (records[2 * g] as usize, records[2 * g + 1] as usize)
        } else {
            (m, 1)
        };
        let slot = ((p & MAX) >> 1) as usize;
        if i < start {
            let name = (i - ties) as i32;
            sam[slot] = (if marked { name } else { name + 1 }) | MIN;
        } else {
            let name = (start - ties) as i32;
            sam[slot] = if marked { name | MIN } else { name + 1 };
            if i == start + size - 1 {
                ties += size - 1;
                g += 1;
            }
        }
    }
}

/// Sorts the group `lms[start..start + size]` by the names that follow its
/// suffixes, while `budget` stays at least `limit`; writes the order back
/// only if it did. Returns the budget left.
#[allow(clippy::too_many_arguments, reason = "libsais's signature")]
fn sort_group(
    lms: &mut [i32],
    sam: &[i32],
    slots: usize,
    scratch: &mut [i32],
    start: usize,
    size: usize,
    mut budget: isize,
    limit: isize,
) -> isize {
    if size == 2 {
        return sort_pair(lms, sam, slots, start, budget, limit);
    }
    let (suf, stack) = scratch[..4 * size].split_at_mut(3 * size);
    let (suf, _) = suf.as_chunks_mut::<3>();
    for (s, &p) in suf.iter_mut().zip(&lms[start..start + size]) {
        let p = p & MAX;
        *s = [0, p >> 1, p];
    }
    budget = advance_equal(sam, slots, suf, budget, limit);
    let mut top = 0;
    stack[0] = 0;
    stack[1] = size as i32;
    top += 2;
    while top > 0 && budget >= limit {
        top -= 2;
        let (lo, hi) = (stack[top] as usize, stack[top + 1] as usize);
        let mut push = |stack: &mut [i32], a: usize, b: usize| {
            stack[top] = a as i32;
            stack[top + 1] = b as i32;
            top += 2;
        };
        if hi - lo == 2 {
            while suf[lo][0] == suf[lo + 1][0] && budget >= limit {
                budget = advance(sam, slots, &mut suf[lo..lo + 2], budget);
            }
            if suf[lo][0] > suf[lo + 1][0] {
                suf.swap(lo, lo + 1);
            }
        } else if hi - lo <= INSERTION_SORT {
            for i in lo + 1..hi {
                let mut j = i;
                while j > lo && suf[j - 1][0] > suf[j][0] {
                    suf.swap(j - 1, j);
                    j -= 1;
                }
            }
            let mut i = lo;
            while i < hi {
                let mut j = i + 1;
                while j < hi && suf[j][0] == suf[i][0] {
                    j += 1;
                }
                if j - i > 1 {
                    budget = advance_equal(sam, slots, &mut suf[i..j], budget, limit);
                    push(stack, i, j);
                }
                i = j;
            }
        } else {
            let (a, b, c) = (suf[lo][0], suf[lo + (hi - lo) / 2][0], suf[hi - 1][0]);
            let (x, y) = (a.min(b), a.max(b));
            let v = c.clamp(x, y);
            let (mut lt, mut gt, mut i) = (lo, hi, lo);
            while i < gt {
                let s = suf[i][0];
                if s < v {
                    suf.swap(i, lt);
                    lt += 1;
                    i += 1;
                } else if s > v {
                    gt -= 1;
                    suf.swap(i, gt);
                } else {
                    i += 1;
                }
            }
            if lt - lo > 1 {
                budget -= (lt - lo) as isize;
                push(stack, lo, lt);
            }
            if hi - gt > 1 {
                budget -= (hi - gt) as isize;
                push(stack, gt, hi);
            }
            if gt - lt > 1 {
                budget = advance_equal(sam, slots, &mut suf[lt..gt], budget, limit);
                push(stack, lt, gt);
            }
        }
    }
    if budget >= limit {
        for (d, s) in lms[start..start + size].iter_mut().zip(suf.iter()) {
            *d = s[2];
        }
    }
    budget
}

/// [`sort_group`] for two suffixes.
fn sort_pair(
    lms: &mut [i32],
    sam: &[i32],
    slots: usize,
    start: usize,
    mut budget: isize,
    limit: isize,
) -> isize {
    let (p0, p1) = (lms[start] & MAX, lms[start + 1] & MAX);
    let (mut c0, mut c1) = ((p0 >> 1) as usize, (p1 >> 1) as usize);
    let (mut n0, mut n1);
    loop {
        let (a, s0) = next(sam, slots, c0);
        let (b, s1) = next(sam, slots, c1);
        (n0, n1) = (a, b);
        budget -= 2 + ((s0 - c0) >> 4) as isize + ((s1 - c1) >> 4) as isize;
        (c0, c1) = (s0, s1);
        if n0 != n1 || budget < limit {
            break;
        }
    }
    if budget >= limit {
        let (first, second) = if n0 < n1 { (p0, p1) } else { (p1, p0) };
        lms[start] = first;
        lms[start + 1] = second;
    }
    budget
}

/// Moves each suffix of `suf` to the next LMS substring and takes its name.
#[inline(always)]
fn advance(sam: &[i32], slots: usize, suf: &mut [[i32; 3]], mut budget: isize) -> isize {
    for s in suf {
        let c = s[1] as usize;
        let (name, at) = next(sam, slots, c);
        *s = [name, at as i32, s[2]];
        budget -= 1 + ((at - c) >> 4) as isize;
    }
    budget
}

/// [`advance`] until the names of `suf` are not all equal or the budget
/// falls below `limit`.
fn advance_equal(
    sam: &[i32],
    slots: usize,
    suf: &mut [[i32; 3]],
    mut budget: isize,
    limit: isize,
) -> isize {
    while budget >= limit {
        budget = advance(sam, slots, suf, budget);
        let first = suf[0][0];
        if suf.iter().any(|s| s[0] != first) {
            break;
        }
    }
    budget
}
