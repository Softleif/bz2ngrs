//! Stream and block headers, the symbol loop (Huffman, inverse MTF and
//! RUNA/RUNB) and the per-block driver (`decompress.c` decode(),
//! `kern_template.h` bzng_symbols).

use fearless_simd::{Simd, SimdBase, SimdFrom, u8x16};

use crate::Error;
use crate::bits::Bits;
use crate::bwt::{self, BYTES, Bwt, ROW_MASK};
use crate::huffman::{HTable, MAX_ALPHA};
use crate::unrle;

/// libbz2 1.0.8 keeps this many selectors and reads and ignores the rest.
const MAX_SELECTORS: usize = 18002;
/// Symbols per Huffman group.
const G_SIZE: usize = 50;

/// Per-thread working memory, reused between calls. Every byte that is read
/// is written first in each block, so it is never cleared.
pub(crate) struct Scratch {
    bwt: Bwt,
    /// The symbol loop's output L, then the walk's pool.
    ll: [u8; BYTES],
    /// The block's text, gathered in order.
    text: [u8; BYTES],
    tables: [HTable; 8],
    selectors: [u8; 1 << 15],
    lens: [[u8; MAX_ALPHA]; 6],
    /// The MTF list; entries 16.. are the tail behind the vector head.
    mtf: [u8; 256],
}

impl Scratch {
    #[allow(unsafe_code, reason = "lazily zeroed boxed scratch via new_zeroed")]
    pub(crate) fn new_boxed() -> Box<Scratch> {
        // SAFETY: Scratch holds only integers and arrays of integers (and
        // structs of those), for which all-zero bytes are a valid value.
        // alloc_zeroed leaves the large arrays as untouched zero pages, so a
        // thread only makes resident what its blocks use.
        unsafe { Box::<Scratch>::new_zeroed().assume_init() }
    }
}

/// The error for a failure at the reader's current position: a stream that
/// has run out of input is truncated rather than corrupt. Inlined, so the
/// reader's address does not escape.
#[inline(always)]
fn fail(br: &Bits) -> Error {
    if br.overrun() {
        Error::UnexpectedEof
    } else {
        Error::Corrupt
    }
}

fn expect(br: &mut Bits, bytes: &[u8]) -> Result<(), Error> {
    for &b in bytes {
        if br.get(8) != u32::from(b) {
            return Err(fail(br));
        }
    }
    Ok(())
}

/// Decodes the stream at the start of `src` into `out`.
#[fearless_simd_macros::simd]
pub(crate) fn decode_stream<S: Simd>(
    simd: S,
    src: &[u8],
    out: &mut [u8],
    sc: &mut Scratch,
) -> Result<usize, Error> {
    let mut br = Bits::new(src);
    expect(&mut br, b"BZh")?;
    let digit = br.get(8).wrapping_sub(u32::from(b'0'));
    if !(1..=9).contains(&digit) {
        return Err(fail(&br));
    }
    let nmax = 100_000 * digit;
    let mut o = 0usize;
    let mut combined = 0u32;
    loop {
        let b = br.get(8);
        if b == 0x17 {
            expect(&mut br, &[0x72, 0x45, 0x38, 0x50, 0x90])?;
            let stored = br.get(32);
            if br.overrun() {
                return Err(Error::UnexpectedEof);
            }
            return if stored == combined {
                Ok(o)
            } else {
                Err(Error::CrcMismatch)
            };
        }
        if b != 0x31 {
            return Err(fail(&br));
        }
        expect(&mut br, &[0x41, 0x59, 0x26, 0x53, 0x59])?;
        let stored_crc = br.get(32);
        let randomised = br.get(1) != 0;
        let orig_ptr = br.get(24);
        if orig_ptr > 10 + nmax {
            return Err(fail(&br));
        }
        let (eob, n_selectors) = block_tables(simd, &mut br, sc)?;
        let n = symbols(simd, &mut br, sc, eob, n_selectors, nmax)?;
        if orig_ptr >= n {
            return Err(Error::Corrupt);
        }

        bwt::inverse(&mut sc.bwt, &mut sc.ll, n, orig_ptr, &mut sc.text);
        let text = sc.text.get_mut(..n as usize).ok_or(Error::Corrupt)?;
        if randomised {
            unrle::derandomise(text);
        }
        let o2 = unrle::unrle(simd, text, out, o)?;
        let crc = !crate::crc::update(!0, out.get(o..o2).unwrap_or_default());
        if crc != stored_crc {
            return Err(Error::CrcMismatch);
        }
        combined = combined.rotate_left(1) ^ crc;
        o = o2;
    }
}

/// Reads the symbol map, selectors and code lengths of a block, builds its
/// Huffman tables and the MTF list. Returns the EOB symbol and the number of
/// selectors kept.
#[inline(always)]
fn block_tables<S: Simd>(simd: S, br: &mut Bits, sc: &mut Scratch) -> Result<(u32, usize), Error> {
    let in_use16 = br.get(16);
    let mut n_in_use = 0usize;
    for i in 0..16 {
        if in_use16 & (0x8000 >> i) != 0 {
            let bits = br.get(16);
            for j in 0..16 {
                if bits & (0x8000 >> j) != 0 {
                    sc.mtf[n_in_use % 256] = (i * 16 + j) as u8;
                    n_in_use += 1;
                }
            }
        }
    }
    if n_in_use == 0 {
        return Err(fail(br));
    }
    let alpha_size = n_in_use + 2;

    let n_groups = br.get(3) as usize;
    if !(2..=6).contains(&n_groups) {
        return Err(fail(br));
    }
    let n_selectors = br.get(15) as usize;
    if n_selectors < 1 {
        return Err(fail(br));
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
            return Err(fail(br));
        }
        br.consume(j as u32 + 1);
        if i < MAX_SELECTORS {
            let v = pos[j % 6];
            pos.copy_within(0..j % 6, 1);
            pos[0] = v;
            sc.selectors[i % (1 << 15)] = v;
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
                    return Err(fail(br));
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
            sc.lens[t % 6][i % MAX_ALPHA] = curr as u8;
        }
    }
    for t in 0..n_groups {
        sc.tables[t % 8].build(simd, &sc.lens[t % 6][..alpha_size.min(MAX_ALPHA)]);
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
) -> Result<u32, Error> {
    let Scratch {
        ll,
        tables,
        selectors,
        mtf,
        ..
    } = sc;
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
                        Err(Error::UnexpectedEof)
                    } else {
                        Ok(st.nb)
                    };
                }
                _ => {
                    // libbz2 stops right after this symbol: truncated only if
                    // its bits are not all in the input.
                    if br.overrun() {
                        br.reset(start);
                        for _ in 0..=i {
                            br.refill();
                            br.consume(t.decode(br.buf) & 31);
                        }
                    }
                    return Err(fail(br));
                }
            }
        }
        if bad {
            // libbz2 rejects a pattern once 21 of its bits are in.
            return Err(if br.overrun_by(21) {
                Error::UnexpectedEof
            } else {
                Error::Corrupt
            });
        }
    }
    Err(fail(br))
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
    mtf: &mut [u8; 256],
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
        // Rare: move the entry within the whole list in memory.
        let idx = idx & 255;
        st.head.store_slice(&mut mtf[..16]);
        let c = mtf[idx];
        mtf.copy_within(0..idx, 1);
        mtf[0] = c;
        // black_box: otherwise LLVM forwards `c` into lane 0 and splits the
        // head on the hot path too (see above).
        st.head = core::hint::black_box(u8x16::simd_from(
            simd,
            *mtf.first_chunk::<16>().unwrap_or(&[0; 16]),
        ));
    }
    // The new head's first byte is the literal; the other 15 are overwritten
    // later or land in L's slack.
    store16(ll, st.nb as usize, st.head);
    st.nb += u32::from(!is_run);
    st.run = if is_run {
        st.run.wrapping_add((sym + 1).wrapping_shl(st.rshift))
    } else {
        0
    };
    st.rshift = if is_run { st.rshift + 1 } else { 0 };
    CONT
}

/// Stores 16 bytes at `ll[at & ROW_MASK..]` (in range: `BYTES` has slack).
#[inline(always)]
fn store16<S: Simd>(ll: &mut [u8; BYTES], at: usize, v: u8x16<S>) {
    let at = at & ROW_MASK;
    v.store_slice(&mut ll[at..at + 16]);
}

#[cfg(test)]
mod tests {
    use fearless_simd::{Level, SimdBase, SimdFrom, dispatch, u8x16};

    #[test]
    fn perm_moves_lane_to_front_and_slide_shifts_in() {
        dispatch!(Level::new(), simd => {
            let v = u8x16::simd_from(simd, core::array::from_fn::<u8, 16, _>(|i| 100 + i as u8));
            let m: [u8; 16] = v.swizzle_dyn(u8x16::simd_from(simd, super::PERM[5])).into();
            assert_eq!(m[..7], [105, 100, 101, 102, 103, 104, 106]);
            assert_eq!(m[15], 115);
            let s: [u8; 16] = u8x16::splat(simd, 7).slide::<15>(v).into();
            assert_eq!(s[..3], [7, 100, 101]);
            assert_eq!(s[15], 114);
        });
    }
}
