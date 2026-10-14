//! Inverse BWT (`dec_block.c`, WIDE layout): a table build in four
//! interleaved quarters, splitter rows that cut the text into segments, a
//! walk of several segments in lock step into a pool of 1 KiB chunks, and a
//! gather of the segments in text order.
//!
//! The table maps each row to its successor in text order, `T(LF(i)) = i`,
//! with the row's F symbol in the low byte. It is a scatter by the exact
//! prefix sums of the symbols in L, so it is a permutation of `[0, n)` for any
//! L, hostile included: every walk step stays in range and every segment ends
//! at a marked row.
//!
//! All arrays are fixed powers of two indexed with masks, so the hot loops
//! need no bounds checks; a valid index never exceeds 900,000 < 2^20, so the
//! masks change nothing.

/// Rows in the largest block, rounded up to a power of two.
pub(crate) const ROWS: usize = 1 << 20;
pub(crate) const ROW_MASK: usize = ROWS - 1;
/// L, the pool and the text: one block plus slack for 16-byte stores.
pub(crate) const BYTES: usize = ROWS + 64;

const CHUNK: usize = 1024;
/// Pool chunks: at most (900,000 + 25 * 1024) / 1024 + 1 = 905 are used.
const CHUNKS: usize = 1024;
/// Target rows per segment.
const SEG_ROWS: u32 = 2048;
/// Segments: at most 900,000 / 2048 + 1 = 440 are used.
const SEGS: usize = 512;
const SPLIT: u32 = 0x8000_0000;

/// Per-thread arrays of the inverse BWT.
pub(crate) struct Bwt {
    tt: [u32; ROWS],
    m: Meta,
}

/// The walk's bookkeeping. Kept apart from the table and the pool so that
/// the walk can take all three as separate arguments: LLVM then knows the
/// pool's byte stores do not change them.
struct Meta {
    chunk_next: [u32; CHUNKS],
    seg_row: [u32; SEGS],
    seg_start: [u32; SEGS],
    seg_len: [u32; SEGS],
    seg_next: [u32; SEGS],
    n_seg: u32,
    next_chunk: u32,
    next_split: u32,
}

/// Inverts the BWT `ll[0..n)` with origin row `orig_ptr < n` into
/// `text[0..n)`. `ll` is used as the walk's pool, so its contents are lost.
pub(crate) fn inverse(
    b: &mut Bwt,
    ll: &mut [u8; BYTES],
    n: u32,
    orig_ptr: u32,
    text: &mut [u8; BYTES],
) {
    build_t(&mut b.tt, ll, n);
    let lanes = mark_splitters(b, n, orig_ptr);
    let Bwt { tt, m } = b;
    let origin = m.seg_index(orig_ptr);
    m.next_chunk = 0;
    m.next_split = 0;
    match lanes {
        24 => walk::<24>(tt, ll, m),
        8 => walk::<8>(tt, ll, m),
        2 => walk::<2>(tt, ll, m),
        _ => walk::<1>(tt, ll, m),
    }
    gather(m, ll, origin, n, text);
}

/// Builds the table from L, four quarters with their own cursors.
fn build_t(tt: &mut [u32; ROWS], ll: &[u8; BYTES], n: u32) {
    let n = (n as usize).min(ROWS);
    let q = n / 4;
    let (l0, rest) = ll[..n].split_at(q);
    let (l1, rest) = rest.split_at(q);
    let (l2, l3) = rest.split_at(q);

    let mut h = [[0u32; 256]; 4];
    for (((&c0, &c1), &c2), &c3) in l0.iter().zip(l1).zip(l2).zip(l3) {
        h[0][usize::from(c0)] += 1;
        h[1][usize::from(c1)] += 1;
        h[2][usize::from(c2)] += 1;
        h[3][usize::from(c3)] += 1;
    }
    for &c in &l3[q..] {
        h[3][usize::from(c)] += 1;
    }

    // Exclusive prefix sums, split into per-quarter cursors.
    let mut cf = [[0u32; 256]; 4];
    let mut sum = 0u32;
    for c in 0..256 {
        for k in 0..4 {
            cf[k][c] = sum;
            sum += h[k][c];
        }
    }

    // All four cursors are loaded before any entry is stored, and each entry
    // is stored before its cursor (the clang order of dec_block.c).
    let q32 = q as u32;
    for (i, (((&c0, &c1), &c2), &c3)) in l0.iter().zip(l1).zip(l2).zip(l3).enumerate() {
        let i = i as u32;
        let (c0, c1, c2, c3) = (
            usize::from(c0),
            usize::from(c1),
            usize::from(c2),
            usize::from(c3),
        );
        let (j0, j1, j2, j3) = (cf[0][c0], cf[1][c1], cf[2][c2], cf[3][c3]);
        tt[j0 as usize & ROW_MASK] = i << 8 | c0 as u32;
        cf[0][c0] = j0 + 1;
        tt[j1 as usize & ROW_MASK] = (i + q32) << 8 | c1 as u32;
        cf[1][c1] = j1 + 1;
        tt[j2 as usize & ROW_MASK] = (i + 2 * q32) << 8 | c2 as u32;
        cf[2][c2] = j2 + 1;
        tt[j3 as usize & ROW_MASK] = (i + 3 * q32) << 8 | c3 as u32;
        cf[3][c3] = j3 + 1;
    }
    for (i, &c) in l3.iter().enumerate().skip(q) {
        let c = usize::from(c);
        let j = cf[3][c];
        tt[j as usize & ROW_MASK] = (i as u32 + 3 * q32) << 8 | c as u32;
        cf[3][c] = j + 1;
    }
}

/// Chooses splitter rows (evenly spaced, plus `orig_ptr`), marks them in the
/// table and returns the lane count.
fn mark_splitters(b: &mut Bwt, n: u32, op: u32) -> u32 {
    let want = (n / SEG_ROWS).clamp(1, SEGS as u32 - 2);
    let rows = &mut b.m.seg_row;
    let mut m = 0usize;
    let mut inserted = false;
    let mut push = |r: u32| {
        if m == 0 || rows[(m - 1) % SEGS] != r {
            rows[m % SEGS] = r;
            m += 1;
        }
    };
    for k in 0..want {
        let r = (u64::from(k) * u64::from(n) / u64::from(want)) as u32;
        if !inserted && op <= r {
            push(op);
            inserted = true;
        }
        push(r);
    }
    if !inserted {
        push(op);
    }
    b.m.n_seg = m as u32;
    for &r in &b.m.seg_row[..m.min(SEGS)] {
        b.tt[r as usize & ROW_MASK] |= SPLIT;
    }
    if n >= 65536 {
        24
    } else if n >= 16384 {
        8
    } else if m >= 2 {
        2
    } else {
        1
    }
}

impl Meta {
    /// Index of `row` in the sorted splitter list (`row` is a splitter).
    fn seg_index(&self, row: u32) -> u32 {
        let n = (self.n_seg as usize).min(SEGS);
        self.seg_row[..n].partition_point(|&r| r < row) as u32
    }

    /// Gives the lane a fresh chunk, linked after its current one.
    fn new_chunk(&mut self, ln: &mut Lane) {
        let c = self.next_chunk;
        self.next_chunk += 1;
        if ln.end != 0 {
            let prev = ln.end as usize / CHUNK - 1;
            self.chunk_next[prev % CHUNKS] = c;
            ln.written += CHUNK as u32;
        }
        self.chunk_next[c as usize % CHUNKS] = 0;
        ln.out = (c % CHUNKS as u32) * CHUNK as u32;
        ln.end = ln.out + CHUNK as u32;
    }

    /// Starts the next unwalked segment on the lane: emits the splitter
    /// row's byte and moves to its successor. False if none is left.
    fn seg_begin(&mut self, tt: &[u32; ROWS], pool: &mut [u8; BYTES], ln: &mut Lane) -> bool {
        if self.next_split >= self.n_seg {
            return false;
        }
        let seg = self.next_split;
        self.next_split += 1;
        if ln.out == ln.end {
            self.new_chunk(ln);
        }
        let s = seg as usize % SEGS;
        self.seg_start[s] = ln.out;
        ln.seg = seg;
        ln.mark = ln.count();
        let e = tt[self.seg_row[s] as usize & ROW_MASK];
        pool[ln.out as usize & ROW_MASK] = e as u8;
        ln.out += 1;
        ln.p = (e >> 8) & ROW_MASK as u32;
        true
    }
}

#[derive(Clone, Copy, Default)]
struct Lane {
    /// Current row.
    p: u32,
    /// Write position and end of the current chunk, as pool offsets; `end`
    /// is 0 before the lane has a chunk.
    out: u32,
    end: u32,
    /// Bytes written before the current chunk.
    written: u32,
    /// Segment being walked, and the lane's byte count at its start.
    seg: u32,
    mark: u32,
}

impl Lane {
    fn count(&self) -> u32 {
        self.written + (CHUNK as u32 - (self.end - self.out))
    }
}

/// One step of a lane: emits the current row's byte and follows the table.
/// The rare cases (a marked row, a full chunk) go out of line. False when
/// the lane has nothing left to walk.
#[inline(always)]
fn step(tt: &[u32; ROWS], pool: &mut [u8; BYTES], m: &mut Meta, ln: &mut Lane) -> bool {
    let e = tt[ln.p as usize & ROW_MASK];
    if e & SPLIT != 0 || ln.out == ln.end {
        return step_slow(tt, pool, m, ln);
    }
    pool[ln.out as usize & ROW_MASK] = e as u8;
    ln.out += 1;
    ln.p = (e >> 8) & ROW_MASK as u32;
    true
}

#[cold]
#[inline(never)]
fn step_slow(tt: &[u32; ROWS], pool: &mut [u8; BYTES], m: &mut Meta, ln: &mut Lane) -> bool {
    let e = tt[ln.p as usize & ROW_MASK];
    if e & SPLIT != 0 {
        // End of the segment: record it and take the next one.
        let s = ln.seg as usize % SEGS;
        m.seg_len[s] = ln.count() - ln.mark;
        m.seg_next[s] = m.seg_index(ln.p);
        return m.seg_begin(tt, pool, ln);
    }
    m.new_chunk(ln);
    pool[ln.out as usize & ROW_MASK] = e as u8;
    ln.out += 1;
    ln.p = (e >> 8) & ROW_MASK as u32;
    true
}

/// Walks every segment with `K` lanes in lock step, then finishes the
/// stragglers.
#[inline(never)]
fn walk<const K: usize>(tt: &[u32; ROWS], pool: &mut [u8; BYTES], m: &mut Meta) {
    let mut ln = [Lane::default(); K];
    let mut active = 0;
    while active < K {
        let mut l = Lane::default();
        m.new_chunk(&mut l);
        l.written = 0;
        if !m.seg_begin(tt, pool, &mut l) {
            break;
        }
        ln[active] = l;
        active += 1;
    }
    if active == K {
        let mut dead = 0u32;
        while dead == 0 {
            for (k, l) in ln.iter_mut().enumerate() {
                dead |= u32::from(!step(tt, pool, m, l)) << k;
            }
        }
        let mut a = 0;
        for k in 0..K {
            if dead >> k & 1 == 0 {
                ln[a] = ln[k];
                a += 1;
            }
        }
        active = a;
    }
    let mut k = 0;
    while active > 0 {
        if k >= active {
            k = 0;
        }
        if step(tt, pool, m, &mut ln[k]) {
            k += 1;
        } else {
            active -= 1;
            ln[k] = ln[active];
        }
    }
}

/// Copies the segments into `text` in text order, starting with the one at
/// `orig_ptr` and wrapping round the cycle until `n` bytes are out.
fn gather(b: &Meta, pool: &[u8; BYTES], origin: u32, n: u32, text: &mut [u8; BYTES]) {
    let n = (n as usize).min(ROWS);
    let mut seg = origin as usize % SEGS;
    let mut t = 0usize;
    while t < n {
        let mut left = (b.seg_len[seg] as usize).min(n - t);
        if left == 0 {
            break; // unreachable: every segment holds its splitter's byte
        }
        let mut off = b.seg_start[seg] as usize & ROW_MASK;
        loop {
            let room = CHUNK - off % CHUNK;
            let k = room.min(left);
            if let (Some(dst), Some(src)) = (text.get_mut(t..t + k), pool.get(off..off + k)) {
                dst.copy_from_slice(src);
            }
            t += k;
            left -= k;
            if left == 0 {
                break;
            }
            off = (b.chunk_next[off / CHUNK % CHUNKS] as usize % CHUNKS) * CHUNK;
        }
        seg = b.seg_next[seg] as usize % SEGS;
    }
}
