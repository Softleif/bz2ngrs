//! The encoder: one call compresses a whole input into one bzip2 stream,
//! byte-identical to what libbz2 1.0.8's `BZ2_bzBuffToBuffCompress` writes
//! (one `BZ_FINISH` over the whole input), with one documented exception: a
//! block that is exactly periodic after RLE1 has several valid origPtrs, and
//! this writes the last of the tied rows, as bz2-ng does.
//!
//! A port of bz2-ng's encoder: the RLE1 input stage (`rle.rs`), the least
//! rotation (`rotate.rs`), the block sorter (`sort.rs`), MTF and zero runs
//! (`mtf.rs`) and the Huffman stage (`huff.rs`); this module frames the
//! stream around them as `BZ2_compressBlock` does.

mod huff;
mod mtf;
mod rle;
mod rotate;
mod sais32;
mod sais_top;
mod sort;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "tests"
)]
mod tests;

use fearless_simd::{Simd, SimdFrom, u8x16, u8x64};

use mtf::MAX_ALPHA;

/// Per-encoder working memory, reused between calls and grown to the
/// largest block met so far. Every byte that is read is written first in
/// each block, so it is never cleared.
pub(crate) struct Scratch {
    /// The block in `tt[..n]` and its copy in `tt[n..2n]` for the least
    /// rotation; the BWT is written over the rotated block.
    tt: Vec<u8>,
    /// The least rotation's candidates, then the sorter's work array.
    work: Vec<i32>,
    mtfv: Vec<u16>,
    huff: Box<huff::Huff>,
}

impl Scratch {
    pub(crate) fn new_boxed() -> Box<Scratch> {
        Box::new(Scratch {
            tt: Vec::new(),
            work: Vec::new(),
            mtfv: Vec::new(),
            huff: huff::Huff::new_boxed(),
        })
    }
}

/// MSB-first bit writer: the low `n` bits of `acc` are pending, and
/// `out[..pos]` is written. Every flush stores 8 bytes at `pos`, so `out` is
/// kept at least 8 bytes longer than anything written: callers reserve the
/// worst case of what follows ([`BitWriter::reserve`]), and
/// [`BitWriter::finish`] cuts `out` back to what was written.
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    pos: usize,
    acc: u64,
    n: u32,
}

/// Bytes past the written end that a flush may store to.
const SLACK: usize = 8;

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        let pos = out.len();
        BitWriter {
            out,
            pos,
            acc: 0,
            n: 0,
        }
    }

    /// A writer that continues after [`BitWriter::suspend`]; `n < 8` bits
    /// are pending in `acc`.
    fn resume(out: &'a mut Vec<u8>, acc: u64, n: u32) -> Self {
        let pos = out.len();
        BitWriter { out, pos, acc, n }
    }

    /// Writes the whole pending bytes, cuts the output back to them and
    /// returns the remaining `n < 8` bits.
    fn suspend(mut self) -> (u64, u32) {
        if self.n >= 8 {
            self.flush();
        }
        self.out.truncate(self.pos);
        (self.acc & ((1 << self.n) - 1), self.n)
    }

    /// Makes room for `bytes` more bytes of output on top of the bits
    /// pending.
    fn reserve(&mut self, bytes: usize) {
        let need = self.pos + self.n.div_ceil(8) as usize + bytes + SLACK;
        if self.out.len() < need {
            self.out.resize(need, 0);
        }
    }

    /// Appends the `n <= 32` low bits of `v` (no bits above them are set).
    #[inline(always)]
    pub(crate) fn put(&mut self, n: u32, v: u64) {
        if self.n + n > 63 {
            self.flush();
        }
        self.acc = (self.acc << n) | v;
        self.n += n;
    }

    /// Writes the whole pending bytes, leaving at most 7 bits pending.
    #[inline(always)]
    pub(crate) fn flush(&mut self) {
        if self.n > 0 {
            store(self.out, self.pos, self.acc << (64 - self.n));
            self.pos += (self.n / 8) as usize;
            self.n &= 7;
        }
    }

    /// The pending bits, the output and its write position, for an emitter
    /// that keeps them in registers ([`BitWriter::set_parts`] hands them back).
    pub(crate) fn parts(&mut self) -> (u64, u32, &mut [u8], usize) {
        (self.acc, self.n, self.out, self.pos)
    }

    pub(crate) fn set_parts(&mut self, acc: u64, n: u32, pos: usize) {
        self.acc = acc;
        self.n = n;
        self.pos = pos;
    }

    /// Pads the last byte with zero bits, writes it, and cuts the output
    /// back to the written bytes.
    fn finish(mut self) {
        let pad = (8 - self.n % 8) % 8;
        self.put(pad, 0);
        self.flush();
        self.out.truncate(self.pos);
    }
}

// The loads and the store below never panic, so that a wrong bound costs a
// wrong stream rather than a panic; the assertions catch one in tests.

/// The 8 bytes of `v`, most significant first, at `out[pos..]`.
#[inline(always)]
pub(crate) fn store(out: &mut [u8], pos: usize, v: u64) {
    debug_assert!(pos + 8 <= out.len(), "store past the reserved output");
    if let Some(d) = out.get_mut(pos..pos + 8) {
        d.copy_from_slice(&v.to_be_bytes());
    }
}

/// `p[i..i + 16]`.
#[inline(always)]
pub(crate) fn load16<S: Simd>(simd: S, p: &[u8], i: usize) -> u8x16<S> {
    debug_assert!(i + 16 <= p.len(), "load past the buffer");
    u8x16::simd_from(simd, *p[i..].first_chunk::<16>().unwrap_or(&[0; 16]))
}

/// `p[i..i + 64]`.
#[inline(always)]
pub(crate) fn load64<S: Simd>(simd: S, p: &[u8], i: usize) -> u8x64<S> {
    debug_assert!(i + 64 <= p.len(), "load past the buffer");
    u8x64::simd_from(simd, *p[i..].first_chunk::<64>().unwrap_or(&[0; 64]))
}

/// The most bytes a block of `n` RLE1 bytes adds to the stream: its header
/// and symbol map, at most 6 bits per selector, the six code-length tables
/// (5 bits each, then per symbol 1 bit and 2 bits per unit of change), and at
/// most `n + 1` coded MTF values of at most 17 bits (bz2-ng's
/// `bzng_block_bound`).
fn block_bound(n: usize) -> usize {
    let nmtf = n + 1;
    let data = (nmtf * 17).div_ceil(8);
    let selectors = (nmtf.div_ceil(50) * 6).div_ceil(8);
    let lengths = 6 * (5 + 258 * (1 + 2 * 17)) / 8 + 1;
    data + selectors + lengths + 14 + 34 + 3 + 1
}

/// Compresses `src` as one stream with blocks of `level` × 100,000 bytes
/// (after RLE1), appending it to `out`.
#[inline(always)]
pub(crate) fn compress_stream<S: Simd>(
    simd: S,
    src: &[u8],
    level: u8,
    out: &mut Vec<u8>,
    sc: &mut Scratch,
) {
    let mut s = Stream::start(simd, level, src.len(), out, sc);
    s.push(simd, sc, src, out);
    s.finish(simd, sc, out);
}

/// One stream in progress: the RLE1 state, the combined CRC and the bits not
/// yet a whole byte, kept between calls so that input may arrive in any
/// pieces. Only whole bytes are in the output `Vec` between calls.
pub(crate) struct Stream {
    st: rle::Rle,
    combined: u32,
    acc: u64,
    n: u32,
    nblock_max: usize,
}

impl Stream {
    /// Sizes `sc` for at most `size_hint` bytes of input and writes the
    /// stream header to `out`.
    #[inline(always)]
    pub(crate) fn start<S: Simd>(
        _simd: S,
        level: u8,
        size_hint: usize,
        out: &mut Vec<u8>,
        sc: &mut Scratch,
    ) -> Stream {
        let cap = 100_000 * usize::from(level);
        // libbz2's limit, 19 bytes below the block size: the last unit,
        // written once the block is full, may add up to 5.
        let nblock_max = cap - 19;
        // RLE1 grows input by at most 5/4 (four equal bytes become five), so
        // a short input never needs a full-size block.
        let biggest = nblock_max.min(size_hint.saturating_add(size_hint / 4)) + rle::SLACK;
        if sc.tt.len() < 2 * biggest {
            sc.tt.resize(2 * biggest, 0);
        }
        if sc.mtfv.len() < biggest + 1 + mtf::SLACK {
            sc.mtfv.resize(biggest + 1 + mtf::SLACK, 0);
        }
        let mut bw = BitWriter::new(out);
        bw.reserve(4);
        bw.put(
            32,
            u64::from(u32::from_be_bytes(*b"BZh0") + u32::from(level)),
        );
        let (acc, n) = bw.suspend();
        Stream {
            st: rle::Rle::new(),
            combined: 0,
            acc,
            n,
            nblock_max,
        }
    }

    /// Feeds `input`, appending each block that completes to `out`.
    #[inline(always)]
    pub(crate) fn push<S: Simd>(
        &mut self,
        simd: S,
        sc: &mut Scratch,
        input: &[u8],
        out: &mut Vec<u8>,
    ) {
        let mut bw = BitWriter::resume(out, self.acc, self.n);
        let mut pos = 0;
        while pos < input.len() {
            // A full block is held until more input shows that it is not
            // the last: libbz2 under BZ_FINISH adds the pending unit to the
            // last block even when it is full.
            if self.st.nblock >= self.nblock_max {
                self.emit(simd, sc, &mut bw);
            }
            pos += rle::fill(
                simd,
                &mut self.st,
                &input[pos..],
                &mut sc.tt,
                self.nblock_max,
            );
        }
        (self.acc, self.n) = bw.suspend();
    }

    /// Ends the stream: the last block, the end-of-stream marker and the
    /// combined CRC, padded to a whole byte.
    #[inline(always)]
    pub(crate) fn finish<S: Simd>(mut self, simd: S, sc: &mut Scratch, out: &mut Vec<u8>) {
        let mut bw = BitWriter::resume(out, self.acc, self.n);
        self.st.flush(&mut sc.tt);
        if self.st.nblock > 0 {
            self.emit(simd, sc, &mut bw);
        }
        bw.reserve(11);
        bw.put(24, 0x17_7245);
        bw.put(24, 0x38_5090);
        bw.put(32, u64::from(self.combined));
        bw.finish();
    }

    /// Encodes the block in `sc.tt` and starts the next one.
    #[inline(always)]
    fn emit<S: Simd>(&mut self, simd: S, sc: &mut Scratch, bw: &mut BitWriter<'_>) {
        let block_crc = !self.st.crc;
        self.combined = self.combined.rotate_left(1) ^ block_crc;
        encode_block(simd, sc, self.st.nblock, block_crc, bw);
        self.st.nblock = 0;
        self.st.crc = !0;
    }
}

/// Encodes the block `sc.tt[..n]` with its finalised CRC: block magic, CRC,
/// randomised bit, origPtr, symbol map, then the Huffman stage.
#[inline(always)]
fn encode_block<S: Simd>(
    simd: S,
    sc: &mut Scratch,
    n: usize,
    block_crc: u32,
    bw: &mut BitWriter<'_>,
) {
    let mut freq = [0i32; 256];
    let (orig, r) = rotate::sort_block(simd, &mut sc.tt, n, &mut freq, &mut sc.work);
    let mut in_use = [false; 256];
    for (u, &f) in in_use.iter_mut().zip(&freq) {
        *u = f != 0;
    }
    let n_in_use = in_use.iter().filter(|&&u| u).count();
    let mut mtf_freq = [0i32; MAX_ALPHA];
    let nmtf = mtf::mtf(simd, &sc.tt[r..r + n], &in_use, &mut sc.mtfv, &mut mtf_freq);

    bw.reserve(block_bound(n));
    bw.put(24, 0x31_4159);
    bw.put(24, 0x26_5359);
    bw.put(32, u64::from(block_crc));
    bw.put(1, 0);
    bw.put(24, orig as u64);
    // 16 bits for which 16-byte ranges are used, then 16 bits per used
    // range.
    let used16 = (0..16).fold(0u64, |acc, i| {
        acc | (u64::from(in_use[i * 16..i * 16 + 16].contains(&true)) << (15 - i))
    });
    bw.put(16, used16);
    for i in 0..16 {
        if used16 & (1 << (15 - i)) != 0 {
            let bits = (0..16).fold(0u64, |acc, j| {
                acc | (u64::from(in_use[i * 16 + j]) << (15 - j))
            });
            bw.put(16, bits);
        }
    }
    huff::encode(&mut sc.huff, &sc.mtfv[..nmtf], &mtf_freq, n_in_use + 2, bw);
}
