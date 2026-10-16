//! Stream and block headers, the symbol loop (Huffman, inverse MTF and
//! RUNA/RUNB) and the per-block driver (`decompress.c` decode(),
//! `kern_template.h` bzng_symbols).

use fearless_simd::{Select, Simd, SimdBase, SimdFrom, u8x16};

use crate::bits::Bits;
use crate::bwt::{self, BYTES, Bwt, ROW_MASK};
use crate::error::{self, Fail};
use crate::huffman::{HTable, MAX_ALPHA};
use crate::unrle;
use crate::{Corruption, Error};

/// libbz2 1.0.8 keeps this many selectors and reads and ignores the rest.
const MAX_SELECTORS: usize = 18002;
/// Symbols per Huffman group.
const G_SIZE: usize = 50;

/// Allocates a zeroed `Box<[$t; $n]>` without a copy. `vec!` of a zero
/// literal of a concrete integer type is `alloc_zeroed`, so a thread makes
/// resident only the pages its blocks touch.
macro_rules! zeroed_box {
    ($t:ty, $n:expr) => {
        <Box<[$t; $n]>>::try_from(vec![0 as $t; $n].into_boxed_slice())
            // The length is $n by construction, so this arm is unreachable; it
            // only has to be sound, and clippy::panic forbids the alternative.
            .unwrap_or_else(|_| Box::new([0; $n]))
    };
}
pub(crate) use zeroed_box;

/// Per-thread working memory, reused between calls. Every byte that is read
/// is written first in each block, so it is never cleared.
pub(crate) struct Scratch {
    bwt: Bwt,
    /// The symbol loop's output L, then the walk's pool.
    ll: Box<[u8; BYTES]>,
    /// The block's text, gathered in order.
    text: Box<[u8; BYTES]>,
    small: Small,
}

/// The parts that are small enough to build on the stack.
struct Small {
    tables: [HTable; 8],
    selectors: [u8; 1 << 15],
    lens: [[u8; MAX_ALPHA]; 6],
    /// The MTF list; entries 16.. are the tail behind the vector head.
    mtf: [u8; 272],
}

impl Scratch {
    pub(crate) fn new_boxed() -> Box<Scratch> {
        Box::new(Scratch {
            bwt: Bwt::new(),
            ll: zeroed_box!(u8, BYTES),
            text: zeroed_box!(u8, BYTES),
            small: Small {
                tables: [HTable::ZERO; 8],
                selectors: [0; 1 << 15],
                lens: [[0; MAX_ALPHA]; 6],
                mtf: [0; 272],
            },
        })
    }
}

/// The failure for a problem at the reader's current position: a stream that
/// has run out of input is truncated rather than `why`. Inlined, so the
/// reader's address does not escape.
#[inline(always)]
fn fail(br: &Bits, why: Fail) -> Fail {
    if br.overrun() { Fail::Eof } else { why }
}

#[inline(always)]
fn corrupt(br: &Bits, kind: Corruption) -> Fail {
    fail(br, Fail::Corrupt(kind))
}

fn expect(br: &mut Bits, bytes: &[u8], why: Fail) -> Result<(), Fail> {
    for &b in bytes {
        if br.get(8) != u32::from(b) {
            return Err(fail(br, why));
        }
    }
    Ok(())
}

/// A stream being decoded: what its header fixed, and its blocks so far.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stream {
    /// The most bytes of text a block may hold.
    nmax: u32,
    /// 0-based index of the next block.
    block: u32,
    /// The stream CRC combined from the blocks so far.
    combined: u32,
    /// Output bytes per 16 text bytes of the last block, a guess at the
    /// next one's.
    expand: usize,
}

/// A block decoded as far as its text, which is in `Scratch::text`.
#[derive(Clone, Copy)]
pub(crate) struct Text {
    len: u32,
    crc: u32,
}

impl Text {
    /// The most bytes the text can decode to: at most one text byte in five
    /// is a count, and four equal bytes and their count decode to 4 + 255.
    fn max_output(self) -> usize {
        (self.len as usize).div_ceil(5) * 259
    }
}

/// The public error for `f`, from scalars read off the reader on the cold
/// path.
#[inline(always)]
fn raise(f: Fail, block: u32, br: &Bits, capacity: usize) -> Error {
    error::raise(f, block, br.consumed(), br.len_bits(), capacity)
}

impl Stream {
    /// Reads the stream header at the reader's position.
    #[inline(always)]
    pub(crate) fn start(br: &mut Bits) -> Result<Stream, Error> {
        if let Err(f) = expect(br, b"BZh", Fail::NotBzip2) {
            return Err(raise(f, 0, br, 0));
        }
        let digit = br.get(8).wrapping_sub(u32::from(b'0'));
        if !(1..=9).contains(&digit) {
            return Err(raise(fail(br, Fail::NotBzip2), 0, br, 0));
        }
        Ok(Stream {
            nmax: 100_000 * digit,
            block: 0,
            combined: 0,
            expand: 20,
        })
    }

    /// Decodes the next block as far as its text, into `sc`. At the
    /// end-of-stream marker instead checks the combined CRC and returns
    /// `None`, with the reader just past the CRC; the rest of that byte is
    /// padding.
    #[inline(always)]
    pub(crate) fn next<S: Simd>(
        &mut self,
        simd: S,
        br: &mut Bits,
        sc: &mut Scratch,
    ) -> Result<Option<Text>, Error> {
        let block = self.block;
        macro_rules! check {
            ($r:expr) => {
                match $r {
                    Ok(v) => v,
                    Err(f) => return Err(raise(f, block, br, 0)),
                }
            };
        }
        let b = br.get(8);
        if b == 0x17 {
            check!(expect(
                br,
                &[0x72, 0x45, 0x38, 0x50, 0x90],
                Fail::Corrupt(Corruption::BlockMagic)
            ));
            let stored = br.get(32);
            if br.overrun() {
                return Err(raise(Fail::Eof, block, br, 0));
            }
            return if stored == self.combined {
                Ok(None)
            } else {
                Err(Error::StreamCrc {
                    stored,
                    computed: self.combined,
                })
            };
        }
        if b != 0x31 {
            check!(Err(corrupt(br, Corruption::BlockMagic)));
        }
        check!(expect(
            br,
            &[0x41, 0x59, 0x26, 0x53, 0x59],
            Fail::Corrupt(Corruption::BlockMagic)
        ));
        let crc = br.get(32);
        let randomised = br.get(1) != 0;
        let orig_ptr = br.get(24);
        if orig_ptr > 10 + self.nmax {
            check!(Err(corrupt(br, Corruption::OrigPtr)));
        }
        let (eob, n_selectors) = check!(block_tables(simd, br, sc));
        let n = check!(symbols(simd, br, sc, eob, n_selectors, self.nmax));
        if orig_ptr >= n {
            check!(Err(Fail::Corrupt(Corruption::OrigPtr)));
        }

        bwt::inverse(&mut sc.bwt, &mut sc.ll, n, orig_ptr, &mut sc.text);
        // Cannot fail: `symbols` bounds `n` by `nmax`, which is within `BYTES`.
        let text = check!(
            sc.text
                .get_mut(..n as usize)
                .ok_or(Fail::Corrupt(Corruption::BlockTooLong))
        );
        if randomised {
            unrle::derandomise(text);
        }
        Ok(Some(Text { len: n, crc }))
    }

    /// Un-RLE1s `text`, the block [`Stream::next`] last returned, into
    /// `out[o..]` and checks its CRC. Returns the new output position. Only
    /// a success changes the stream, so a caller may retry with a larger
    /// `out`.
    #[inline(always)]
    pub(crate) fn emit<S: Simd>(
        &mut self,
        simd: S,
        br: &Bits,
        sc: &Scratch,
        text: Text,
        out: &mut [u8],
        o: usize,
    ) -> Result<usize, Error> {
        let t = sc.text.get(..text.len as usize).unwrap_or_default();
        let o2 = match unrle::unrle(simd, t, out, o) {
            Ok(o2) => o2,
            Err(f) => return Err(raise(f, self.block, br, out.len())),
        };
        let crc = !crate::crc::update(!0, out.get(o..o2).unwrap_or_default());
        if crc != text.crc {
            return Err(Error::BlockCrc {
                block: self.block,
                stored: text.crc,
                computed: crc,
            });
        }
        self.combined = self.combined.rotate_left(1) ^ crc;
        self.block = self.block.saturating_add(1);
        Ok(o2)
    }

    /// [`Stream::emit`], growing `out` (zero-filled, up to `cap` bytes) when
    /// the block needs more room. Leaves `out` at least as long as the
    /// returned position.
    #[allow(clippy::too_many_arguments, reason = "inlined into its two callers")]
    #[inline(always)]
    fn emit_growing<S: Simd>(
        &mut self,
        simd: S,
        br: &Bits,
        sc: &Scratch,
        text: Text,
        out: &mut Vec<u8>,
        o: usize,
        cap: usize,
    ) -> Result<usize, Error> {
        // Blocks of a stream tend to expand alike, so the room is what the
        // last block's ratio gives, and an eighth more. A block that needs more is
        // un-RLE1ed again, into twice the room each time, up to the most it
        // can need.
        let most = o.saturating_add(text.max_output()).min(cap);
        let len = text.len as usize;
        let guess = len.saturating_mul(self.expand) / 16;
        let mut want = o.saturating_add(guess + guess / 8 + 64).min(most);
        loop {
            if out.len() < want {
                out.resize(want, 0);
            }
            match self.emit(simd, br, sc, text, out, o) {
                Err(Error::OutputTooSmall { .. }) if want < most => {
                    want = o.saturating_add((want - o).saturating_mul(2)).min(most);
                }
                Ok(o2) => {
                    self.expand = (o2 - o) * 16 / len.max(1);
                    return Ok(o2);
                }
                r => return r,
            }
        }
    }
}

/// Decodes the stream at the start of `src` into `out`. Returns the bytes
/// written and the bytes of `src` the stream takes up.
#[fearless_simd_macros::simd]
pub(crate) fn decode_stream<S: Simd>(
    simd: S,
    src: &[u8],
    out: &mut [u8],
    sc: &mut Scratch,
) -> Result<(usize, usize), Error> {
    let mut br = Bits::new(src);
    let mut st = Stream::start(&mut br)?;
    let mut o = 0usize;
    while let Some(text) = st.next(simd, &mut br, sc)? {
        o = st.emit(simd, &br, sc, text, out, o)?;
    }
    Ok((o, br.consumed().div_ceil(8) as usize))
}

/// Decodes the streams that make up all of `src`, back to back, appending
/// their bytes to `out`, which may grow by at most `limit`. Returns the
/// bytes appended. On error, `out` is cut back to its length before the call.
#[fearless_simd_macros::simd]
pub(crate) fn decode_all<S: Simd>(
    simd: S,
    src: &[u8],
    out: &mut Vec<u8>,
    limit: usize,
    sc: &mut Scratch,
) -> Result<usize, Error> {
    let start = out.len();
    let cap = start.saturating_add(limit);
    let mut o = start;
    let mut at = 0u64;
    let r = 'streams: loop {
        let mut br = Bits::at(src, at);
        let mut st = match Stream::start(&mut br) {
            Ok(st) => st,
            Err(e) => break Err(e),
        };
        loop {
            match st.next(simd, &mut br, sc) {
                Ok(Some(text)) => match st.emit_growing(simd, &br, sc, text, out, o, cap) {
                    Ok(o2) => o = o2,
                    Err(Error::OutputTooSmall { .. }) => {
                        break 'streams Err(Error::OutputTooSmall { capacity: limit });
                    }
                    Err(e) => break 'streams Err(e),
                },
                Ok(None) => break,
                Err(e) => break 'streams Err(e),
            }
        }
        at = br.consumed().next_multiple_of(8);
        if at >= br.len_bits() {
            break Ok(o - start);
        }
    };
    out.truncate(if r.is_ok() { o } else { start });
    r
}

/// The next block of a stream read piece by piece, un-RLE1ed into
/// `out[..n]` (grown as needed, never shrunk), as `Some(n)`; or `None` at
/// the end-of-stream marker.
#[fearless_simd_macros::simd]
pub(crate) fn next_block<S: Simd>(
    simd: S,
    st: &mut Stream,
    br: &mut Bits,
    sc: &mut Scratch,
    out: &mut Vec<u8>,
) -> Result<Option<usize>, Error> {
    match st.next(simd, br, sc)? {
        Some(text) => st
            .emit_growing(simd, br, sc, text, out, 0, usize::MAX)
            .map(Some),
        None => Ok(None),
    }
}

/// Reads the symbol map, selectors and code lengths of a block, builds its
/// Huffman tables and the MTF list. Returns the EOB symbol and the number of
/// selectors kept.
#[inline(always)]
fn block_tables<S: Simd>(simd: S, br: &mut Bits, sc: &mut Scratch) -> Result<(u32, usize), Fail> {
    let in_use16 = br.get(16);
    let mut n_in_use = 0usize;
    for i in 0..16 {
        if in_use16 & (0x8000 >> i) != 0 {
            let bits = br.get(16);
            for j in 0..16 {
                if bits & (0x8000 >> j) != 0 {
                    sc.small.mtf[n_in_use % 256] = (i * 16 + j) as u8;
                    n_in_use += 1;
                }
            }
        }
    }
    if n_in_use == 0 {
        return Err(corrupt(br, Corruption::NoSymbols));
    }
    let alpha_size = n_in_use + 2;

    let n_groups = br.get(3) as usize;
    if !(2..=6).contains(&n_groups) {
        return Err(corrupt(br, Corruption::GroupCount));
    }
    let n_selectors = br.get(15) as usize;
    if n_selectors < 1 {
        return Err(corrupt(br, Corruption::SelectorCount));
    }
    // Selector MTF values are undone as they arrive. A unary code is a count
    // of leading ones.
    let mut pos = [0u8, 1, 2, 3, 4, 5];
    for i in 0..n_selectors {
        if br.cnt < 8 {
            br.refill();
        }
        let j = (!br.buf).leading_zeros() as usize;
        if j >= n_groups {
            br.consume(n_groups as u32);
            return Err(corrupt(br, Corruption::Selector));
        }
        br.consume(j as u32 + 1);
        if i < MAX_SELECTORS {
            let v = pos[j % 6];
            pos.copy_within(0..j % 6, 1);
            pos[0] = v;
            sc.small.selectors[i % (1 << 15)] = v;
        }
    }
    let n_selectors = n_selectors.min(MAX_SELECTORS);

    // Code lengths: a 5-bit start, then per symbol steps of 10 (+1) or 11
    // (-1) ended by 0, the range checked before every step.
    for t in 0..n_groups {
        let mut curr = br.get(5) as i32;
        for i in 0..alpha_size {
            loop {
                if !(1..=20).contains(&curr) {
                    return Err(corrupt(br, Corruption::CodeLength));
                }
                if br.cnt < 2 {
                    br.refill();
                }
                let two = br.buf >> 62;
                if two < 2 {
                    br.consume(1);
                    break;
                }
                br.consume(2);
                curr += if two == 2 { 1 } else { -1 };
            }
            sc.small.lens[t % 6][i % MAX_ALPHA] = curr as u8;
        }
    }
    for t in 0..n_groups {
        sc.small.tables[t % 8].build(simd, &sc.small.lens[t % 6][..alpha_size.min(MAX_ALPHA)]);
    }
    Ok((n_in_use as u32 + 1, n_selectors))
}

/// `PERM[k]` moves lane `k` to the front: `[k, 0, 1, .., k-1, k+1, .., 15]`.
static PERM: [[u8; 16]; 16] = {
    let mut p = [[0u8; 16]; 16];
    let mut k = 0;
    while k < 16 {
        p[k][0] = k as u8;
        let mut i = 1;
        while i < 16 {
            p[k][i] = if i <= k { i as u8 - 1 } else { i as u8 };
            i += 1;
        }
        k += 1;
    }
    p
};

/// Symbol-loop state that stays in registers.
struct Run<S: Simd> {
    /// Symbols written to L.
    nb: u32,
    /// Pending run length (bijective base 2) and its next digit's weight.
    run: u32,
    rshift: u32,
    /// MTF entries 0..16; the rest are in `Scratch::mtf[16..]`.
    head: u8x16<S>,
    /// All zeros, as indices that broadcast lane 0. Opaque to LLVM (see
    /// `step`).
    zero: u8x16<S>,
}

const CONT: u32 = 0;
const EOB: u32 = 1;
const ERR: u32 = 2;

/// Huffman decoding, inverse MTF and RUNA/RUNB of one block into `sc.ll`.
/// Returns the block's length.
#[inline(always)]
fn symbols<S: Simd>(
    simd: S,
    br: &mut Bits,
    sc: &mut Scratch,
    eob: u32,
    n_selectors: usize,
    nmax: u32,
) -> Result<u32, Fail> {
    let Scratch { ll, small, .. } = sc;
    let Small {
        tables,
        selectors,
        mtf,
        ..
    } = small;
    let head = u8x16::simd_from(simd, *mtf.first_chunk::<16>().unwrap_or(&[0; 16]));
    let mut st = Run {
        nb: 0,
        run: 0,
        rshift: 0,
        head,
        zero: core::hint::black_box(u8x16::splat(simd, 0)),
    };
    let mut syms = [0u16; 64];
    for g in 0..n_selectors {
        let t = &tables[usize::from(selectors[g % (1 << 15)]) % 8];
        let start = br.mark();
        // Decode up to 50 codes, two per refill (a refill leaves at least 56
        // bits and a code has at most 20), stopping after EOB.
        let mut k = 0usize;
        let mut bad = false;
        'group: while k < G_SIZE {
            br.refill();
            for _ in 0..2 {
                let e = t.decode(br.buf);
                let l = e & 31;
                if l == 0 {
                    bad = true;
                    break 'group;
                }
                br.consume(l);
                syms[k % 64] = (e >> 5) as u16;
                k += 1;
                if e >> 5 == eob {
                    break 'group;
                }
            }
        }
        for (i, &sym) in syms[..k.min(64)].iter().enumerate() {
            match step(simd, &mut st, u32::from(sym), ll, mtf, eob, nmax) {
                CONT => {}
                EOB => {
                    return if br.overrun() {
                        Err(Fail::Eof)
                    } else {
                        Ok(st.nb)
                    };
                }
                _ => {
                    // libbz2 stops right after this symbol: truncated only if
                    // its bits are not all in the input. Rewinding to it also
                    // gives the error its position.
                    br.reset(start);
                    for _ in 0..=i {
                        br.refill();
                        br.consume(t.decode(br.buf) & 31);
                    }
                    let kind = if sym < 2 && st.rshift >= 21 {
                        Corruption::RunTooLong
                    } else {
                        Corruption::BlockTooLong
                    };
                    return Err(corrupt(br, kind));
                }
            }
        }
        if bad {
            // libbz2 rejects a pattern once 21 of its bits are in.
            return Err(if br.overrun_by(21) {
                Fail::Eof
            } else {
                Fail::Corrupt(Corruption::HuffmanCode)
            });
        }
    }
    Err(corrupt(br, Corruption::NoEndOfBlock))
}

/// One symbol (`kern_template.h` SYMBOL_STEP): run and literal symbols take
/// the same branch-free path. A pending run is flushed with 16-byte stores
/// of the front byte (length 0 for a run symbol), the literal is moved to the
/// front (index 0, a no-op, for a run symbol) and stored, and `nb` advances
/// only for a literal. Stores past `nb` land in L's slack or are overwritten.
#[inline(always)]
fn step<S: Simd>(
    simd: S,
    st: &mut Run<S>,
    sym: u32,
    ll: &mut [u8; BYTES],
    mtf: &mut [u8; 272],
    eob: u32,
    nmax: u32,
) -> u32 {
    if sym == eob {
        if st.nb + st.run > nmax {
            return ERR;
        }
        let fv = st.head.swizzle_dyn(st.zero);
        let mut j = 0;
        while j < st.run as usize {
            store16(ll, st.nb as usize + j, fv);
            j += 16;
        }
        st.nb += st.run;
        return EOB;
    }
    let is_run = sym < 2;
    let flush = if is_run { 0 } else { st.run };
    // libbz2's checks: a run length of 2^21 or more, a block past nmax.
    if (is_run & (st.rshift >= 21)) | (st.nb + flush + u32::from(!is_run) > nmax) {
        return ERR;
    }
    // The front byte stays in the vector domain throughout. Taking it out as
    // a scalar (or a broadcast LLVM can see is one) lets LLVM split the head
    // into a vector and a scalar lane 0, which puts two more operations on
    // the MTF dependency chain; hence the opaque zero indices.
    let fv = st.head.swizzle_dyn(st.zero);
    store16(ll, st.nb as usize, fv);
    if flush > 16 {
        let mut j = 16;
        while j < flush as usize {
            store16(ll, st.nb as usize + j, fv);
            j += 16;
        }
    }
    st.nb += flush;
    let idx = if is_run { 0 } else { sym - 1 } as usize;
    if idx < 16 {
        st.head = st.head.swizzle_dyn(u8x16::simd_from(simd, PERM[idx]));
    } else {
        st.head = tail_move(simd, mtf, st.head, idx & 255);
    }
    // The new head's first byte is the literal; the other 15 are overwritten
    // later or land in L's slack.
    store16(ll, st.nb as usize, st.head);
    st.nb += u32::from(!is_run);
    // The run/literal pattern is close to random, and on x86-64 LLVM turns
    // plain ternaries here into a branch on it.
    st.run = core::hint::select_unpredictable(
        is_run,
        st.run.wrapping_add((sym + 1).wrapping_shl(st.rshift)),
        0,
    );
    st.rshift = core::hint::select_unpredictable(is_run, st.rshift + 1, 0);
    CONT
}

/// Moves MTF entry `idx` (16..=255) to the front of a list whose first 16
/// entries are `head` and the rest `mtf[16..]` (`kern_template.h`
/// mtf_tail_move): the tail shifts up one entry, 16 at a time, each vector
/// taking the previous one's last byte, the last one merged up to `idx`.
/// Returns the new head.
#[inline(always)]
fn tail_move<S: Simd>(simd: S, mtf: &mut [u8; 272], head: u8x16<S>, idx: usize) -> u8x16<S> {
    let c = mtf[idx];
    let mut prev = head;
    let mut j = 16;
    while j + 16 <= idx {
        let v = load16(simd, mtf, j);
        prev.slide::<15>(v).store_slice(&mut mtf[j..j + 16]);
        prev = v;
        j += 16;
    }
    let v = load16(simd, mtf, j);
    let keep = u8x16::simd_from(simd, IOTA).simd_le(u8x16::splat(simd, (idx - j) as u8));
    keep.select(prev.slide::<15>(v), v)
        .store_slice(&mut mtf[j..j + 16]);
    // black_box: `c` is a scalar, and an LLVM that sees lane 0 as one splits
    // the head into a vector and a scalar lane on the hot path too.
    core::hint::black_box(u8x16::splat(simd, c).slide::<15>(head))
}

/// `mtf[j..j + 16]` (`j <= 255`, and the list has 16 bytes of slack).
#[inline(always)]
fn load16<S: Simd>(simd: S, mtf: &[u8; 272], j: usize) -> u8x16<S> {
    u8x16::simd_from(simd, *mtf[j..].first_chunk::<16>().unwrap_or(&[0; 16]))
}

const IOTA: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Stores 16 bytes at `ll[at & ROW_MASK..]` (in range: `BYTES` has slack).
#[inline(always)]
fn store16<S: Simd>(ll: &mut [u8; BYTES], at: usize, v: u8x16<S>) {
    let at = at & ROW_MASK;
    v.store_slice(&mut ll[at..at + 16]);
}

#[cfg(test)]
mod tests {
    use fearless_simd::{SimdBase, SimdFrom, dispatch, u8x16};

    /// `PERM[k]` as a shuffle moves lane `k` to the front, for every `k`, at
    /// every level; `slide::<15>` shifts a byte in at the front.
    #[test]
    fn perm_moves_lane_to_front_and_slide_shifts_in() {
        for level in crate::tests::levels() {
            dispatch!(level, simd => {
                let list: [u8; 16] = core::array::from_fn(|i| 100 + i as u8);
                let v = u8x16::simd_from(simd, list);
                for k in 0..16 {
                    let mut want = list.to_vec();
                    let moved = want.remove(k);
                    want.insert(0, moved);
                    let m: [u8; 16] = v.swizzle_dyn(u8x16::simd_from(simd, super::PERM[k])).into();
                    assert_eq!(m[..], want[..], "{level:?} k={k}");
                }
                let s: [u8; 16] = u8x16::splat(simd, 7).slide::<15>(v).into();
                assert_eq!(s[0], 7, "{level:?}");
                assert_eq!(s[1..], list[..15], "{level:?}");
            });
        }
    }

    /// The vector tail move against a plain move-to-front of the whole list,
    /// for every index past the head, at every level.
    #[test]
    fn tail_move_is_move_to_front() {
        for level in crate::tests::levels() {
            for idx in 16..=255usize {
                let list: [u8; 256] =
                    core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(5));
                let mut want = list;
                want.copy_within(0..idx, 1);
                want[0] = list[idx];
                let mut mtf = [0xEEu8; 272];
                mtf[..256].copy_from_slice(&list);
                let head: [u8; 16] = dispatch!(level, simd => {
                    let head = u8x16::simd_from(simd, *list.first_chunk::<16>().unwrap_or(&[0; 16]));
                    super::tail_move(simd, &mut mtf, head, idx).into()
                });
                assert_eq!(head[..], want[..16], "{level:?} idx {idx}");
                assert_eq!(mtf[16..256], want[16..], "{level:?} idx {idx}");
                assert!(
                    mtf[256..].iter().all(|&b| b == 0xEE),
                    "{level:?} idx {idx}: slack written"
                );
            }
        }
    }
}
