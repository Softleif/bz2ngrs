//! Decompression from a reader, one block at a time.

use core::fmt;
use std::io::{self, BufRead, Read};

use crate::bits::Bits;
use crate::decode::{self, Scratch, Stream};
use crate::{Decoder, Error};

/// Compressed bytes to have at hand before a block is decoded: a little more
/// than the largest block that libbz2 writes for incompressible data at
/// level 9. A longer block (only a hostile encoder writes one) makes the
/// reader read more and decode the block again.
const READ_AHEAD: usize = 1 << 20;

/// Decompresses the bzip2 data that a reader yields.
///
/// It reads all of `inner` until its end: one bzip2 stream or several back
/// to back (as `pbzip2`, `lbzip2` and `cat a.bz2 b.bz2` make them). Every
/// byte must belong to a stream, so data after the last stream is an error,
/// as with the `bzip2` crate's `MultiBzDecoder`.
///
/// Output comes one block at a time: each block is decoded whole and its CRC
/// checked before any of its bytes are returned, so a corrupt block yields an
/// error and none of its bytes. The reader holds the decoder's working memory
/// (about 6 MiB), about 1 MiB of compressed input read ahead, and the
/// current block's output (about 1 MB at level 9 for typical data). It reads
/// `inner` in large pieces, so wrapping `inner` in a `BufReader` gains
/// nothing.
///
/// It also implements [`BufRead`], whose [`fill_buf`](BufRead::fill_buf)
/// hands out the decoded block without a copy.
///
/// Errors are [`io::Error`]s that wrap the [`Error`] (get it back with
/// [`io::Error::get_ref`] and `downcast_ref`). [`Error::UnexpectedEof`] is
/// [`io::ErrorKind::UnexpectedEof`], other bad input
/// [`io::ErrorKind::InvalidData`]; their positions count from the first byte
/// read. Once the data turns out bad, every later read fails the same way.
/// An error from `inner` is passed on as it is, and a later read retries.
///
/// # Examples
///
/// ```
/// use std::io::Read;
/// use bz2ngrs::{Compression, DecompressReader, Encoder};
///
/// let file = Encoder::new(Compression::BEST).compress_to_vec(b"hello world");
///
/// let mut text = String::new();
/// DecompressReader::new(&file[..]).read_to_string(&mut text).unwrap();
/// assert_eq!(text, "hello world");
/// ```
pub struct DecompressReader<R> {
    inner: R,
    decoder: Decoder,
    /// Compressed bytes read from `inner` and not yet dropped.
    input: Vec<u8>,
    /// The next bit of `input` to decode.
    bit: u64,
    /// Bytes read from `inner` before `input[0]`.
    dropped: u64,
    /// Whether `inner` has reported its end.
    eof: bool,
    state: State,
    /// The current block's bytes; `out[pos..end]` are not yet read.
    out: Vec<u8>,
    pos: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
enum State {
    /// Before a stream header; the first stream's is required, later ones
    /// only if there is more input.
    Header {
        first: bool,
    },
    Blocks(Stream),
    Done,
    Failed(Error),
}

impl<R: Read> DecompressReader<R> {
    /// A reader of the bzip2 data in `inner`. Allocates nothing until the
    /// first read.
    pub fn new(inner: R) -> Self {
        Self::with_decoder(inner, Decoder::new())
    }

    /// [`DecompressReader::new`] at a fixed SIMD level, as
    /// [`Decoder::__with_level`]. Not part of the stable API.
    #[doc(hidden)]
    pub fn __with_level(inner: R, level: fearless_simd::Level) -> Self {
        Self::with_decoder(inner, Decoder::__with_level(level))
    }

    fn with_decoder(inner: R, decoder: Decoder) -> Self {
        DecompressReader {
            inner,
            decoder,
            input: Vec::new(),
            bit: 0,
            dropped: 0,
            eof: false,
            state: State::Header { first: true },
            out: Vec::new(),
            pos: 0,
            end: 0,
        }
    }

    /// Starts over on `inner`, keeping the working memory, and returns the
    /// old reader.
    pub fn reset(&mut self, inner: R) -> R {
        self.input.clear();
        self.bit = 0;
        self.dropped = 0;
        self.eof = false;
        self.state = State::Header { first: true };
        self.pos = 0;
        self.end = 0;
        core::mem::replace(&mut self.inner, inner)
    }

    /// Reads from `inner` until at least `want` bytes from the next whole
    /// byte to decode are at hand, or `inner` ends. Drops the bytes before
    /// it first.
    fn fill(&mut self, want: usize) -> io::Result<()> {
        let skip = usize::try_from(self.bit / 8)
            .unwrap_or(usize::MAX)
            .min(self.input.len());
        if skip > 0 {
            self.input.drain(..skip);
            self.bit -= skip as u64 * 8;
            self.dropped += skip as u64;
        }
        while !self.eof && self.input.len() < want {
            let len = self.input.len();
            self.input.resize(want, 0);
            let r = self.inner.read(&mut self.input[len..]);
            let n = *r.as_ref().unwrap_or(&0);
            self.input.truncate(len + n);
            match r {
                Ok(0) => self.eof = true,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads the next stream header, or finds the input's end.
    fn header(&mut self, first: bool) -> io::Result<()> {
        self.fill(4)?;
        if !first && self.eof && self.input.len() as u64 * 8 <= self.bit {
            self.state = State::Done;
            return Ok(());
        }
        let mut br = Bits::at(&self.input, self.bit);
        match Stream::start(&mut br) {
            Ok(st) => {
                self.bit = br.consumed();
                self.state = State::Blocks(st);
            }
            Err(e) => self.state = State::Failed(e.offset(self.dropped * 8)),
        }
        Ok(())
    }

    /// Decodes the next block into `out`, or reads the end of the stream.
    fn block(&mut self, st: Stream) -> io::Result<()> {
        let mut want = READ_AHEAD;
        loop {
            self.fill(want)?;
            let mut next = st;
            let mut br = Bits::at(&self.input, self.bit);
            let sc = self.decoder.scratch.get_or_insert_with(Scratch::new_boxed);
            let r = fearless_simd::dispatch!(
                self.decoder.level,
                simd => decode::next_block(simd, &mut next, &mut br, sc, &mut self.out)
            );
            let consumed = br.consumed();
            match r {
                Ok(Some(n)) => {
                    self.bit = consumed;
                    self.state = State::Blocks(next);
                    self.pos = 0;
                    self.end = n;
                }
                Ok(None) => {
                    self.bit = consumed.next_multiple_of(8);
                    self.state = State::Header { first: false };
                }
                Err(Error::UnexpectedEof { .. }) if !self.eof => {
                    want = self.input.len().saturating_mul(2).max(READ_AHEAD);
                    continue;
                }
                Err(e) => self.state = State::Failed(e.offset(self.dropped * 8)),
            }
            return Ok(());
        }
    }
}

impl<R> DecompressReader<R> {
    /// The reader of compressed data.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The reader of compressed data. Reading from it directly skips data
    /// this reader expects.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// The reader of compressed data. It has been read past what was
    /// decoded so far, by up to about 1 MiB.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> BufRead for DecompressReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        while self.pos == self.end {
            match self.state {
                State::Header { first } => self.header(first)?,
                State::Blocks(st) => self.block(st)?,
                State::Done => break,
                State::Failed(e) => return Err(e.into()),
            }
        }
        Ok(self.out.get(self.pos..self.end).unwrap_or_default())
    }

    fn consume(&mut self, amt: usize) {
        self.pos = self.pos.saturating_add(amt).min(self.end);
    }
}

impl<R: Read> Read for DecompressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let avail = self.fill_buf()?;
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl<R: fmt::Debug> fmt::Debug for DecompressReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecompressReader")
            .field("inner", &self.inner)
            .field("decoder", &self.decoder)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}
