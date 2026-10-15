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

use fearless_simd::Simd;

use mtf::MAX_ALPHA;

/// Per-encoder working memory, reused between calls and grown to the
/// largest block met so far. Every byte that is read is written first in
/// each block, so it is never cleared.
pub(crate) struct Scratch {
    /// The block in `tt[..n]`, its copy in `tt[n..2n]` for the least
    /// rotation, then padding; the BWT is written over the rotated block.
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

    /// Makes room for `bytes` more bytes of output.
    fn reserve(&mut self, bytes: usize) {
        let need = self.pos + bytes + SLACK;
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

/// The 8 bytes of `v`, most significant first, at `out[pos..]`.
#[inline(always)]
pub(crate) fn store(out: &mut [u8], pos: usize, v: u64) {
    if let Some(d) = out.get_mut(pos..pos + 8) {
        d.copy_from_slice(&v.to_be_bytes());
    }
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
    let cap = 100_000 * usize::from(level);
    // libbz2's limit, 19 bytes below the block size: the last unit, written
    // once the block is full, may add up to 5.
    let nblock_max = cap - 19;
    // RLE1 grows input by at most 5/4 (four equal bytes become five), so a
    // short input never needs a full-size block.
    let biggest = nblock_max.min(src.len() + src.len() / 4) + rle::SLACK;
    let tt_len = 2 * biggest + rotate::PAD;
    if sc.tt.len() < tt_len {
        sc.tt.resize(tt_len, 0);
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
    let mut st = rle::Rle::new();
    let mut combined: u32 = 0;
    let mut pos = 0;
    loop {
        if st.nblock < nblock_max {
            pos += rle::fill(simd, &mut st, &src[pos..], &mut sc.tt, nblock_max);
        }
        // As in libbz2 under BZ_FINISH: once the input is used up, the
        // pending unit joins the block even if the block is already full.
        let last = pos == src.len();
        if last {
            st.flush(&mut sc.tt);
        }
        if st.nblock > 0 {
            let block_crc = !st.crc;
            combined = combined.rotate_left(1) ^ block_crc;
            encode_block(simd, sc, st.nblock, block_crc, &mut bw);
        }
        if last {
            break;
        }
        st.nblock = 0;
        st.crc = !0;
    }
    bw.reserve(11);
    bw.put(24, 0x17_7245);
    bw.put(24, 0x38_5090);
    bw.put(32, u64::from(combined));
    bw.finish();
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
