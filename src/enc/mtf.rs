//! Move-to-front and zero-run coding of the BWT (libbz2's
//! `generateMTFValues`; bz2-ng's `bzng_mtf`, vector tier).
//!
//! Output: RUNA = 0, RUNB = 1, rank k >= 1 coded as k + 1, and EOB =
//! `n_in_use + 1` last, with their counts. Works on raw byte values: the list
//! starts as the in-use bytes in ascending order, which gives the same ranks
//! as libbz2's list of sequence numbers.
//!
//! - Rank 0 is exactly `L[i] == L[i - 1]`, so only positions where the byte
//!   changes are visited, found 64 at a time with vector compares.
//! - List positions 0 to 7 live in one 64-bit word: ranks 1 to 7 are a SWAR
//!   byte search and a masked shift.
//! - Positions 8 to 255 live in an array, searched and shifted 16 bytes at a
//!   time.

use fearless_simd::{Select, Simd, SimdBase, SimdFrom, u8x16, u8x64, u16x8};

use super::rle::zero_bytes;

/// The highest MTF symbol plus one (RUNA, RUNB, ranks 1 to 255, EOB).
pub(crate) const MAX_ALPHA: usize = 258;
const RUNA: usize = 0;
const RUNB: usize = 1;
/// Values past the last one that [`mtf`] may write to: a zero run's digits
/// go out as one 8-value store.
pub(crate) const SLACK: usize = 8;

/// The MTF list: entries 0 to 7 in `head` (entry 0 in the low byte), 8 to
/// 255 in `tail`, padded with a byte that never matches.
struct List {
    head: u64,
    tail: [u8; 256],
}

const IOTA: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

impl List {
    /// Moves `c` to the front and returns its rank.
    #[inline(always)]
    fn rank<S: Simd>(&mut self, simd: S, c: u8) -> usize {
        let h = self.head;
        let z = zero_bytes(h ^ (0x0101_0101_0101_0101 * u64::from(c)));
        if z != 0 {
            self.head = h ^ ((h ^ ((h << 8) | u64::from(c))) & (z << 1).wrapping_sub(1));
            (z.trailing_zeros() >> 3) as usize
        } else {
            self.tail_to_front(simd, c)
        }
    }

    /// Moves `c`, known to be at list position 8 or later, to the front;
    /// returns its rank. Each 16 bytes of the tail shift up one entry, taking
    /// the previous vector's last byte; the one holding `c` shifts only up
    /// to it.
    #[inline(always)]
    fn tail_to_front<S: Simd>(&mut self, simd: S, c: u8) -> usize {
        let vc = u8x16::splat(simd, c);
        let mut prev = u8x16::splat(simd, (self.head >> 56) as u8);
        let mut r = 0;
        for (k, chunk) in self.tail.as_chunks_mut::<16>().0.iter_mut().enumerate() {
            let cur = u8x16::simd_from(simd, *chunk);
            let shifted = prev.slide::<15>(cur);
            let hit = simd.to_bitmask_mask8x16(cur.simd_eq(vc));
            if hit != 0 {
                let lane = hit.trailing_zeros();
                let keep = u8x16::simd_from(simd, IOTA).simd_le(u8x16::splat(simd, lane as u8));
                keep.select(shifted, cur).store_slice(chunk);
                r = 8 + 16 * k + lane as usize;
                break;
            }
            shifted.store_slice(chunk);
            prev = cur;
        }
        self.head = (self.head << 8) | u64::from(c);
        r
    }
}

/// Bit `k` set iff `l[i + k] != l[i + k - 1]`, for `k` in `0..64`; reads
/// `l[i - 1..i + 64]`.
#[inline(always)]
fn change_mask64<S: Simd>(simd: S, l: &[u8], i: usize) -> u64 {
    let at = |o: usize| u8x64::simd_from(simd, *l[o..].first_chunk::<64>().unwrap_or(&[0; 64]));
    !simd.to_bitmask_mask8x64(at(i).simd_eq(at(i - 1)))
}

/// Writes the digits of a zero run of length `z` at `mtfv[wr..]` and counts
/// them; returns the new write position. The RUNA/RUNB digits of a run of
/// `z` are the bits of `z + 1` below its leading one, least significant
/// first. Up to 8 digits go out as one store, so it writes up to
/// [`SLACK`] values past the new write position.
#[inline(always)]
fn put_zero_run<S: Simd>(
    simd: S,
    mtfv: &mut [u16],
    wr: usize,
    freq: &mut [i32; MAX_ALPHA],
    z: u32,
) -> usize {
    let x = z + 1;
    let k = 31 - x.leading_zeros();
    if k <= 8 {
        const BIT: [u16; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
        let d = u16x8::splat(simd, x as u16) & u16x8::simd_from(simd, BIT);
        d.min(u16x8::splat(simd, 1))
            .store_slice(&mut mtfv[wr..wr + 8]);
    } else {
        for i in 0..k {
            mtfv[wr + i as usize] = ((x >> i) & 1) as u16;
        }
    }
    let ones = (x & ((1 << k) - 1)).count_ones();
    freq[RUNB] += ones as i32;
    freq[RUNA] += (k - ones) as i32;
    wr + k as usize
}

/// Codes the rank of `c` at `mtfv[wr]`; returns the new write position.
#[inline(always)]
fn put_symbol<S: Simd>(
    simd: S,
    lst: &mut List,
    c: u8,
    mtfv: &mut [u16],
    wr: usize,
    freq: &mut [i32; MAX_ALPHA],
) -> usize {
    let r = lst.rank(simd, c) + 1;
    mtfv[wr] = r as u16;
    freq[r] += 1;
    wr + 1
}

/// MTF and zero-run coding of the BWT `l`, whose bytes are those marked in
/// `in_use`: the values to `mtfv` (at least `l.len() + 1 + SLACK` long) and their
/// counts to `freq`. Returns the number of values.
#[inline(always)]
pub(crate) fn mtf<S: Simd>(
    simd: S,
    l: &[u8],
    in_use: &[bool; 256],
    mtfv: &mut [u16],
    freq: &mut [i32; MAX_ALPHA],
) -> usize {
    let mut list = [0u8; 256 + 8];
    let mut n_in_use = 0;
    for c in 0..=255u8 {
        if in_use[usize::from(c)] {
            list[n_in_use] = c;
            n_in_use += 1;
        }
    }
    // Unused slots must never match a symbol: a byte not in use fills them
    // (there is one whenever there are unused slots).
    let filler = (0..=255u8).find(|&c| !in_use[usize::from(c)]).unwrap_or(0);
    list[n_in_use..].fill(filler);
    let mut head = [0; 8];
    head.copy_from_slice(&list[..8]);
    let mut tail = [0; 256];
    tail.copy_from_slice(&list[8..]);
    let mut lst = List {
        head: u64::from_le_bytes(head),
        tail,
    };

    let eob = n_in_use + 1;
    freq[..=eob].fill(0);
    let mut wr = 0;
    if let Some((&first, rest)) = l.split_first() {
        // Position 0 compares with the list head, the others with their
        // predecessor.
        let mut z_pend = 0;
        if first == list[0] {
            z_pend = 1;
        } else {
            wr = put_symbol(simd, &mut lst, first, mtfv, wr, freq);
        }
        let n = rest.len() + 1;
        // `cur` is the first position whose zero rank is not yet counted.
        let (mut i, mut cur) = (1, 1);
        while i + 64 <= n {
            let mut m = change_mask64(simd, l, i);
            while m != 0 {
                let pos = i + m.trailing_zeros() as usize;
                m &= m - 1;
                wr = put_zero_run(simd, mtfv, wr, freq, z_pend + (pos - cur) as u32);
                z_pend = 0;
                wr = put_symbol(simd, &mut lst, l[pos], mtfv, wr, freq);
                cur = pos + 1;
            }
            i += 64;
        }
        z_pend += (i - cur) as u32;
        for pos in i..n {
            if l[pos] == l[pos - 1] {
                z_pend += 1;
                continue;
            }
            wr = put_zero_run(simd, mtfv, wr, freq, z_pend);
            z_pend = 0;
            wr = put_symbol(simd, &mut lst, l[pos], mtfv, wr, freq);
        }
        wr = put_zero_run(simd, mtfv, wr, freq, z_pend);
    }
    mtfv[wr] = eob as u16;
    freq[eob] += 1;
    wr + 1
}
