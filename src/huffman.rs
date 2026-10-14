//! Huffman decoding tables (`dec_block.c` bzng_build_htable, `dec.h`
//! bzng_decode_code).
//!
//! A table decodes every bit pattern exactly as libbz2 1.0.8's
//! limit/base/perm tables do: codes are assigned canonically by length, then
//! symbol; in an oversubscribed set the codes past the end of the code space
//! are unreachable, and in an incomplete set the unused patterns are no code.

use fearless_simd::{Simd, SimdBase, u16x8};

/// Primary table width in bits. bz2-ng's 11 fills twice the entries per
/// table, a fixed cost per block that small blocks feel more than the rarer
/// fallback to `decode_long` for 11-bit codes (notes/06-perf-log.md).
const K: u32 = 10;
/// Longest code bzip2 allows.
pub(crate) const MAX_CODE: u32 = 20;
/// Most symbols in an alphabet (256 bytes + RUNA/RUNB, minus one, plus EOB).
pub(crate) const MAX_ALPHA: usize = 258;

#[derive(Clone)]
pub(crate) struct HTable {
    /// `symbol << 5 | length`; length 0 means longer than `K` bits, or no code.
    prim: [u16; 1 << K],
    /// Exclusive end of each length's codes, left-justified to 20 bits. May
    /// exceed 2^20 for oversubscribed sets (every pattern then matches).
    limit_lj: [u32; MAX_CODE as usize + 2],
    first_code: [i32; MAX_CODE as usize + 2],
    first_idx: [i32; MAX_CODE as usize + 2],
    perm: [u16; MAX_ALPHA],
    maxlen: u32,
}

impl HTable {
    pub(crate) const ZERO: HTable = HTable {
        prim: [0; 1 << K],
        limit_lj: [0; MAX_CODE as usize + 2],
        first_code: [0; MAX_CODE as usize + 2],
        first_idx: [0; MAX_CODE as usize + 2],
        perm: [0; MAX_ALPHA],
        maxlen: 0,
    };

    /// Builds the table for `lengths` (each 1..=20, at most 258 of them).
    #[inline(always)]
    pub(crate) fn build<S: Simd>(&mut self, simd: S, lengths: &[u8]) {
        let mut count = [0u32; MAX_CODE as usize + 2];
        for &l in lengths {
            count[usize::from(l).min(MAX_CODE as usize + 1)] += 1;
        }
        self.maxlen = (1..=MAX_CODE)
            .rev()
            .find(|&l| count[l as usize] != 0)
            .unwrap_or(0);
        let mut offs = [0i32; MAX_CODE as usize + 2];
        let (mut code, mut idx) = (0i32, 0i32);
        for l in 1..=MAX_CODE as usize {
            self.first_code[l] = code;
            self.first_idx[l] = idx;
            offs[l] = idx;
            code += count[l] as i32;
            idx += count[l] as i32;
            // code <= 258 * 2^(l-1), so the shifted value stays below 2^28.
            self.limit_lj[l] = (code as u32) << (MAX_CODE as usize - l);
            code <<= 1;
        }
        self.limit_lj[MAX_CODE as usize + 1] = 0;
        for (i, &l) in lengths.iter().enumerate() {
            let o = &mut offs[usize::from(l).min(MAX_CODE as usize + 1)];
            if let Some(p) = self.perm.get_mut(*o as usize) {
                *p = i as u16;
            }
            *o += 1;
        }

        // Codes of length <= K fill the primary table in canonical order; a
        // code's slots start where the previous one's end, so stopping when
        // the table is full is exactly libbz2's truncation of oversubscribed
        // sets.
        let size = 1usize << K;
        let mut pos = 0usize;
        'fill: for l in 1..=self.maxlen.min(K) {
            let span = 1usize << (K - l);
            let first = self.first_idx[l as usize] as usize;
            for k in 0..count[l as usize] as usize {
                if pos >= size {
                    break 'fill;
                }
                let sym = self.perm.get(first + k).copied().unwrap_or(0);
                let e = (sym << 5) | l as u16;
                if let Some(s) = self.prim.get_mut(pos..pos + span) {
                    // Spans are powers of two: whole 16-byte stores from 8 up.
                    if span >= 8 {
                        let v = u16x8::splat(simd, e);
                        for c in s.as_chunks_mut::<8>().0 {
                            v.store_slice(c);
                        }
                    } else {
                        s.fill(e);
                    }
                }
                pos += span;
            }
        }
        if let Some(rest) = self.prim.get_mut(pos..) {
            rest.fill(0);
        }
    }

    /// Decodes the code at the top of `buf` (at least 20 meaningful bits).
    /// Returns `symbol << 5 | length`, with length 0 if the pattern is no
    /// code.
    #[inline(always)]
    pub(crate) fn decode(&self, buf: u64) -> u32 {
        let e = u32::from(self.prim[(buf >> (64 - K)) as usize]);
        if e & 31 != 0 {
            e
        } else {
            self.decode_long(buf)
        }
    }

    /// Canonical decode of codes longer than `K` bits.
    #[cold]
    #[inline(never)]
    fn decode_long(&self, buf: u64) -> u32 {
        let v = (buf >> (64 - MAX_CODE)) as u32;
        for l in K + 1..=self.maxlen.min(MAX_CODE) {
            let li = l as usize;
            if v < self.limit_lj[li] {
                let i = self.first_idx[li] + (v >> (MAX_CODE - l)) as i32 - self.first_code[li];
                return match self.perm.get(i as usize) {
                    Some(&sym) => (u32::from(sym) << 5) | l,
                    None => 0,
                };
            }
        }
        0
    }
}

impl Default for HTable {
    fn default() -> Self {
        HTable {
            prim: [0; 1 << K],
            limit_lj: [0; MAX_CODE as usize + 2],
            first_code: [0; MAX_CODE as usize + 2],
            first_idx: [0; MAX_CODE as usize + 2],
            perm: [0; MAX_ALPHA],
            maxlen: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HTable;

    fn code(t: &HTable, bits: &str) -> (u32, u32) {
        let mut v = 0u64;
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                v |= 1 << (63 - i);
            }
        }
        let e = t.decode(v);
        (e >> 5, e & 31)
    }

    fn build(t: &mut HTable, lens: &[u8]) {
        fearless_simd::dispatch!(fearless_simd::Level::new(), simd => t.build(simd, lens));
    }

    #[test]
    fn canonical_codes() {
        // lengths 2, 1, 3, 3 -> codes: sym1 = 0, sym0 = 10, sym2 = 110, sym3 = 111
        let mut t = HTable::default();
        build(&mut t, &[2, 1, 3, 3]);
        assert_eq!(code(&t, "0"), (1, 1));
        assert_eq!(code(&t, "10"), (0, 2));
        assert_eq!(code(&t, "110"), (2, 3));
        assert_eq!(code(&t, "111"), (3, 3));
    }

    #[test]
    fn long_codes_and_incomplete_sets() {
        // One 1-bit code, then codes of length 12..=20: incomplete.
        let mut lens = vec![1u8];
        lens.extend(12..=20u8);
        let mut t = HTable::default();
        build(&mut t, &lens);
        assert_eq!(code(&t, "0"), (0, 1));
        // first 12-bit code is 1000_0000_0000
        assert_eq!(code(&t, "100000000000"), (1, 12));
        // the all-ones pattern is no code
        assert_eq!(code(&t, "11111111111111111111").1, 0);
    }

    #[test]
    fn oversubscribed_set_truncates() {
        // Three 1-bit codes: the third is unreachable.
        let mut t = HTable::default();
        build(&mut t, &[1, 1, 1]);
        assert_eq!(code(&t, "0"), (0, 1));
        assert_eq!(code(&t, "1"), (1, 1));
    }

    /// libbz2 1.0.8's decode tables and decode loop (`BZ2_hbCreateDecodeTables`
    /// and `GET_MTF_VAL`), transcribed, as the oracle for [`HTable`]: the
    /// symbol and code length that the top bits of `bits` decode to, if any.
    fn libbz2_decode(lens: &[u8], bits: u64) -> Option<(usize, u32)> {
        let min = usize::from(*lens.iter().min()?);
        let max = usize::from(*lens.iter().max()?);
        let mut perm = [0usize; 258];
        let mut pp = 0;
        for i in min..=max {
            for (j, &l) in lens.iter().enumerate() {
                if usize::from(l) == i {
                    perm[pp] = j;
                    pp += 1;
                }
            }
        }
        let mut base = [0i32; 23];
        for &l in lens {
            base[usize::from(l) + 1] += 1;
        }
        for i in 1..23 {
            base[i] += base[i - 1];
        }
        let mut limit = [0i32; 23];
        let mut vec = 0i32;
        for i in min..=max {
            vec += base[i + 1] - base[i];
            limit[i] = vec - 1;
            vec <<= 1;
        }
        for i in min + 1..=max {
            base[i] = ((limit[i - 1] + 1) << 1) - base[i];
        }
        let mut zn = min;
        let mut zvec = (bits >> (64 - zn)) as i32;
        loop {
            if zn > 20 {
                return None;
            }
            if zvec <= limit[zn] {
                break;
            }
            zn += 1;
            zvec = (zvec << 1) | ((bits >> (64 - zn)) & 1) as i32;
        }
        let idx = zvec - base[zn];
        if !(0..258).contains(&idx) {
            return None;
        }
        assert!(
            (idx as usize) < pp,
            "libbz2 would read a stale entry of perm"
        );
        Some((perm[idx as usize], zn as u32))
    }

    /// The canonical code of each symbol (`BZ2_hbAssignCodes`); a code is
    /// valid where it is below 2^length.
    fn assign_codes(lens: &[u8]) -> Vec<u32> {
        let mut codes = vec![0; lens.len()];
        let mut vec = 0u32;
        for n in 1..=20 {
            for (i, &l) in lens.iter().enumerate() {
                if u32::from(l) == n {
                    codes[i] = vec;
                    vec += 1;
                }
            }
            vec <<= 1;
        }
        codes
    }

    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Code lengths for `alpha` symbols: independent ones (mode 0), those of a
    /// random binary tree, which make a complete code (mode 1), or a tree's
    /// with some lengths lengthened (mode 2, incomplete) or also shortened
    /// (mode 3, oversubscribed).
    fn some_lengths(mode: u8, alpha: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        if mode == 0 {
            let top = 1 + splitmix(&mut s) % 20;
            return (0..alpha)
                .map(|_| 1 + (splitmix(&mut s) % top) as u8)
                .collect();
        }
        let mut lens = vec![1u8, 1];
        while lens.len() < alpha {
            let i = splitmix(&mut s) as usize % lens.len();
            if lens[i] < 20 {
                lens[i] += 1;
                lens.push(lens[i]);
            }
        }
        for i in (1..lens.len()).rev() {
            lens.swap(i, splitmix(&mut s) as usize % (i + 1));
        }
        if mode >= 2 {
            for l in &mut lens {
                match splitmix(&mut s) % 16 {
                    0 if *l < 20 => *l += 1,
                    1 if *l > 1 && mode == 3 => *l -= 1,
                    _ => {}
                }
            }
        }
        lens
    }

    /// For any code lengths libbz2 accepts (1 to 20 each, 3 to 258 symbols):
    /// complete, incomplete or oversubscribed, the table decodes every bit
    /// pattern to the symbol and length libbz2's tables do, or to no code.
    /// Patterns are random, or begin with the canonical code of a symbol.
    #[hegel::test]
    fn decodes_like_libbz2(tc: hegel::TestCase) {
        use hegel::generators as gs;
        let alpha = tc.draw(gs::integers::<usize>().min_value(3).max_value(258));
        let mode = tc.draw(gs::integers::<u8>().max_value(3));
        let lens = some_lengths(mode, alpha, tc.draw(gs::integers::<u64>()));
        let codes = assign_codes(&lens);
        let kraft: u64 = lens.iter().map(|&l| 1u64 << (20 - l)).sum();
        for level in crate::tests::levels() {
            let mut t = HTable::default();
            fearless_simd::dispatch!(level, simd => t.build(simd, &lens));
            for _ in 0..16 {
                let mut bits = tc.draw(gs::integers::<u64>());
                if tc.draw(gs::booleans()) {
                    let s = tc.draw(gs::integers::<usize>().max_value(alpha - 1));
                    let l = u32::from(lens[s]);
                    if u64::from(codes[s]) < 1 << l {
                        bits = u64::from(codes[s]) << (64 - l) | bits >> l;
                        if kraft <= 1 << 20 {
                            assert_eq!(libbz2_decode(&lens, bits), Some((s, l)), "model");
                        }
                    }
                }
                let e = t.decode(bits);
                let got = (e & 31 != 0).then_some(((e >> 5) as usize, e & 31));
                assert_eq!(
                    got,
                    libbz2_decode(&lens, bits),
                    "{level:?} {lens:?} {bits:064b}"
                );
            }
        }
    }
}
