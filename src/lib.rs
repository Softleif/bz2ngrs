//! Fast one-shot bzip2 decompression: a Rust port of the decoder of
//! bz2-ng, the fast libbz2 replacement in C, for callers that hold a whole
//! compressed stream in memory and know its uncompressed size (CRAM blocks).
//!
//! # Memory
//!
//! Each thread that calls [`decompress`] keeps its working memory for later
//! calls: about 6 MiB of address space, allocated zeroed on first use. Only
//! the pages a block touches become resident: about 6 bytes per byte of the
//! largest block (after bzip2's first run-length stage) decoded on that
//! thread, plus about 40 KiB. That is at most about 5.5 MB for `BZh9` streams
//! and 3.7 MB for `BZh6`.

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

use core::cell::Cell;

pub use error::{Corruption, Error, Position};

use decode::Scratch;

/// Decompresses the single bzip2 stream at the start of `src` into `out` and
/// returns how many bytes it wrote. Bytes after the end-of-stream marker are
/// ignored, as `bzip2::read::BzDecoder` and libbz2's `BZ2_bzDecompress` do.
///
/// Accepts and rejects exactly the streams libbz2 1.0.8 does. For a stream
/// libbz2 accepts, returns [`Error::OutputTooSmall`] as soon as the output
/// would pass `out.len()`; a corrupt stream may report its corruption first.
///
/// Never panics, whatever `src` holds (a failure to allocate the per-thread
/// memory aborts, as Rust's allocation failures do). On success the bytes of
/// `out` past the returned length are untouched. On error the contents of
/// `out` are unspecified.
pub fn decompress(src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let level = fearless_simd::Level::new();
    // bz2-ng measured no gain from AVX-512, and its byte shuffle (`vpermb`)
    // sits on the MTF dependency chain where AVX2's `pshufb` is cheaper.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let level = level.as_avx2().map_or(level, fearless_simd::Level::Avx2);
    __decompress_at(level, src, out)
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
    thread_local! {
        static SCRATCH: Cell<Option<Box<Scratch>>> = const { Cell::new(None) };
    }
    // Take the scratch out for the call, so no borrow is held; a fresh one
    // serves if the thread-local is gone (thread teardown).
    let mut sc = SCRATCH
        .try_with(Cell::take)
        .ok()
        .flatten()
        .unwrap_or_else(Scratch::new_boxed);
    let r = fearless_simd::dispatch!(level, simd => decode::decode_stream(simd, src, out, &mut sc));
    let _ = SCRATCH.try_with(|c| c.set(Some(sc)));
    r
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "tests"
)]
mod tests;
