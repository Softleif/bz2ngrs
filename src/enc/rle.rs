//! The encoder's input stage: libbz2's RLE1 with its exact block-boundary
//! behaviour (bz2-ng's `rle_fill`, vector tier, of `kern_template.h`).
//!
//! Input is cut into units, maximal runs of one byte cut at 255. A unit of 1
//! to 3 bytes is stored as is, a longer one as 4 bytes and a count byte. A
//! unit is written to the block (and its bytes added to the block CRC) only
//! when the byte after it arrives; the block is full once a written unit
//! brings it to `nblock_max` or more, and the byte that triggered that write
//! is consumed and stays pending.
//!
//! Done in bulk: every unit followed by another byte of the same call is
//! complete, so whole literal spans and runs are written at once, and the CRC
//! covers one contiguous input range per call plus the pending unit carried
//! in from the previous call (whose bytes are added as copies of its byte).

use fearless_simd::{Simd, SimdBase, SimdFrom, u8x16};

use crate::crc;

/// The fast phase keeps the block this far below `nblock_max`, so that the
/// exact unit-by-unit tail makes every block-full decision.
const GUARD: usize = 8;
/// Calls with fewer input bytes than this take libbz2's byte loop.
const SMALL: usize = 16;
/// Bytes the fast phase examines and copies per step.
const STEP: usize = 16;
/// Bytes the block buffer needs past `nblock_max`: the last unit (5 bytes)
/// written with one 8-byte store.
pub(crate) const SLACK: usize = 16;

/// The RLE1 state carried between calls and blocks.
pub(crate) struct Rle {
    /// Byte of the pending unit, 256 if none.
    pub(crate) ch: u32,
    /// Its length so far (0 if none).
    pub(crate) len: usize,
    /// Bytes in the block.
    pub(crate) nblock: usize,
    /// Running (non-finalised) CRC of the units written.
    pub(crate) crc: u32,
}

impl Rle {
    pub(crate) fn new() -> Rle {
        Rle {
            ch: 256,
            len: 0,
            nblock: 0,
            crc: !0,
        }
    }

    /// Writes the pending unit to the block (libbz2's `flush_RL`).
    pub(crate) fn flush(&mut self, blk: &mut [u8]) {
        if self.ch < 256 {
            let c = self.ch as u8;
            self.crc = crc::repeat(self.crc, c, self.len);
            self.nblock = put_unit(blk, self.nblock, c, self.len);
        }
        self.ch = 256;
        self.len = 0;
    }
}

/// Writes one unit of `len` (1 to 255) copies of `c` at `blk[nb]` with one
/// 8-byte store; returns the new block length.
#[inline(always)]
fn put_unit(blk: &mut [u8], nb: usize, c: u8, len: usize) -> usize {
    let x = if len >= 4 { (len - 4) as u8 } else { c };
    blk[nb..nb + 8].copy_from_slice(&[c, c, c, c, x, 0, 0, 0]);
    nb + if len >= 4 { 5 } else { len }
}

/// `p[i..i + 16]`.
#[inline(always)]
fn load16<S: Simd>(simd: S, p: &[u8], i: usize) -> u8x16<S> {
    u8x16::simd_from(simd, *p[i..].first_chunk::<16>().unwrap_or(&[0; 16]))
}

/// Bit `k` is set iff `p[i + k..i + k + 4]` are equal (k = 0..16); reads
/// `p[i..i + 19]`.
#[inline(always)]
fn run4_mask<S: Simd>(simd: S, p: &[u8], i: usize) -> u64 {
    let (a, b, c, d) = (
        load16(simd, p, i),
        load16(simd, p, i + 1),
        load16(simd, p, i + 2),
        load16(simd, p, i + 3),
    );
    simd.to_bitmask_mask8x16(a.simd_eq(b) & b.simd_eq(c) & c.simd_eq(d))
}

/// Number of bytes equal to `c` at `p[..avail]`.
#[inline(always)]
fn run_length<S: Simd>(simd: S, p: &[u8], avail: usize, c: u8) -> usize {
    let vc = u8x16::splat(simd, c);
    let mut l = 0;
    while l + 16 <= avail {
        let differ = !simd.to_bitmask_mask8x16(load16(simd, p, l).simd_eq(vc)) & 0xffff;
        if differ != 0 {
            return l + differ.trailing_zeros() as usize;
        }
        l += 16;
    }
    while l < avail && p[l] == c {
        l += 1;
    }
    l
}

/// Feeds `input` to the block `blk` until it runs out or the block is full;
/// returns how many bytes it consumed. `blk` holds at least
/// `nblock_max + SLACK` bytes.
#[inline(always)]
pub(crate) fn fill<S: Simd>(
    simd: S,
    st: &mut Rle,
    input: &[u8],
    blk: &mut [u8],
    nblock_max: usize,
) -> usize {
    let avail = input.len();
    if avail == 0 {
        return 0;
    }
    let mut nb = st.nblock;
    let mut crc = st.crc;
    let mut p = 0;

    // Small calls: libbz2's byte loop, which costs less than setting up the
    // bulk scan when there is little to scan.
    if avail < SMALL {
        let (mut ch, mut len) = (st.ch, st.len);
        while p < avail {
            let c = u32::from(input[p]);
            if c != ch && len == 1 {
                crc = crc::repeat(crc, ch as u8, 1);
                blk[nb] = ch as u8;
                nb += 1;
                ch = c;
            } else if c != ch || len == 255 {
                if ch < 256 {
                    crc = crc::repeat(crc, ch as u8, len);
                    nb = put_unit(blk, nb, ch as u8, len);
                }
                ch = c;
                len = 1;
            } else {
                len += 1;
                p += 1;
                continue;
            }
            p += 1;
            if nb >= nblock_max {
                break;
            }
        }
        st.ch = ch;
        st.len = len;
        st.nblock = nb;
        st.crc = crc;
        return p;
    }

    // The unit pending from the previous call: extend it, or write it out
    // once a byte that ends it arrives.
    if st.len > 0 {
        let c = st.ch as u8;
        let room = 255 - st.len;
        p = run_length(simd, input, avail.min(room), c);
        if p == avail {
            st.len += p;
            return avail;
        }
        crc = crc::repeat(crc, c, st.len + p);
        nb = put_unit(blk, nb, c, st.len + p);
        if nb >= nblock_max {
            st.ch = u32::from(input[p]);
            st.len = 1;
            st.nblock = nb;
            st.crc = crc;
            return p + 1;
        }
    }

    // From here every unit starts in this call; input[u..p] is written and
    // not yet in the CRC.
    let u = p;
    let limit = nblock_max.saturating_sub(GUARD);

    // Fast phase: whole literal spans and runs while the block has room for
    // a whole step and the input for the loads. It stops only at a unit
    // boundary or inside a literal unit of at most 3 bytes that ends within
    // this call (which writes the same bytes however it is split).
    'fast: while nb + STEP <= limit && p + 2 * STEP <= avail {
        let m = run4_mask(simd, input, p);
        blk[nb..nb + STEP].copy_from_slice(&input[p..p + STEP]);
        if m == 0 {
            nb += STEP;
            p += STEP;
            continue;
        }
        let j = m.trailing_zeros() as usize;
        nb += j;
        p += j;
        // A maximal run of 4 or more starts at p.
        let c = input[p];
        let mut l = run_length(simd, &input[p..], avail - p, c);
        if p + l == avail {
            // It may continue in the next call.
            break;
        }
        while l >= 255 {
            if nb + 5 > limit {
                break 'fast;
            }
            nb = put_unit(blk, nb, c, 255);
            p += 255;
            l -= 255;
        }
        if l > 0 {
            if nb + 5 > limit {
                break 'fast;
            }
            nb = put_unit(blk, nb, c, l);
            p += l;
        }
    }

    // Exact unit-by-unit tail: libbz2's block-full decision, and the pending
    // unit at the end of the call.
    loop {
        let c = input[p];
        let max = (avail - p).min(255);
        let mut l = 1;
        while l < max && input[p + l] == c {
            l += 1;
        }
        if p + l == avail {
            st.ch = u32::from(c);
            st.len = l;
            st.nblock = nb;
            st.crc = crc::update(crc, &input[u..p]);
            return avail;
        }
        nb = put_unit(blk, nb, c, l);
        p += l;
        if nb >= nblock_max {
            st.ch = u32::from(input[p]);
            st.len = 1;
            st.nblock = nb;
            st.crc = crc::update(crc, &input[u..p]);
            return p + 1;
        }
    }
}
