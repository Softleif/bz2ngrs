//! Move-to-front and zero-run coding of the BWT (libbz2's
//! `generateMTFValues`; bz2-ng's `bzng_mtf`, scalar tier).
//!
//! Output: RUNA = 0, RUNB = 1, rank k >= 1 coded as k + 1, and EOB =
//! `n_in_use + 1` last, with their counts. Works on raw byte values: the list
//! starts as the in-use bytes in ascending order, which gives the same ranks
//! as libbz2's list of sequence numbers.
//!
//! - Rank 0 is exactly `L[i] == L[i - 1]`, so only positions where the byte
//!   changes are coded.
//! - List positions 0 to 7 live in one 64-bit word: ranks 1 to 7 are a SWAR
//!   byte search and a masked shift.
//! - Positions 8 to 255 live in an array, searched and shifted 8 bytes at a
//!   time.

use super::rle::zero_bytes;

/// The highest MTF symbol plus one (RUNA, RUNB, ranks 1 to 255, EOB).
pub(crate) const MAX_ALPHA: usize = 258;
const RUNA: usize = 0;
const RUNB: usize = 1;

/// The MTF list: entries 0 to 7 in `head` (entry 0 in the low byte), 8 to
/// 255 in `tail`, padded with a byte that never matches.
struct List {
    head: u64,
    tail: [u8; 256],
}

impl List {
    /// Moves `c` to the front and returns its rank.
    #[inline(always)]
    fn rank(&mut self, c: u8) -> usize {
        let h = self.head;
        let z = zero_bytes(h ^ (0x0101_0101_0101_0101 * u64::from(c)));
        if z != 0 {
            self.head = h ^ ((h ^ ((h << 8) | u64::from(c))) & (z << 1).wrapping_sub(1));
            (z.trailing_zeros() >> 3) as usize
        } else {
            self.tail_to_front(c)
        }
    }

    /// Moves `c`, known to be at list position 8 or later, to the front;
    /// returns its rank.
    fn tail_to_front(&mut self, c: u8) -> usize {
        let vc = 0x0101_0101_0101_0101 * u64::from(c);
        let mut carry = self.head >> 56;
        let mut r = 0;
        for (k, chunk) in self.tail.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let mut w = u64::from_le_bytes(*chunk);
            let shifted = (w << 8) | carry;
            let z = zero_bytes(w ^ vc);
            if z != 0 {
                w ^= (w ^ shifted) & (z << 1).wrapping_sub(1);
                *chunk = w.to_le_bytes();
                r = 8 + 8 * k + (z.trailing_zeros() >> 3) as usize;
                break;
            }
            *chunk = shifted.to_le_bytes();
            carry = w >> 56;
        }
        self.head = (self.head << 8) | u64::from(c);
        r
    }
}

/// Writes the digits of a zero run of length `z` at `mtfv[wr..]` and counts
/// them; returns the new write position. The RUNA/RUNB digits of a run of
/// `z` are the bits of `z + 1` below its leading one, least significant
/// first.
#[inline(always)]
fn put_zero_run(mtfv: &mut [u16], wr: usize, freq: &mut [i32; MAX_ALPHA], z: u32) -> usize {
    let x = z + 1;
    let k = 31 - x.leading_zeros();
    for i in 0..k {
        mtfv[wr + i as usize] = ((x >> i) & 1) as u16;
    }
    let ones = (x & ((1 << k) - 1)).count_ones();
    freq[RUNB] += ones as i32;
    freq[RUNA] += (k - ones) as i32;
    wr + k as usize
}

/// Codes the rank of `c` at `mtfv[wr]`; returns the new write position.
#[inline(always)]
fn put_symbol(
    lst: &mut List,
    c: u8,
    mtfv: &mut [u16],
    wr: usize,
    freq: &mut [i32; MAX_ALPHA],
) -> usize {
    let r = lst.rank(c) + 1;
    mtfv[wr] = r as u16;
    freq[r] += 1;
    wr + 1
}

/// MTF and zero-run coding of the BWT `l`, whose bytes are those marked in
/// `in_use`: the values to `mtfv` (at least `l.len() + 1` long) and their
/// counts to `freq`. Returns the number of values.
pub(crate) fn mtf(
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
            wr = put_symbol(&mut lst, first, mtfv, wr, freq);
        }
        let mut prev = first;
        for &c in rest {
            if c == prev {
                z_pend += 1;
                continue;
            }
            wr = put_zero_run(mtfv, wr, freq, z_pend);
            z_pend = 0;
            wr = put_symbol(&mut lst, c, mtfv, wr, freq);
            prev = c;
        }
        wr = put_zero_run(mtfv, wr, freq, z_pend);
    }
    mtfv[wr] = eob as u16;
    freq[eob] += 1;
    wr + 1
}
