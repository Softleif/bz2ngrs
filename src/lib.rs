//! Fast one-shot bzip2 decompression: a Rust port of the decoder of
//! bz2-ng, the fast libbz2 replacement in C, for callers that hold a whole
//! compressed stream in memory and know its uncompressed size (CRAM blocks).

mod crc;

use core::fmt;

/// Why a stream could not be decompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The input is not a bzip2 stream or is corrupt (bad magic, bad block
    /// header, invalid Huffman tables or selectors, `origPtr` out of range,
    /// a run that overflows the block).
    Corrupt,
    /// A block's CRC or the stream's combined CRC does not match the data.
    CrcMismatch,
    /// The input ends before the end-of-stream marker.
    UnexpectedEof,
    /// The stream decodes to more bytes than `out` holds.
    OutputTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::Corrupt => "corrupt bzip2 stream",
            Error::CrcMismatch => "bzip2 CRC mismatch",
            Error::UnexpectedEof => "bzip2 stream ends early",
            Error::OutputTooSmall => "bzip2 stream decodes past the output buffer",
        })
    }
}

impl std::error::Error for Error {}

/// Decompresses the single bzip2 stream at the start of `src` into `out` and
/// returns how many bytes it wrote. Bytes after the end-of-stream marker are
/// ignored, as `bzip2::read::BzDecoder` and libbz2's `BZ2_bzDecompress` do.
///
/// Never panics, whatever `src` holds. On error the contents of `out` are
/// unspecified.
pub fn decompress(src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let _ = (src, out, crc::update(0, &[]));
    Err(Error::Corrupt)
}

/// [`decompress`] with the SIMD level fixed, so tests and fuzz targets can
/// run every kernel at every level the CPU supports, `Level::fallback()`
/// included. Not part of the stable API.
#[doc(hidden)]
pub fn __decompress_at(
    level: fearless_simd::Level,
    src: &[u8],
    out: &mut [u8],
) -> Result<usize, Error> {
    let _ = level;
    decompress(src, out)
}
