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
mod error;
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "values bounded by the format"
)]
mod huffman;
mod unrle;

use core::fmt;

pub use error::{Corruption, Error, Position};

use decode::Scratch;

/// Reusable decoder state. Keep one per reader or thread and reuse it for
/// every stream: it owns the working memory a decode needs (about 6 MiB of
/// address space). For a single stream, `Decoder::new().decompress(src, out)`
/// allocates that memory, uses it once and frees it.
///
/// Nothing is allocated until the first decode, so [`Decoder::new`] is free
/// and a reader that never meets bzip2 pays nothing. The memory then lives
/// until the `Decoder` is dropped, and is never shrunk in between. The SIMD
/// level is chosen once, in [`Decoder::new`].
///
/// A result never depends on earlier calls: a `Decoder` that has decoded
/// other streams, or failed on them, gives the same answer as a new one. It
/// is `Send`, so a reader can move to another thread with its decoder.
///
/// # Examples
///
/// ```
/// use std::io::Write;
///
/// let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
/// enc.write_all(b"hello hello hello hello").unwrap();
/// let stream = enc.finish().unwrap();
///
/// let mut decoder = bz2ngrs::Decoder::new();
/// // A block that declares its uncompressed size: allocate it, decode into it.
/// let block = decoder.decompress_to_vec(&stream, 23).unwrap();
/// assert_eq!(block, b"hello hello hello hello");
/// // Later blocks reuse the same memory.
/// let mut out = [0u8; 64];
/// let n = decoder.decompress(&stream, &mut out).unwrap();
/// assert_eq!(&out[..n], b"hello hello hello hello");
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
    /// Never panics, whatever `src` holds (a failure to allocate the working
    /// memory on the first call aborts, as Rust's allocation failures do). On
    /// success the bytes of `out` past the returned length are untouched. On
    /// error the contents of `out` are unspecified.
    ///
    /// A caller that pools its output buffers keeps each one at full length
    /// and reads `out[..n]`; this call never zeroes or resizes `out`.
    pub fn decompress(&mut self, src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        let sc = self.scratch.get_or_insert_with(Scratch::new_boxed);
        fearless_simd::dispatch!(self.level, simd => decode::decode_stream(simd, src, out, sc))
    }

    /// Decompresses the stream at the start of `src` into a new `Vec`, for a
    /// block whose uncompressed size is declared (a CRAM block header says
    /// it). Allocates `size` bytes, decodes, and truncates to the bytes
    /// written. A stream shorter than `size` is allowed, which a caller that
    /// wants exact sizes checks with `len()`. A longer one is
    /// [`Error::OutputTooSmall`] with `capacity: size`.
    ///
    /// `size` comes from the file being read, so a caller that distrusts it
    /// bounds it first: the allocation is `size` bytes, and its failure
    /// aborts. The `Vec` keeps capacity `size` even when shorter.
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
