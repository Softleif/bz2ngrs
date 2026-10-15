//! The encoder's input stage: libbz2's RLE1 with its exact block-boundary
//! behaviour (bz2-ng's `rle_fill`, vector tier, of `kern_template.h`, with
//! the bulk phase done by 64-byte window masks).
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

use fearless_simd::{Simd, SimdBase, SimdFrom, u8x16, u8x64};

use crate::crc;

/// The fast phase keeps the block this far below `nblock_max`, so that the
/// exact unit-by-unit tail makes every block-full decision.
const GUARD: usize = 8;
/// Calls with fewer input bytes than this take libbz2's byte loop.
const SMALL: usize = 16;
/// Bytes the bulk phase needs from the start of a 64-byte window: the
/// window, the next window's mask and the byte after it.
const LOOKAHEAD: usize = 130;
/// Bytes the block buffer needs past `nblock_max`: the last unit (5 bytes)
/// written with one 8-byte store, and a 16-byte copy of literal bytes that
/// ends at `nblock_max - GUARD` at the latest.
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

/// `p[i..i + 64]`.
#[inline(always)]
pub(super) fn load64<S: Simd>(simd: S, p: &[u8], i: usize) -> u8x64<S> {
    u8x64::simd_from(simd, *p[i..].first_chunk::<64>().unwrap_or(&[0; 64]))
}

/// Bit `k` is set iff `p[i + k] != p[i + k + 1]`, for `k` in `0..64`; reads
/// `p[i..i + 65]`.
#[inline(always)]
fn diff_mask64<S: Simd>(simd: S, p: &[u8], i: usize) -> u64 {
    !simd.to_bitmask_mask8x64(load64(simd, p, i).simd_eq(load64(simd, p, i + 1)))
}

/// Whether a run of 4 equal bytes starts in `p[i..i + 64]`; reads
/// `p[i..i + 67]`.
#[inline(always)]
fn has_run4<S: Simd>(simd: S, p: &[u8], i: usize) -> bool {
    let (a, b) = (load64(simd, p, i), load64(simd, p, i + 1));
    let (c, d) = (load64(simd, p, i + 2), load64(simd, p, i + 3));
    simd.any_true_mask8x64(a.simd_eq(b) & b.simd_eq(c) & c.simd_eq(d))
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

/// The bulk phase of [`fill`]: from the unit boundary `p`, whole 64-byte
/// windows of `input` while the block has room (output at most `limit`).
/// Returns the unit boundary it stopped at and the block length.
///
/// A window's mask of positions that differ from their successor gives the
/// runs: a run of 4 or more starts where three successive bits are clear, and
/// ends at the next set bit. The bytes between runs are literal units and
/// are copied, in one piece, when the next run or the end comes. It stops at
/// a unit boundary, or inside a literal unit of at most 3 bytes that ends
/// within the input (which writes the same bytes however it is split).
#[inline(always)]
fn bulk<S: Simd>(
    simd: S,
    input: &[u8],
    blk: &mut [u8],
    p: usize,
    mut nb: usize,
    limit: usize,
) -> (usize, usize) {
    const NONE: usize = usize::MAX;
    let avail = input.len();
    if p + LOOKAHEAD > avail {
        return (p, nb);
    }
    // `lit`: start of the literal bytes not yet copied; `run`: start of a run
    // whose end is in a later window.
    let (mut lit, mut base, mut run) = (p, p, NONE);
    // `dn`: the mask of the window at `dn_at`, if it was computed.
    let (mut dn, mut dn_at) = (0, NONE);
    let stop = 'win: {
        while base + LOOKAHEAD <= avail {
            let end = if run == NONE { base + 64 } else { run };
            if nb + (end - lit) > limit {
                break 'win end.min(base);
            }
            if run == NONE && !has_run4(simd, input, base) {
                base += 64;
                continue;
            }
            let dw = if dn_at == base {
                dn
            } else {
                diff_mask64(simd, input, base)
            };
            dn = diff_mask64(simd, input, base + 64);
            dn_at = base + 64;
            let r4 = !(dw | (dw >> 1 | dn << 63) | (dw >> 2 | dn << 62));
            let mut from = 0;
            loop {
                let (j, k) = if run == NONE {
                    let m = r4 & (!0u64 << from);
                    if m == 0 {
                        break;
                    }
                    let k = m.trailing_zeros();
                    (base + k as usize, k)
                } else {
                    (run, 0)
                };
                let ends = dw & (!0u64 << k);
                if ends == 0 {
                    run = j;
                    break;
                }
                let last = ends.trailing_zeros();
                let len = base + last as usize + 1 - j;
                if nb + (j - lit) + 5 * (len / 255 + 1) > limit {
                    break 'win lit;
                }
                if j - lit <= 16 {
                    // One fixed-size copy, which may write past the bytes
                    // that count (`SLACK`).
                    blk[nb..nb + 16].copy_from_slice(&input[lit..lit + 16]);
                } else {
                    blk[nb..nb + j - lit].copy_from_slice(&input[lit..j]);
                }
                nb += j - lit;
                let c = input[j];
                let mut l = len;
                while l >= 255 {
                    nb = put_unit(blk, nb, c, 255);
                    l -= 255;
                }
                if l > 0 {
                    nb = put_unit(blk, nb, c, l);
                }
                lit = j + len;
                run = NONE;
                from = last + 1;
                if from == 64 {
                    break;
                }
            }
            base += 64;
        }
        if run == NONE { base } else { run }
    };
    if stop > lit && nb + (stop - lit) <= limit {
        blk[nb..nb + stop - lit].copy_from_slice(&input[lit..stop]);
        nb += stop - lit;
        lit = stop;
    }
    (lit, nb)
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

    let (p2, nb2) = bulk(simd, input, blk, p, nb, limit);
    p = p2;
    nb = nb2;

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
