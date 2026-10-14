//! un-RLE1 (the first run-length stage) from the block's text into the
//! caller's buffer, and libbz2's block randomisation.

use fearless_simd::{Simd, SimdBase, u8x16};

use crate::Error;

/// Text bytes that must remain for a bulk step. A step consumes at most 12
/// and stores at most 31 bytes past its output, and the at least 52 text
/// bytes left after it decode to at least 40 bytes (at most one in five text
/// bytes is a count), which overwrite those stores. So on success no byte
/// past the returned length is ever written.
const MARGIN: usize = 64;

/// Zero bytes of `x` as 0x80, others 0 (exact, no carries between bytes).
#[inline(always)]
fn zero_bytes(x: u64) -> u64 {
    const L: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    !(((x & L) + L) | x | L)
}

/// Bytes `i..i + 8` of `w` (constant `i`, so no bounds check survives).
#[inline(always)]
fn le8(w: &[u8; 16], i: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&w[i..i + 8]);
    u64::from_le_bytes(b)
}

/// Decodes `text` (one block's text) into `out[o..]` and returns the new
/// output position. Four equal bytes are followed by a count byte (0..=255)
/// of further copies.
#[inline(always)]
pub(crate) fn unrle<S: Simd>(
    simd: S,
    text: &[u8],
    out: &mut [u8],
    mut o: usize,
) -> Result<usize, Error> {
    let n = text.len();
    let mut j = 0;
    // Start of the current stretch of literals (just after the last run).
    let mut lit = 0;
    // Bulk: eight candidate run starts at a time. Candidate k is set when
    // text[j+k..j+k+4] are equal; the first one after a clean state is a true
    // run start (an earlier equal byte would have set an earlier candidate).
    while j + MARGIN <= n && o + 8 <= out.len() {
        let Some(w) = text.get(j..j + 16).and_then(|s| s.first_chunk::<16>()) else {
            break;
        };
        let a = le8(w, 0);
        let m = zero_bytes(a ^ le8(w, 1)) & zero_bytes(a ^ le8(w, 2)) & zero_bytes(a ^ le8(w, 3));
        if let Some(d) = out.get_mut(o..o + 8) {
            d.copy_from_slice(&a.to_le_bytes());
        }
        if m == 0 {
            j += 8;
            o += 8;
            continue;
        }
        let k = (m.trailing_zeros() / 8) as usize & 7;
        let ch = w[k];
        let total = 4 + usize::from(w[k + 4]);
        j += k + 5;
        o += k;
        lit = j;
        fill(simd, out, o, ch, total)?;
        o += total;
    }

    // Tail: byte by byte, with the equal bytes carried in from the bulk loop.
    let mut last = 0u8;
    let mut eq = 0u32;
    if j > lit {
        last = text.get(j - 1).copied().unwrap_or(0);
        eq = 1;
        while eq < 3 && j - eq as usize > lit && text.get(j - 1 - eq as usize) == Some(&last) {
            eq += 1;
        }
    }
    while let Some(&x) = text.get(j) {
        j += 1;
        *out.get_mut(o).ok_or(Error::OutputTooSmall)? = x;
        o += 1;
        if eq > 0 && x == last {
            eq += 1;
        } else {
            last = x;
            eq = 1;
        }
        if eq == 4 {
            // Four equal bytes ending the block, with no count byte, is
            // libbz2's one corrupt-output case.
            let cnt = usize::from(*text.get(j).ok_or(Error::Corrupt)?);
            j += 1;
            out.get_mut(o..o + cnt)
                .ok_or(Error::OutputTooSmall)?
                .fill(x);
            o += cnt;
            eq = 0;
        }
    }
    Ok(o)
}

/// Writes `total` copies of `ch` at `out[o..]`: two 16-byte stores, then a
/// loop only for runs past 32 bytes (most are shorter, so its exit is
/// predictable), when there is room for the overshoot.
#[inline(always)]
fn fill<S: Simd>(simd: S, out: &mut [u8], o: usize, ch: u8, total: usize) -> Result<(), Error> {
    let wide = total.max(32).next_multiple_of(16);
    let v = u8x16::splat(simd, ch);
    if let Some(d) = out.get_mut(o..o + wide) {
        if let [c0, c1, rest @ ..] = d.as_chunks_mut::<16>().0 {
            v.store_slice(c0);
            v.store_slice(c1);
            for c in rest {
                v.store_slice(c);
            }
        }
        Ok(())
    } else {
        out.get_mut(o..o + total)
            .ok_or(Error::OutputTooSmall)?
            .fill(ch);
        Ok(())
    }
}

/// Undoes libbz2's block randomisation: flips bit 0 of the text byte at
/// each position where the `RAND_NUMS` countdown hits 1.
pub(crate) fn derandomise(text: &mut [u8]) {
    let mut start = 0usize;
    let mut t = 0usize;
    loop {
        let r = RAND_NUMS[t % 512] as usize;
        match text.get_mut(start + r - 2) {
            Some(b) => *b ^= 1,
            None => break,
        }
        start += r;
        t += 1;
    }
}

/// libbz2's `BZ2_rNums`.
static RAND_NUMS: [u16; 512] = [
    619, 720, 127, 481, 931, 816, 813, 233, 566, 247, 985, 724, 205, 454, 863, 491, 741, 242, 949,
    214, 733, 859, 335, 708, 621, 574, 73, 654, 730, 472, 419, 436, 278, 496, 867, 210, 399, 680,
    480, 51, 878, 465, 811, 169, 869, 675, 611, 697, 867, 561, 862, 687, 507, 283, 482, 129, 807,
    591, 733, 623, 150, 238, 59, 379, 684, 877, 625, 169, 643, 105, 170, 607, 520, 932, 727, 476,
    693, 425, 174, 647, 73, 122, 335, 530, 442, 853, 695, 249, 445, 515, 909, 545, 703, 919, 874,
    474, 882, 500, 594, 612, 641, 801, 220, 162, 819, 984, 589, 513, 495, 799, 161, 604, 958, 533,
    221, 400, 386, 867, 600, 782, 382, 596, 414, 171, 516, 375, 682, 485, 911, 276, 98, 553, 163,
    354, 666, 933, 424, 341, 533, 870, 227, 730, 475, 186, 263, 647, 537, 686, 600, 224, 469, 68,
    770, 919, 190, 373, 294, 822, 808, 206, 184, 943, 795, 384, 383, 461, 404, 758, 839, 887, 715,
    67, 618, 276, 204, 918, 873, 777, 604, 560, 951, 160, 578, 722, 79, 804, 96, 409, 713, 940,
    652, 934, 970, 447, 318, 353, 859, 672, 112, 785, 645, 863, 803, 350, 139, 93, 354, 99, 820,
    908, 609, 772, 154, 274, 580, 184, 79, 626, 630, 742, 653, 282, 762, 623, 680, 81, 927, 626,
    789, 125, 411, 521, 938, 300, 821, 78, 343, 175, 128, 250, 170, 774, 972, 275, 999, 639, 495,
    78, 352, 126, 857, 956, 358, 619, 580, 124, 737, 594, 701, 612, 669, 112, 134, 694, 363, 992,
    809, 743, 168, 974, 944, 375, 748, 52, 600, 747, 642, 182, 862, 81, 344, 805, 988, 739, 511,
    655, 814, 334, 249, 515, 897, 955, 664, 981, 649, 113, 974, 459, 893, 228, 433, 837, 553, 268,
    926, 240, 102, 654, 459, 51, 686, 754, 806, 760, 493, 403, 415, 394, 687, 700, 946, 670, 656,
    610, 738, 392, 760, 799, 887, 653, 978, 321, 576, 617, 626, 502, 894, 679, 243, 440, 680, 879,
    194, 572, 640, 724, 926, 56, 204, 700, 707, 151, 457, 449, 797, 195, 791, 558, 945, 679, 297,
    59, 87, 824, 713, 663, 412, 693, 342, 606, 134, 108, 571, 364, 631, 212, 174, 643, 304, 329,
    343, 97, 430, 751, 497, 314, 983, 374, 822, 928, 140, 206, 73, 263, 980, 736, 876, 478, 430,
    305, 170, 514, 364, 692, 829, 82, 855, 953, 676, 246, 369, 970, 294, 750, 807, 827, 150, 790,
    288, 923, 804, 378, 215, 828, 592, 281, 565, 555, 710, 82, 896, 831, 547, 261, 524, 462, 293,
    465, 502, 56, 661, 821, 976, 991, 658, 869, 905, 758, 745, 193, 768, 550, 608, 933, 378, 286,
    215, 979, 792, 961, 61, 688, 793, 644, 986, 403, 106, 366, 905, 644, 372, 567, 466, 434, 645,
    210, 389, 550, 919, 135, 780, 773, 635, 389, 707, 100, 626, 958, 165, 504, 920, 176, 193, 713,
    857, 265, 203, 50, 668, 108, 645, 990, 626, 197, 510, 357, 358, 850, 858, 364, 936, 638,
];
