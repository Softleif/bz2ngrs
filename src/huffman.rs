//! Huffman decoding tables (`dec_block.c` bzng_build_htable, `dec.h`
//! bzng_decode_code).
//!
//! A table decodes every bit pattern exactly as libbz2 1.0.8's
//! limit/base/perm tables do: codes are assigned canonically by length, then
//! symbol; in an oversubscribed set the codes past the end of the code space
//! are unreachable, and in an incomplete set the unused patterns are no code.

/// Primary table width in bits.
const K: u32 = 11;
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
    /// Builds the table for `lengths` (each 1..=20, at most 258 of them).
    pub(crate) fn build(&mut self, lengths: &[u8]) {
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
                    s.fill(e);
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

    #[test]
    fn canonical_codes() {
        // lengths 2, 1, 3, 3 -> codes: sym1 = 0, sym0 = 10, sym2 = 110, sym3 = 111
        let mut t = HTable::default();
        t.build(&[2, 1, 3, 3]);
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
        t.build(&lens);
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
        t.build(&[1, 1, 1]);
        assert_eq!(code(&t, "0"), (0, 1));
        assert_eq!(code(&t, "1"), (1, 1));
    }
}
