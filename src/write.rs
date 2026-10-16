//! A streaming encoder over [`std::io::Write`].

use std::fmt;
use std::io::{self, Write};

use crate::{Compression, Encoder, enc};

/// Compresses everything written to it as one bzip2 stream into an inner
/// writer.
///
/// The stream is byte-identical to what [`Encoder::compress`] writes for the
/// concatenation of all the input, however the input is split into `write`
/// calls. Call [`finish`](CompressWriter::finish) to end the stream and to
/// see its errors; a writer that is dropped first ends the stream as well,
/// but ignores errors, as `flate2` does.
///
/// Output appears one block at a time: nothing reaches the inner writer
/// until a block (`level` × 100 kB of run-length-coded input) is full and
/// more input follows it. The writer holds that block, about 10 bytes of
/// working memory per byte of it, and the compressed bytes the inner writer
/// has not accepted yet. A single large `write` call is compressed in full
/// before it returns, so that much compressed output is buffered if the
/// inner writer refuses it.
///
/// An error from the inner writer loses no output: the bytes are kept and
/// the next call retries them, in order.
///
/// # Examples
///
/// ```
/// use std::io::Write;
/// use bz2ngrs::{CompressWriter, Compression, Decoder};
///
/// let data = b"hello hello hello hello";
/// let mut w = CompressWriter::new(Vec::new(), Compression::BEST);
/// w.write_all(&data[..5])?;
/// w.write_all(&data[5..])?;
/// let stream = w.finish()?;
/// assert_eq!(Decoder::new().decompress_to_vec(&stream, data.len()).unwrap(), data);
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct CompressWriter<W: Write> {
    /// `None` only once `finish` has taken it, which drops the writer.
    inner: Option<W>,
    enc: Encoder,
    stream: Option<enc::Stream>,
    ended: bool,
    /// Compressed bytes; `pending[sent..]` are not yet written to `inner`.
    pending: Vec<u8>,
    sent: usize,
}

impl<W: Write> CompressWriter<W> {
    /// A writer that compresses into `inner` at `compression`.
    pub fn new(inner: W, compression: Compression) -> Self {
        Self::__with_level(inner, compression, fearless_simd::Level::new())
    }

    /// [`CompressWriter::new`] with the SIMD level fixed, so tests can run
    /// every kernel at every level. Not part of the stable API.
    #[doc(hidden)]
    pub fn __with_level(inner: W, compression: Compression, level: fearless_simd::Level) -> Self {
        CompressWriter {
            inner: Some(inner),
            enc: Encoder::__with_level(compression, level),
            stream: None,
            ended: false,
            pending: Vec::new(),
            sent: 0,
        }
    }

    /// The inner writer.
    #[allow(clippy::expect_used, reason = "inner is None only inside finish")]
    pub fn get_ref(&self) -> &W {
        self.inner.as_ref().expect("inner is present until finish")
    }

    /// The inner writer, mutably. Writing to it directly corrupts the
    /// stream.
    #[allow(clippy::expect_used, reason = "inner is None only inside finish")]
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.as_mut().expect("inner is present until finish")
    }

    /// Ends the stream (last block, end-of-stream marker, combined CRC),
    /// writes it out, flushes the inner writer and returns it.
    #[allow(clippy::expect_used, reason = "inner is None only inside finish")]
    pub fn finish(mut self) -> io::Result<W> {
        self.end()?;
        Ok(self.inner.take().expect("inner is present until finish"))
    }

    /// Ends the current stream into the old writer as
    /// [`finish`](CompressWriter::finish) does, then starts a new stream into
    /// `inner`, keeping the encoder's working memory. Returns the old writer.
    ///
    /// If ending the stream fails, `inner` is dropped and the writer stays
    /// ended, so that further writes fail; call `reset` again to retry.
    #[allow(clippy::expect_used, reason = "inner is None only inside finish")]
    pub fn reset(&mut self, inner: W) -> io::Result<W> {
        self.end()?;
        self.stream = None;
        self.ended = false;
        Ok(self
            .inner
            .replace(inner)
            .expect("inner is present until finish"))
    }

    /// Feeds `input` to the stream, or ends it if `None`; the compressed
    /// bytes are appended to `pending`.
    fn run(&mut self, input: Option<&[u8]>) {
        let level = self.enc.level;
        let compression = self.enc.compression.0;
        let mut out = std::mem::take(&mut self.pending);
        let mut sc = self
            .enc
            .scratch
            .take()
            .unwrap_or_else(enc::Scratch::new_boxed);
        let mut stream = self.stream.take();
        fearless_simd::dispatch!(level, simd => {
            let hint = input.map_or(0, |_| usize::MAX);
            let s = stream.take().unwrap_or_else(|| {
                enc::Stream::start(simd, compression, hint, &mut out, &mut sc)
            });
            match input {
                Some(buf) => {
                    let mut s = s;
                    s.push(simd, &mut sc, buf, &mut out);
                    stream = Some(s);
                }
                None => s.finish(simd, &mut sc, &mut out),
            }
        });
        self.stream = stream;
        self.enc.scratch = Some(sc);
        self.pending = out;
    }

    /// Ends the stream if it is not ended, then writes all of it out.
    fn end(&mut self) -> io::Result<()> {
        if !self.ended {
            self.run(None);
            self.ended = true;
        }
        self.drain()?;
        self.get_mut().flush()
    }

    /// Writes `pending` to `inner`, keeping what it does not accept.
    fn drain(&mut self) -> io::Result<()> {
        while self.sent < self.pending.len() {
            let Self {
                inner,
                pending,
                sent,
                ..
            } = self;
            #[allow(clippy::expect_used, reason = "inner is None only inside finish")]
            let inner = inner.as_mut().expect("inner is present until finish");
            match inner.write(&pending[*sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => *sent += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.pending.clear();
        self.sent = 0;
        Ok(())
    }
}

impl<W: Write> Write for CompressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.drain()?;
        if self.ended {
            return Err(io::Error::other("the stream has ended"));
        }
        self.run(Some(buf));
        // The input is consumed either way; the next call retries the rest.
        let _ = self.drain();
        Ok(buf.len())
    }

    /// Writes the compressed bytes that exist and flushes the inner writer.
    /// It does not end a block, which would change the stream.
    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.get_mut().flush()
    }
}

impl<W: Write> Drop for CompressWriter<W> {
    /// Ends the stream if [`finish`](CompressWriter::finish) has not;
    /// errors are ignored, so call `finish` to see them.
    fn drop(&mut self) {
        if self.inner.is_some() {
            let _ = self.end();
        }
    }
}

impl<W: Write + fmt::Debug> fmt::Debug for CompressWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompressWriter")
            .field("inner", &self.inner)
            .field("encoder", &self.enc)
            .field("ended", &self.ended)
            .field("buffered", &(self.pending.len() - self.sent))
            .finish()
    }
}
