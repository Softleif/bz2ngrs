//! The block sorter: the cyclic BWT of a Lyndon word by induced sorting
//! (SA-IS, Nong, Zhang and Chan).
//!
//! The top level, on the block's bytes, is libsais's (`sais_top.rs`). The
//! recursion on the reduced string and the final induction, which writes the
//! BWT directly, are a port of the sorter bz2-ng used before it adopted
//! libsais (`sort.c` and `sort_template.h` of bz2-ng's history, MIT), which
//! derives from Yuta Mori's sais-lite 2.4.1 (MIT, `LICENSE-sais-lite`).
//! Measured on CRAM tag data, libsais's top level is the faster one, and
//! these two are as fast as libsais's own.
//!
//! For a Lyndon word (strictly smaller than each of its proper rotations)
//! suffix order equals rotation order, so a suffix sorter with an implicit
//! sentinel yields bzip2's rotation order directly; the encoder rotates every
//! block to such a word first (`rotate.rs`).
//!
//! The suffix array and the recursion's scratch live in one work array, as
//! in bz2-ng: the suffix array at its start, then free space holding names,
//! the LMS list and the reduced string, and a level's bucket arrays at its
//! end. Those regions are disjoint at every step, so each phase splits the
//! array instead of aliasing it. Where bz2-ng would fail a level whose
//! buckets do not fit (it may not allocate), this allocates them; with the
//! work size of [`work_len`] that never happens.
//!
//! Kernel forms are bz2-ng's choices for clang on the server cores
//! (`SORT_SUFFIX0_APART` on; branchy naming, branchy selects and no
//! empty-slot skipping), since rustc's backend is LLVM.

use super::sais_top as top;

/// Ints of work array [`bwt`] uses for a block of `n` bytes: the suffix
/// array and free space of more than `n / 2` ([`free_len`]), then the top
/// level's LMS positions (at most `n / 2`), kept for after the recursion
/// rather than found again as libsais does.
pub(crate) fn work_len(n: usize) -> usize {
    n + free_len(n) + n / 2
}

/// The free space after the suffix array. The top level names its `m <= n /
/// 2` LMS substrings in the `n / 2` slots after them and gathers the reduced
/// string into the last `m`; every level below has an alphabet below `n / 2`
/// and at least that free space, so its buckets fit beside it.
fn free_len(n: usize) -> usize {
    n / 2 + 2048
}

/// The cyclic BWT of the Lyndon word `t`, written over `t`; the byte
/// histogram of `t` into `freq`; returns the row of rotation `q < t.len()`.
/// `work` is grown to [`work_len`] if it is shorter. The top level's
/// left-to-right scan picks the bucket-cursor mode of everything after it.
pub(crate) fn bwt(t: &mut [u8], q: usize, freq: &mut [i32; 256], work: &mut Vec<i32>) -> usize {
    let n = t.len();
    if n <= 1 {
        freq.fill(0);
        if let Some(&b) = t.first() {
            freq[usize::from(b)] = 1;
        }
        return 0;
    }
    if work.len() < work_len(n) {
        work.resize(work_len(n), 0);
    }
    let fs = free_len(n);
    let (sa, lms) = work.split_at_mut(n + fs);
    let mut bk = top::Buckets::new();
    let m = top::count_and_gather(t, sa, &mut bk);
    lms[..m].copy_from_slice(&sa[n - m..n]);
    top::start_and_end(&mut bk, freq);
    let mut mode = false;
    if m > 0 {
        let first_lms = sa[n - m] as usize;
        let left_count = top::buckets_for_radix_sort(t, &mut bk, first_lms);
        top::radix_sort_lms(t, sa, &mut bk, n - m + 1, n);
        top::buckets_for_partial_sorting(t, &mut bk, first_lms, left_count);
        mode = top::induce_partial_order(t, sa, &mut bk, first_lms, left_count);
        let names = top::renumber_and_gather(sa, n, m, fs);
        if names < m {
            let (rec, rest) = sa.split_at_mut(n + fs - m);
            if mode {
                sort_main::<true>(rest, rec, fs + n - 2 * m, names);
            } else {
                sort_main::<false>(rest, rec, fs + n - 2 * m, names);
            }
            for v in &mut sa[..m] {
                *v = lms[*v as usize];
            }
        }
        top::place_lms(sa, n, m, &bk);
    } else {
        sa[..n].fill(0);
    }
    let (mut start, mut end) = (bk.start, bk.end);
    let (pidx, qrank) = if mode {
        compute_bwt::<true>(t, &mut sa[..n], &mut start, &mut end, q)
    } else {
        compute_bwt::<false>(t, &mut sa[..n], &mut start, &mut end, q)
    };
    debug_assert_eq!(pidx, 0, "the block is not a Lyndon word");
    t[0] = t[n - 1];
    for (l, &s) in t[1..].iter_mut().zip(&sa[1..n]) {
        *l = s as u8;
    }
    if q == 0 { 0 } else { qrank }
}

/// A symbol of a level's string: bytes at the top, names below it.
trait Sym: Copy {
    fn get(self) -> usize;

    /// Symbol counts of `t` into `c` (one counter per symbol).
    fn counts(t: &[Self], c: &mut [i32]) {
        c.fill(0);
        for &x in t {
            c[x.get()] += 1;
        }
    }
}

impl Sym for i32 {
    #[inline(always)]
    fn get(self) -> usize {
        self as usize
    }
}

/// Bucket starts (`end` false) or ends (`end` true) from the counts `c`.
fn get_buckets(c: &[i32], b: &mut [i32], end: bool) {
    let mut sum = 0;
    for (b, &c) in b.iter_mut().zip(c) {
        sum += c;
        *b = if end { sum } else { sum - c };
    }
}

/// [`get_buckets`] with the counts in `b` itself.
fn counts_to_buckets(b: &mut [i32], end: bool) {
    let mut sum = 0;
    for b in b.iter_mut() {
        let c = *b;
        sum += c;
        *b = if end { sum } else { sum - c };
    }
}

/// A level's bucket heads `b` and counts `c`. An empty `c` means the two
/// share `b` (too little free space for both), so the counts are taken
/// again from the string whenever the heads are reset.
struct Buckets<'a> {
    b: &'a mut [i32],
    c: &'a mut [i32],
}

impl Buckets<'_> {
    fn counts(&mut self) -> &mut [i32] {
        if self.c.is_empty() { self.b } else { self.c }
    }

    fn heads_from_counts(&mut self, end: bool) {
        if self.c.is_empty() {
            counts_to_buckets(self.b, end);
        } else {
            get_buckets(self.c, self.b, end);
        }
    }

    fn set<S: Sym>(&mut self, t: &[S], end: bool) {
        if self.c.is_empty() {
            S::counts(t, self.b);
        }
        self.heads_from_counts(end);
    }
}

/// The part of `sa` before its last `tail_len` ints, and those ints; or all
/// of `sa` and `ext` when the buckets did not fit in `sa`.
fn split_tail<'a>(
    sa: &'a mut [i32],
    ext: &'a mut [i32],
    tail_len: usize,
) -> (&'a mut [i32], &'a mut [i32]) {
    if ext.is_empty() {
        let at = sa.len() - tail_len;
        sa.split_at_mut(at)
    } else {
        (sa, ext)
    }
}

/// The bucket arrays at the end of `tail` (`[.. | B | C]`, or `[.. | B]` when
/// `one`), and what precedes them.
fn layout(tail: &mut [i32], k: usize, one: bool) -> (&mut [i32], Buckets<'_>) {
    if one {
        let at = tail.len() - k;
        let (front, b) = tail.split_at_mut(at);
        (front, Buckets { b, c: &mut [] })
    } else {
        let at = tail.len() - 2 * k;
        let (front, bc) = tail.split_at_mut(at);
        let (b, c) = bc.split_at_mut(k);
        (front, Buckets { b, c })
    }
}

/// The write cursor of an induction scan. With `REG` the current bucket's
/// head is kept here and written back when the bucket changes (a branch per
/// step, mispredicted at every change, so it suits repetitive blocks);
/// without it the head is updated in `B` every step (no branch, but a
/// store-to-load chain while the bucket repeats).
struct Cur<const REG: bool> {
    c1: usize,
    b: usize,
}

impl<const REG: bool> Cur<REG> {
    #[inline(always)]
    fn new(c1: usize, bk: &[i32]) -> Self {
        Cur {
            c1,
            b: if REG { bk[c1] as usize } else { 0 },
        }
    }

    #[inline(always)]
    fn switch(&mut self, c0: usize, bk: &mut [i32]) {
        if REG {
            if c0 != self.c1 {
                bk[self.c1] = self.b as i32;
                self.c1 = c0;
                self.b = bk[c0] as usize;
            }
        } else {
            self.c1 = c0;
        }
    }

    #[inline(always)]
    fn put_inc(&mut self, sa: &mut [i32], bk: &mut [i32], v: i32) {
        if REG {
            sa[self.b] = v;
            self.b += 1;
        } else {
            let p = bk[self.c1];
            sa[p as usize] = v;
            bk[self.c1] = p + 1;
        }
    }

    #[inline(always)]
    fn put_dec(&mut self, sa: &mut [i32], bk: &mut [i32], v: i32) {
        if REG {
            self.b -= 1;
            sa[self.b] = v;
        } else {
            let p = bk[self.c1] - 1;
            bk[self.c1] = p;
            sa[p as usize] = v;
        }
    }

    #[inline(always)]
    fn pos_inc(&self, bk: &[i32]) -> usize {
        if REG { self.b } else { bk[self.c1] as usize }
    }
}

#[inline(always)]
fn mark_if(cond: bool, v: i32) -> i32 {
    if cond { !v } else { v }
}

/// The L-scan of the LMS-substring sort, naming the substrings as it goes,
/// then the group marks moved to the other end of each group. `d[2c + t]`
/// holds the pass counter of the last suffix written to bucket `c` from a
/// source of type `t`, and an entry gets `+n` when it starts a new name
/// group. Seeds must be in place with the group starts marked. Returns the
/// pass counter for the S-scan.
fn lms_sort2_l<S: Sym, const REG: bool>(
    t: &[S],
    sa: &mut [i32],
    c: &[i32],
    b: &mut [i32],
    d: &mut [i32],
) -> i32 {
    let n = t.len();
    let ni = n as i32;
    get_buckets(c, b, false);
    let c1 = t[n - 1].get();
    let mut cur = Cur::<REG>::new(c1, b);
    let j = n - 2;
    let jv = (j + n) as i32;
    cur.put_inc(sa, b, mark_if(t[j].get() < c1, jv));
    let mut dd = 0;
    for i in 0..n {
        let mut j = sa[i];
        if 0 < j {
            if ni <= j {
                dd += 1;
                j -= ni;
            }
            let c0 = t[j as usize].get();
            cur.switch(c0, b);
            j -= 1;
            let ty = (c0 << 1) | usize::from(t[j as usize].get() < c0);
            if d[ty] != dd {
                j += ni;
                d[ty] = dd;
            }
            cur.put_inc(sa, b, mark_if(ty & 1 != 0, j));
            sa[i] = 0;
        } else if j < 0 {
            sa[i] = !j;
        }
    }
    let mut i = n as isize - 1;
    while 0 <= i {
        let v = sa[i as usize];
        if 0 < v && v < ni {
            sa[i as usize] = v + ni;
            let mut j = i - 1;
            while sa[j as usize] < ni {
                j -= 1;
            }
            sa[j as usize] -= ni;
            i = j;
        }
        i -= 1;
    }
    dd
}

/// The S-scan of the LMS-substring sort, continuing the naming of
/// [`lms_sort2_l`] from its pass counter `dd`.
fn lms_sort2_s<S: Sym, const REG: bool>(
    t: &[S],
    sa: &mut [i32],
    c: &[i32],
    b: &mut [i32],
    d: &mut [i32],
    dd: i32,
) {
    let n = t.len();
    let ni = n as i32;
    get_buckets(c, b, true);
    let mut cur = Cur::<REG>::new(0, b);
    let mut dd = dd + 1;
    for i in (0..n).rev() {
        let mut j = sa[i];
        if 0 < j {
            if ni <= j {
                dd += 1;
                j -= ni;
            }
            let c0 = t[j as usize].get();
            cur.switch(c0, b);
            j -= 1;
            let ty = (c0 << 1) | usize::from(t[j as usize].get() > c0);
            if d[ty] != dd {
                j += ni;
                d[ty] = dd;
            }
            cur.put_dec(sa, b, if ty & 1 != 0 { !(j + 1) } else { j });
            sa[i] = 0;
        }
    }
}

/// Compacts the LMS substrings sorted by [`lms_sort2_l`] and
/// [`lms_sort2_s`] into `sa[..m]` and, when names are not unique, stores
/// name + 1 of the substring at `p` in `sa[m + p / 2]`. Returns the number
/// of names. A non-negative slot writes a placeholder to `sa[d]` that the
/// next marked one overwrites, so the loop has no data-dependent branch.
fn lms_postproc2(sa: &mut [i32], n: usize, m: usize) -> usize {
    let ni = n as i32;
    let (mut i, mut d, mut name) = (0, 0, 0);
    while d < m {
        let v = sa[i];
        let neg = v < 0;
        let j = !v;
        name += usize::from(neg && j >= ni);
        sa[i] = 0;
        sa[d] = j;
        d += usize::from(neg);
        i += 1;
    }
    if name < m {
        let mut d = name as i32 + 1;
        for i in (0..m).rev() {
            let mut j = sa[i];
            if j >= ni {
                j -= ni;
                d -= 1;
            }
            sa[m + (j >> 1) as usize] = d;
        }
    } else {
        for v in &mut sa[..m] {
            if *v >= ni {
                *v -= ni;
            }
        }
    }
    name
}

/// Sorts the LMS substrings by induction without naming them, for levels
/// whose free space has no room for the naming counters.
fn lms_sort1<S: Sym, const REG: bool>(t: &[S], sa: &mut [i32], bk: &mut Buckets<'_>) {
    let n = t.len();
    bk.set(t, false);
    let b = &mut *bk.b;
    let c1 = t[n - 1].get();
    let mut cur = Cur::<REG>::new(c1, b);
    let j = (n - 2) as i32;
    cur.put_inc(sa, b, mark_if(t[n - 2].get() < c1, j));
    for i in 0..n {
        let mut j = sa[i];
        if 0 < j {
            let c0 = t[j as usize].get();
            cur.switch(c0, b);
            j -= 1;
            cur.put_inc(sa, b, mark_if(t[j as usize].get() < c0, j));
            sa[i] = 0;
        } else if j < 0 {
            sa[i] = !j;
        }
    }
    bk.set(t, true);
    let b = &mut *bk.b;
    let mut cur = Cur::<REG>::new(0, b);
    for i in (0..n).rev() {
        let mut j = sa[i];
        if 0 < j {
            let c0 = t[j as usize].get();
            cur.switch(c0, b);
            j -= 1;
            cur.put_dec(
                sa,
                b,
                if t[j as usize].get() > c0 {
                    !(j + 1)
                } else {
                    j
                },
            );
            sa[i] = 0;
        }
    }
}

/// Names the LMS substrings sorted by [`lms_sort1`] by comparing neighbours;
/// stores the names as [`lms_postproc2`] does and returns their number.
fn lms_postproc1<S: Sym>(t: &[S], sa: &mut [i32], m: usize) -> usize {
    let n = t.len();
    let mut i = 0;
    while sa[i] < 0 {
        sa[i] = !sa[i];
        i += 1;
    }
    if i < m {
        let mut j = i;
        i += 1;
        loop {
            let p = sa[i];
            if p < 0 {
                sa[j] = !p;
                j += 1;
                sa[i] = 0;
                if j == m {
                    break;
                }
            }
            i += 1;
        }
    }
    // The length of each LMS substring (from its position to the next LMS
    // position, inclusive) into sa[m + p / 2]. A pair of positions 2k, 2k+1
    // holds at most one LMS position.
    {
        let mut s1 = false;
        let mut j = n - 1;
        let mut c1 = t[n - 1].get();
        for i in (0..n - 1).rev() {
            let c0 = t[i].get();
            let s0 = c0 < c1 || (c0 == c1 && s1);
            if s1 && !s0 {
                sa[m + ((i + 1) >> 1)] = (j - i) as i32;
                j = i + 1;
            }
            s1 = s0;
            c1 = c0;
        }
    }
    let (mut name, mut q, mut qlen) = (0usize, n, 0i32);
    for i in 0..m {
        let p = sa[i] as usize;
        let plen = sa[m + (p >> 1)];
        let mut diff = true;
        if plen == qlen && q + (plen as usize) < n {
            let len = plen as usize;
            diff = t[p..p + len]
                .iter()
                .zip(&t[q..q + len])
                .any(|(a, b)| a.get() != b.get());
        }
        if diff {
            name += 1;
            q = p;
            qlen = plen;
        }
        sa[m + (p >> 1)] = name as i32;
    }
    name
}

/// Final induction of a reduced problem: its suffix array from the sorted
/// LMS suffixes seeded at the ends of their buckets.
fn induce_sa<S: Sym, const REG: bool>(t: &[S], sa: &mut [i32], bk: &mut Buckets<'_>) {
    let n = t.len();
    bk.set(t, false);
    let b = &mut *bk.b;
    let j = n - 1;
    let c1 = t[j].get();
    let mut cur = Cur::<REG>::new(c1, b);
    cur.put_inc(sa, b, mark_if(j > 0 && t[j - 1].get() < c1, j as i32));
    for i in 0..n {
        let j = sa[i];
        sa[i] = !j;
        // Entry 1 induces suffix 0, which has no predecessor; every other
        // inducing entry reads t[j - 1].
        if 1 < j {
            let j = (j - 1) as usize;
            let c0 = t[j].get();
            cur.switch(c0, b);
            cur.put_inc(sa, b, mark_if(t[j - 1].get() < c0, j as i32));
        } else if j == 1 {
            cur.switch(t[0].get(), b);
            cur.put_inc(sa, b, 0);
        }
    }
    bk.set(t, true);
    let b = &mut *bk.b;
    let mut cur = Cur::<REG>::new(0, b);
    for i in (0..n).rev() {
        let j = sa[i];
        if 1 < j {
            let j = (j - 1) as usize;
            let c0 = t[j].get();
            cur.switch(c0, b);
            cur.put_dec(sa, b, mark_if(t[j - 1].get() > c0, j as i32));
        } else if j == 1 {
            cur.switch(t[0].get(), b);
            cur.put_dec(sa, b, !0);
        } else {
            sa[i] = !j;
        }
    }
}

/// Final induction for the block: leaves the BWT byte of every row in `sa`
/// and returns the row of suffix 0 (which holds 0) and that of suffix `q`
/// (`1 <= q < n`, recorded when the suffix is written; `usize::MAX` for
/// `q = 0`). Every suffix but 0 is written to its final row exactly once.
fn compute_bwt<const REG: bool>(
    t: &[u8],
    sa: &mut [i32],
    start: &mut [i32; 256],
    end: &mut [i32; 256],
    q: usize,
) -> (usize, usize) {
    let n = t.len();
    let mut qr = usize::MAX;
    let b = &mut start[..];
    let j = n - 1;
    let c1 = usize::from(t[j]);
    let mut cur = Cur::<REG>::new(c1, b);
    if j == q {
        qr = cur.pos_inc(b);
    }
    cur.put_inc(sa, b, mark_if(usize::from(t[j - 1]) < c1, j as i32));
    for i in 0..n {
        let e = sa[i];
        if 0 < e {
            let j = (e - 1) as usize;
            let c0 = usize::from(t[j]);
            sa[i] = !(c0 as i32);
            cur.switch(c0, b);
            if j == q {
                qr = cur.pos_inc(b);
            }
            // Suffix 0 of a Lyndon word is S-type and never induced here, so
            // j >= 1; the saturation only keeps a broken caller in bounds.
            let pv = usize::from(t[j.saturating_sub(1)]);
            cur.put_inc(sa, b, mark_if(pv < c0, j as i32));
        } else if e != 0 {
            sa[i] = !e;
        }
    }
    let b = &mut end[..];
    let mut cur = Cur::<REG>::new(0, b);
    let mut pidx = usize::MAX;
    for i in (0..n).rev() {
        let e = sa[i];
        // Only the entry of suffix 1 induces suffix 0, and it is left out:
        // suffix 0 is the smallest, so its row is 0, and sa[0] still holds
        // the 0 the seeding left there. Slot 0 is never a seed (suffix 0 is
        // not LMS), the L-scan writes nothing to bucket t[0] (a Lyndon word
        // of length >= 2 cannot end in its least symbol), and this scan fills
        // that bucket from its end with every S-type suffix but 0. That
        // entry's row gets its BWT byte t[0].
        if 2 <= e {
            let j = (e - 1) as usize;
            let c0 = usize::from(t[j]);
            sa[i] = c0 as i32;
            cur.switch(c0, b);
            if j == q {
                qr = cur.pos_inc(b) - 1;
            }
            let pv = usize::from(t[j - 1]);
            cur.put_dec(sa, b, if pv > c0 { !(pv as i32) } else { j as i32 });
        } else if e < 0 {
            sa[i] = !e;
        } else if e == 0 {
            pidx = i;
        } else {
            sa[i] = i32::from(t[0]);
        }
    }
    (pidx, qr)
}

/// Where [`sais_reduce`] left a level's buckets: `one` when counts and heads
/// share one array, and `ext` holding them when they did not fit in the
/// work array (empty otherwise).
struct Reduced {
    one: bool,
    ext: Vec<i32>,
}

/// Stages 1 and 2 of SA-IS on a reduced problem, then its sorted LMS
/// suffixes placed at the ends of their buckets (stage 3 seeding); the
/// caller runs the final induction with the buckets this leaves. `sa` has
/// `n + fs` ints, and the bucket arrays come from its last ones. A level
/// recounts its buckets after recursing rather than reserving them, so each
/// level's free space is at least its parent's.
fn sais_reduce<S: Sym, const REG: bool>(t: &[S], sa: &mut [i32], fs: usize, k: usize) -> Reduced {
    let n = t.len();
    let mut ext = Vec::new();
    let (one, mut recount) = if 2 * k <= fs {
        (false, 0)
    } else if k <= fs {
        (true, 1)
    } else {
        ext = vec![0; 2 * k];
        (false, 0)
    };
    let lmssort2 = recount == 0 && ext.is_empty() && 2 <= n / k && 4 * k < fs;
    let used_tail = if !ext.is_empty() {
        0
    } else if lmssort2 {
        4 * k
    } else if one {
        k
    } else {
        2 * k
    };
    // Room for the list of LMS positions below the bucket arrays.
    let listed = fs - used_tail > n / 2;

    let m;
    let name;
    {
        let (main, tail) = split_tail(sa, &mut ext, used_tail);
        let (d, mut bk) = layout(tail, k, one);
        S::counts(t, bk.counts());
        bk.heads_from_counts(true);
        main[..n].fill(0);
        // Each LMS position p reserves the next slot from the end of bucket
        // t[p]; the slot reserved by the previous (right-hand) LMS receives
        // j, the L-type position before that LMS. Suffix types follow the
        // recurrence s(i) = t[i] < t[i+1] or (t[i] = t[i+1] and s(i+1)).
        let mut slot: Option<usize> = None;
        let mut j = n as i32;
        let mut mm = 0;
        if listed {
            // The LMS positions into the free space just below the bucket
            // arrays (found right to left, stored downwards, so they end up
            // in ascending order), then the bucket slots for them.
            let end = main.len();
            let mut s1 = false;
            let mut c1 = t[n - 1].get();
            for i in (0..n - 1).rev() {
                let c0 = t[i].get();
                let s0 = c0 < c1 || (c0 == c1 && s1);
                main[end - 1 - mm] = (i + 1) as i32;
                mm += usize::from(s1 && !s0);
                s1 = s0;
                c1 = c0;
            }
            for i in (0..mm).rev() {
                let p = main[end - mm + i] as usize;
                if let Some(s) = slot {
                    main[s] = j;
                }
                let c = t[p].get();
                bk.b[c] -= 1;
                slot = Some(bk.b[c] as usize);
                j = p as i32 - 1;
            }
        } else {
            let mut s1 = false;
            let mut c1 = t[n - 1].get();
            for i in (0..n - 1).rev() {
                let c0 = t[i].get();
                let s0 = c0 < c1 || (c0 == c1 && s1);
                if s1 && !s0 {
                    if let Some(s) = slot {
                        main[s] = j;
                    }
                    bk.b[c1] -= 1;
                    slot = Some(bk.b[c1] as usize);
                    j = i as i32;
                    mm += 1;
                }
                s1 = s0;
                c1 = c0;
            }
        }
        m = mm;

        name = if 1 < m {
            if lmssort2 {
                let (b, c) = (&mut *bk.b, &*bk.c);
                b[t[(j + 1) as usize].get()] += 1;
                let mut end = 0;
                for i in 0..k {
                    end += c[i];
                    if b[i] != end {
                        main[b[i] as usize] += n as i32;
                    }
                    d[i] = 0;
                    d[i + k] = 0;
                }
                let dd = lms_sort2_l::<S, REG>(t, &mut main[..n], c, b, d);
                lms_sort2_s::<S, REG>(t, &mut main[..n], c, b, d, dd);
                lms_postproc2(main, n, m)
            } else {
                lms_sort1::<S, REG>(t, &mut main[..n], &mut bk);
                lms_postproc1(t, main, m)
            }
        } else if m == 1 {
            if let Some(s) = slot {
                main[s] = j + 1;
            }
            1
        } else {
            0
        };
    }

    if name < m {
        // Keep the LMS list through the recursion when the recursion still
        // gets free space >= m (enough for every level below, see
        // `work_len`) and the reduced string, placed just below the list,
        // stays clear of the names it is compacted from. Otherwise the LMS
        // positions are found again after the recursion.
        let keep_list = listed && n + fs - used_tail >= 4 * m && n / 2 + fs - used_tail >= 3 * m;
        let newfs = if keep_list {
            // [0, m + newfs) recursion | reduced string | LMS list | buckets
            n + fs - used_tail - 3 * m
        } else {
            if ext.is_empty() {
                recount = 2;
            }
            n + fs - 2 * m
        };
        let ra = m + newfs;
        // Unconditional stores: a slot is rewritten until a name lands in it.
        let (mut i, mut j) = (m + n / 2, m as isize - 1);
        while m < i && 0 <= j {
            i -= 1;
            let v = sa[i];
            sa[ra + j as usize] = v - 1;
            j -= isize::from(v != 0);
        }
        {
            let (rec, rest) = sa.split_at_mut(ra);
            sort_main::<REG>(&rest[..m], rec, newfs, name);
        }
        if keep_list {
            let list = n + fs - used_tail - m;
            for i in 0..m {
                sa[i] = sa[list + sa[i] as usize];
            }
        } else {
            // sa[ra..ra + m] = the LMS positions, left to right.
            let mut s1 = false;
            let mut c1 = t[n - 1].get();
            let (mut i, mut j) = (n - 1, m as isize - 1);
            while 0 < i && 0 <= j {
                i -= 1;
                let c0 = t[i].get();
                let s0 = c0 < c1 || (c0 == c1 && s1);
                sa[ra + j as usize] = (i + 1) as i32;
                j -= isize::from(s1 && !s0);
                s1 = s0;
                c1 = c0;
            }
            for i in 0..m {
                sa[i] = sa[ra + sa[i] as usize];
            }
        }
    }

    {
        let (main, tail) = split_tail(sa, &mut ext, if one { k } else { 2 * k });
        let (_, mut bk) = layout(tail, k, one);
        if recount != 0 {
            S::counts(t, bk.counts());
        }
        if 1 < m {
            bk.heads_from_counts(true);
            let b = &*bk.b;
            let mut i = m as isize - 1;
            let mut j = n;
            let mut p = main[m - 1];
            let mut c1 = t[p as usize].get();
            loop {
                let c0 = c1;
                let q2 = b[c0] as usize;
                while q2 < j {
                    j -= 1;
                    main[j] = 0;
                }
                loop {
                    j -= 1;
                    main[j] = p;
                    i -= 1;
                    if i < 0 {
                        break;
                    }
                    p = main[i as usize];
                    c1 = t[p as usize].get();
                    if c1 != c0 {
                        break;
                    }
                }
                if i < 0 {
                    break;
                }
            }
            main[..j].fill(0);
        }
    }
    Reduced { one, ext }
}

/// The suffix array of the name string `t` over `0..k`, for the recursion,
/// into `sa[..t.len()]` (`sa` has `t.len() + fs` ints).
fn sort_main<const REG: bool>(t: &[i32], sa: &mut [i32], fs: usize, k: usize) {
    let n = t.len();
    let mut r = sais_reduce::<i32, REG>(t, sa, fs, k);
    let (main, tail) = split_tail(sa, &mut r.ext, if r.one { k } else { 2 * k });
    let (_, mut bk) = layout(tail, k, r.one);
    induce_sa::<i32, REG>(t, &mut main[..n], &mut bk);
}
