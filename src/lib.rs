#![doc = include_str!("../README.md")]

// The decoder's casts narrow row indices, symbols and code lengths, which
// the format bounds far below the target types (rows < 2^20, symbols < 258,
// lengths <= 20); the hot loops rely on them being plain casts.
#[allow(
    clippy::cast_possible_truncation,
    reason = "values bounded by the format"
)]
mod bits;
#[allow(
    clippy::cast_possible_truncation,
    reason = "values bounded by the format"
)]
mod bwt;
mod crc;
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "values bounded by the format"
)]
mod decode;
// The encoder's casts narrow sizes and positions bounded by the block size
// (< 2^20) and symbols bounded by the alphabet (< 258).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "values bounded by the format"
)]
mod enc;
mod error;
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "values bounded by the format"
)]
mod huffman;
mod read;
mod unrle;
mod write;

use core::fmt;

pub use error::{Corruption, Error, Position};
pub use read::DecompressReader;
pub use write::CompressWriter;

use decode::Scratch;

/// Decoder for bzip2 data in memory; [`DecompressReader`] decodes from a
/// `Read`.
///
/// It owns the working memory of a decode (about 6 MiB), allocated
/// on the first call and kept until it is dropped, so keep one per thread
/// and reuse it. The SIMD level is chosen in [`Decoder::new`]. The crate
/// docs say which method fits which input.
///
/// A result never depends on earlier calls, failed ones included.
///
/// # Examples
///
/// ```
/// use bz2ngrs::{Compression, Decoder, Encoder};
///
/// let data = b"hello hello hello hello";
/// let stream = Encoder::new(Compression::BEST).compress_to_vec(data);
///
/// let mut decoder = Decoder::new();
/// // The decompressed size is known.
/// assert_eq!(decoder.decompress_to_vec(&stream, data.len()).unwrap(), data);
/// // It is not: the output grows, up to a bound.
/// assert_eq!(decoder.decompress_all_to_vec(&stream, 1 << 30).unwrap(), data);
/// // Into a caller's buffer.
/// let mut out = [0u8; 64];
/// let n = decoder.decompress(&stream, &mut out).unwrap();
/// assert_eq!(&out[..n], data);
/// ```
pub struct Decoder {
    level: fearless_simd::Level,
    scratch: Option<Box<Scratch>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// A decoder that has allocated nothing yet.
    pub fn new() -> Self {
        let level = fearless_simd::Level::new();
        // bz2-ng measured no gain from AVX-512, and its byte shuffle (`vpermb`)
        // sits on the MTF dependency chain where AVX2's `pshufb` is cheaper.
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let level = level.as_avx2().map_or(level, fearless_simd::Level::Avx2);
        Self::__with_level(level)
    }

    /// [`Decoder::new`] with the SIMD level fixed, so tests and fuzz targets
    /// can run every kernel at every level the CPU supports,
    /// `Level::fallback()` included. Not part of the stable API.
    #[doc(hidden)]
    pub fn __with_level(level: fearless_simd::Level) -> Self {
        Decoder {
            level,
            scratch: None,
        }
    }

    /// Decompresses the single bzip2 stream at the start of `src` into `out`
    /// and returns how many bytes it wrote. Bytes after the end-of-stream
    /// marker are ignored, as `bzip2::read::BzDecoder` and libbz2's
    /// `BZ2_bzDecompress` do.
    ///
    /// Accepts and rejects exactly the streams libbz2 1.0.8 does. For a
    /// stream libbz2 accepts, returns [`Error::OutputTooSmall`] as soon as the
    /// output would pass `out.len()`; a corrupt stream may report its
    /// corruption first.
    ///
    /// Never panics, whatever `src` holds. On success the bytes of `out`
    /// past the returned length are untouched; on error `out` is
    /// unspecified.
    pub fn decompress(&mut self, src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        self.decompress_prefix(src, out).map(|d| d.written)
    }

    /// [`Decoder::decompress`], which also says where in `src` the stream
    /// ends, for a stream that other data follows.
    ///
    /// ```
    /// use bz2ngrs::{Compression, Decoded, Decoder, Encoder};
    ///
    /// let mut src = Encoder::new(Compression::BEST).compress_to_vec(b"hello");
    /// let len = src.len();
    /// src.extend_from_slice(b"trailer");
    ///
    /// let mut out = [0u8; 16];
    /// let d = Decoder::new().decompress_prefix(&src, &mut out).unwrap();
    /// assert_eq!(d, Decoded { written: 5, read: len });
    /// assert_eq!(&src[d.read..], b"trailer");
    /// ```
    pub fn decompress_prefix(&mut self, src: &[u8], out: &mut [u8]) -> Result<Decoded, Error> {
        let sc = self.scratch.get_or_insert_with(Scratch::new_boxed);
        let (written, read) = fearless_simd::dispatch!(
            self.level,
            simd => decode::decode_stream(simd, src, out, sc)
        )?;
        Ok(Decoded { written, read })
    }

    /// Decompresses all of `src`, one stream or several back to back (as
    /// `pbzip2`, `lbzip2` and `cat a.bz2 b.bz2` make them), and appends the
    /// bytes to `out`. Returns how many it appended.
    ///
    /// `out` grows one block at a time and by at most `limit` bytes in all:
    /// a stream that decodes to more is [`Error::OutputTooSmall`] with
    /// `capacity: limit`. bzip2 can expand data a thousandfold, so untrusted
    /// input needs a `limit` the caller can afford; `usize::MAX` trusts it.
    ///
    /// Every byte of `src` must belong to a stream: bytes after a stream
    /// that do not start another are [`Error::NotBzip2`] (or
    /// [`Error::UnexpectedEof`] if they could start one), as with the
    /// `bzip2` crate's `MultiBzDecoder`. An error leaves `out` as it was.
    ///
    /// ```
    /// use bz2ngrs::{Compression, Decoder, Encoder, Error};
    ///
    /// let mut enc = Encoder::new(Compression::BEST);
    /// let mut src = enc.compress_to_vec(b"hello, ");
    /// enc.compress(b"world", &mut src);
    ///
    /// let mut out = Vec::new();
    /// let mut decoder = Decoder::new();
    /// assert_eq!(decoder.decompress_all(&src, &mut out, 1 << 20), Ok(12));
    /// assert_eq!(out, b"hello, world");
    /// assert_eq!(
    ///     decoder.decompress_all(&src, &mut out, 11),
    ///     Err(Error::OutputTooSmall { capacity: 11 })
    /// );
    /// assert_eq!(out, b"hello, world");
    /// ```
    pub fn decompress_all(
        &mut self,
        src: &[u8],
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<usize, Error> {
        let sc = self.scratch.get_or_insert_with(Scratch::new_boxed);
        fearless_simd::dispatch!(self.level, simd => decode::decode_all(simd, src, out, limit, sc))
    }

    /// [`Decoder::decompress_all`] into a new `Vec`.
    pub fn decompress_all_to_vec(&mut self, src: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.decompress_all(src, &mut out, limit)?;
        Ok(out)
    }

    /// Decompresses the stream at the start of `src` into a new `Vec` of
    /// capacity `size`, for a stream whose decompressed size is declared
    /// (as a CRAM block header does). A shorter stream gives a shorter
    /// `Vec`; a longer one is [`Error::OutputTooSmall`] with `capacity:
    /// size`. `size` is allocated up front, so bound it if it is untrusted.
    ///
    /// ```
    /// use std::io::Write;
    /// use bz2ngrs::{Decoder, Error};
    ///
    /// let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
    /// enc.write_all(b"hello hello hello hello").unwrap();
    /// let stream = enc.finish().unwrap();
    ///
    /// let mut decoder = Decoder::new();
    /// assert_eq!(decoder.decompress_to_vec(&stream, 23).unwrap().len(), 23);
    /// assert_eq!(decoder.decompress_to_vec(&stream, 100).unwrap().len(), 23);
    /// assert_eq!(
    ///     decoder.decompress_to_vec(&stream, 22),
    ///     Err(Error::OutputTooSmall { capacity: 22 })
    /// );
    /// ```
    pub fn decompress_to_vec(&mut self, src: &[u8], size: usize) -> Result<Vec<u8>, Error> {
        let mut out = vec![0; size];
        let n = self.decompress(src, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

/// What [`Decoder::decompress_prefix`] wrote and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decoded {
    /// Bytes written to `out`.
    pub written: usize,
    /// Bytes of `src` the stream takes up: through the byte that holds the
    /// end of its combined CRC (the rest of that byte is padding).
    pub read: usize,
}

/// A bzip2 compression level, 1 to 9: the block size in units of 100,000
/// bytes. A larger block usually compresses better and needs more memory;
/// the speed per byte barely depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Compression(u8);

impl Compression {
    /// Level 9 (900,000-byte blocks), what the `bzip2` program and htslib
    /// use by default.
    pub const BEST: Compression = Compression(9);
    /// Level 1 (100,000-byte blocks).
    pub const FAST: Compression = Compression(1);

    /// The level `level`, or `None` outside 1 to 9.
    #[allow(clippy::cast_possible_truncation, reason = "the level is below 10")]
    pub const fn new(level: u32) -> Option<Compression> {
        match level {
            1..=9 => Some(Compression(level as u8)),
            _ => None,
        }
    }

    /// The level, 1 to 9.
    pub const fn level(self) -> u32 {
        self.0 as u32
    }
}

impl Default for Compression {
    fn default() -> Self {
        Compression::BEST
    }
}

/// Encoder for whole inputs in memory; [`CompressWriter`] compresses a
/// `Write` piece by piece.
///
/// It owns the working memory of a compression,
/// about 10 bytes per byte of the largest block it has met (9 MB at level
/// 9), allocated on the first call and kept until it is dropped, so keep
/// one per thread and reuse it. A result never depends on earlier calls.
///
/// The output is byte-identical to libbz2 1.0.8's for one
/// `BZ2_bzBuffToBuffCompress` of the same input and level, with one
/// exception. A block that is exactly periodic after run-length coding (a
/// string repeated a whole number of times, such as all zeros) has several
/// valid BWT origins, and libbz2's choice depends on which of its sorters
/// ran. This always writes the last of the tied rows, as bz2-ng does. The
/// streams differ in those 24 bits only and decode identically.
///
/// # Examples
///
/// ```
/// use bz2ngrs::{Compression, Decoder, Encoder};
///
/// let data = b"hello hello hello hello";
/// let mut encoder = Encoder::new(Compression::BEST);
/// let stream = encoder.compress_to_vec(data);
/// assert_eq!(Decoder::new().decompress_to_vec(&stream, data.len()).unwrap(), data);
///
/// // Appended to a buffer.
/// let mut block = b"header".to_vec();
/// encoder.compress(data, &mut block);
/// assert_eq!(&block[6..], stream);
/// ```
pub struct Encoder {
    level: fearless_simd::Level,
    compression: Compression,
    scratch: Option<Box<enc::Scratch>>,
}

impl Encoder {
    /// An encoder at `compression` that has allocated nothing yet.
    pub fn new(compression: Compression) -> Self {
        Self::__with_level(compression, fearless_simd::Level::new())
    }

    /// [`Encoder::new`] with the SIMD level fixed, so tests and fuzz targets
    /// can run every kernel at every level the CPU supports,
    /// `Level::fallback()` included. Not part of the stable API.
    #[doc(hidden)]
    pub fn __with_level(compression: Compression, level: fearless_simd::Level) -> Self {
        Encoder {
            level,
            compression,
            scratch: None,
        }
    }

    /// The level this encoder compresses at.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// Compresses all of `src` as one bzip2 stream and appends it to `out`.
    /// Cannot fail. `out` is grown to each block's worst case (up to 2 MB
    /// at level 9) while the block is coded, so its capacity may end up
    /// larger than the stream.
    pub fn compress(&mut self, src: &[u8], out: &mut Vec<u8>) {
        let sc = self.scratch.get_or_insert_with(enc::Scratch::new_boxed);
        let level = self.compression.0;
        fearless_simd::dispatch!(self.level, simd => enc::compress_stream(simd, src, level, out, sc));
    }

    /// Compresses all of `src` as one bzip2 stream into a new `Vec`.
    pub fn compress_to_vec(&mut self, src: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        self.compress(src, &mut out);
        out
    }
}

impl fmt::Debug for Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encoder")
            .field("level", &self.level)
            .field("compression", &self.compression)
            .field("allocated", &self.scratch.is_some())
            .finish()
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder")
            .field("level", &self.level)
            .field("allocated", &self.scratch.is_some())
            .finish()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "tests"
)]
mod tests;
