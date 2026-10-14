//! MSB-first bit reader over the whole input (`dec.h` bzng_bits).
//!
//! Reads past the end of `src` yield zero bits, so decoding never stops for
//! lack of input. Instead every error and the end of every block check
//! [`Bits::overrun`]: if the decoder has used bits that are not in `src`, the
//! input was truncated ([`crate::Error::UnexpectedEof`]).

pub(crate) struct Bits<'a> {
    src: &'a [u8],
    /// Next byte of `src` to load into `buf`; may pass `src.len()`.
    pos: usize,
    /// The next bits, MSB-aligned. Bits below the top `cnt` are either zero or
    /// the stream's next bits, so a refill can OR the same values in again.
    pub(crate) buf: u64,
    /// Valid bits in `buf`.
    pub(crate) cnt: u32,
}

/// A saved reader position, to re-decode from.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Self {
        Bits {
            src,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }

    /// Tops `buf` up to at least 56 valid bits with one 8-byte load.
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        let w = match self.src.get(self.pos..).and_then(|s| s.first_chunk::<8>()) {
            Some(b) => u64::from_be_bytes(*b),
            None => tail_word(self.src, self.pos),
        };
        self.buf |= w >> self.cnt;
        self.pos += ((63 - self.cnt) >> 3) as usize;
        self.cnt |= 56;
    }

    /// Drops `n` (< 64) bits that are known to be valid.
    #[inline(always)]
    pub(crate) fn consume(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }

    /// Takes `n` (1..=32) bits.
    #[inline(always)]
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        if self.cnt < n {
            self.refill();
        }
        let v = (self.buf >> (64 - n)) as u32;
        self.consume(n);
        v
    }

    /// Bits taken so far (consumed, not merely loaded).
    #[inline(always)]
    pub(crate) fn consumed(&self) -> u64 {
        (self.pos as u64) * 8 - u64::from(self.cnt)
    }

    /// Whether the bits taken so far, plus `extra`, run past the input.
    #[inline(always)]
    pub(crate) fn overrun_by(&self, extra: u64) -> bool {
        self.consumed() + extra > (self.src.len() as u64) * 8
    }

    /// Whether the bits taken so far run past the input.
    #[inline(always)]
    pub(crate) fn overrun(&self) -> bool {
        self.overrun_by(0)
    }

    pub(crate) fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            buf: self.buf,
            cnt: self.cnt,
        }
    }

    pub(crate) fn reset(&mut self, m: Mark) {
        self.pos = m.pos;
        self.buf = m.buf;
        self.cnt = m.cnt;
    }
}

/// The 8 bytes at `pos` near or past the end of `src`, zero-padded. Takes
/// values, not the reader, so that the reader's address never escapes and
/// its fields stay in registers.
#[cold]
#[inline(never)]
fn tail_word(src: &[u8], pos: usize) -> u64 {
    let mut b = [0u8; 8];
    if let Some(rest) = src.get(pos..) {
        let k = rest.len().min(8);
        b[..k].copy_from_slice(&rest[..k]);
    }
    u64::from_be_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::Bits;

    #[test]
    fn reads_msb_first_and_pads_with_zeros() {
        let mut b = Bits::new(&[0b1010_0000, 0xFF]);
        assert_eq!(b.get(3), 0b101);
        assert_eq!(b.get(5), 0);
        assert_eq!(b.get(8), 0xFF);
        assert!(!b.overrun());
        assert_eq!(b.get(4), 0);
        assert!(b.overrun());
    }

    #[test]
    fn long_input_crosses_refills() {
        let src: Vec<u8> = (0..40u8).collect();
        let mut b = Bits::new(&src);
        for &x in &src {
            assert_eq!(b.get(8), u32::from(x));
        }
        assert!(!b.overrun());
    }
}
