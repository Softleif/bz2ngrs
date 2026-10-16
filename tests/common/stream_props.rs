//! Property bodies for the streaming surface: `decompress_prefix`,
//! `decompress_all`, `decompress_all_to_vec`, `DecompressReader` and
//! `From<Error> for io::Error`.
//!
//! The oracle for several streams is `bzip2::read::MultiBzDecoder`. For the
//! bytes a reader returns before an error the truth is what was compressed
//! (the pieces), because libbz2 drops the bytes of the read call that fails.
#![allow(
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "a failed property is a panic; test-sized counts"
)]

use std::io::{self, BufRead, Read};

use bz2ngrs::{Decoded, DecompressReader, Error, Position};
use bzip2::read::MultiBzDecoder;
use hegel::TestCase;
use hegel::generators::{self as gs};

use super::forge::{self, OrigClass, forge};
use super::inputs::{self, Valid};
use super::oracle::{
    Class, Decoder, Reference, block_starts, check_outcome, class, compress_pieces, flip_bit,
    io_class, read_bits, reference, stream_crc_bit,
};

fn below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
}

// ---------------------------------------------------------------- sources

/// Input for the multi-stream decoders and what is known about it.
#[derive(Debug, Clone)]
pub struct Source {
    pub what: &'static str,
    pub src: Vec<u8>,
    /// About how much it decodes to, for choosing an output limit.
    pub hint: usize,
}

#[hegel::composite]
fn valid_streams(tc: &TestCase) -> Vec<Valid> {
    tc.draw(gs::vecs(inputs::valid_stream()).min_size(1).max_size(3))
}

fn join(vs: &[Valid]) -> (Vec<u8>, Vec<u8>) {
    (
        vs.iter().flat_map(|v| v.stream.iter().copied()).collect(),
        vs.iter().flat_map(|v| v.data.iter().copied()).collect(),
    )
}

/// Bytes that follow a stream: a partial or full header, zeros, noise, or a
/// stream cut short.
#[hegel::composite]
fn junk(tc: &TestCase) -> Vec<u8> {
    match tc.draw(gs::integers::<u8>().max_value(4)) {
        0 => tc.draw(gs::binary().max_size(64)),
        1 => b"BZh9"[..1 + below(tc, 4)].to_vec(),
        2 => vec![0; 1 + below(tc, 20)],
        3 => {
            let mut h = tc.draw(inputs::hostile_bzh());
            h.truncate(4 + below(tc, 30));
            h
        }
        _ => {
            let s = tc.draw(inputs::valid_stream()).stream;
            s[..below(tc, s.len())].to_vec()
        }
    }
}

/// Clean concatenations, junk after them, mutated ones, forged streams
/// (valid or defective) alone or after them, and hostile bytes after them.
#[hegel::composite]
pub fn source(tc: &TestCase) -> Source {
    let (mut src, data) = join(&tc.draw(valid_streams()));
    let mut hint = data.len();
    let what = match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => "clean",
        1 => {
            src.extend(tc.draw(junk()));
            "junk after"
        }
        2 => {
            for m in tc.draw(gs::vecs(inputs::mutation()).min_size(1).max_size(3)) {
                inputs::apply(&mut src, &m);
            }
            "mutated"
        }
        3 | 4 => {
            let spec = if tc.draw(gs::booleans()) {
                tc.draw(forge::several_blocks())
            } else {
                tc.draw(forge::any_defective()).spec
            };
            let f = forge(&spec);
            if tc.draw(gs::booleans()) {
                src.clear();
                hint = 0;
            }
            src.extend(&f.stream);
            hint += f.cap;
            "forged"
        }
        _ => {
            src.extend(tc.draw(inputs::hostile_bzh()));
            "hostile after"
        }
    };
    Source { what, src, hint }
}

// ---------------------------------------------------------------- oracle

#[derive(Debug)]
pub enum Multi {
    Ok(Vec<u8>),
    Exceeds,
    Err(io::ErrorKind),
}

impl Multi {
    fn is_err_kind(&self, k: io::ErrorKind) -> bool {
        matches!(self, Multi::Err(e) if *e == k)
    }
}

/// `MultiBzDecoder` over `src`, stopping once the output passes `cap`; also
/// returns the bytes it gave out before an error.
fn multi_partial(src: &[u8], cap: usize) -> (Multi, Vec<u8>) {
    let mut dec = MultiBzDecoder::new(src);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 1 << 14];
    loop {
        let room = (cap - out.len() + 1).min(buf.len());
        match dec.read(&mut buf[..room]) {
            Ok(0) => return (Multi::Ok(out.clone()), out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => return (Multi::Err(e.kind()), out),
        }
        if out.len() > cap {
            return (Multi::Exceeds, out);
        }
    }
}

pub fn multi_reference(src: &[u8], cap: usize) -> Multi {
    multi_partial(src, cap).0
}

fn multi_unbounded(src: &[u8]) -> Result<(), io::ErrorKind> {
    let mut dec = MultiBzDecoder::new(src);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0usize;
    while total < 256 << 20 {
        match dec.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => total += n,
            Err(e) => return Err(e.kind()),
        }
    }
    Ok(())
}

/// The differential rule for `decompress_all` against `MultiBzDecoder`, as
/// `oracle::check_outcome` is for one stream. `prefix` is what `out` held
/// before the call; `out` is it after. An error leaves `out` as it was.
pub fn check_all(
    who: &str,
    src: &[u8],
    limit: usize,
    prefix: &[u8],
    out: &[u8],
    got: &Result<usize, Error>,
) -> &'static str {
    let want = multi_reference(src, limit);
    match (&want, got) {
        (Multi::Ok(bytes), Ok(n)) => {
            assert_eq!(*n, bytes.len(), "{who}: length (limit {limit})");
            assert!(out[..prefix.len()] == *prefix, "{who}: prefix overwritten");
            assert!(out[prefix.len()..] == bytes[..], "{who}: bytes differ");
        }
        (Multi::Ok(bytes), Err(e)) => {
            panic!("{who}: oracle decodes {} bytes, we say {e:?}", bytes.len())
        }
        (Multi::Exceeds | Multi::Err(_), Ok(n)) => {
            panic!("{who}: oracle gives {want:?}, we return Ok({n}) (limit {limit})")
        }
        (Multi::Exceeds, Err(e)) => match (multi_unbounded(src), class(e)) {
            (_, Class::OutputFull) => assert_eq!(
                *e,
                Error::OutputTooSmall { capacity: limit },
                "{who}: oracle exceeds limit {limit}"
            ),
            (Err(k), c) => assert_eq!(io_class(k), c, "{who}: oracle exceeds, we say {e:?}"),
            (Ok(()), _) => panic!("{who}: oracle accepts more than {limit}, we say {e:?}"),
        },
        (Multi::Err(kind), Err(e)) => {
            let c = class(e);
            if c == Class::OutputFull {
                assert!(
                    multi_unbounded(src).is_err(),
                    "{who}: oracle accepts the input, we say {e:?}"
                );
            } else {
                assert_eq!(
                    io_class(*kind),
                    c,
                    "{who}: oracle gives {kind:?}, we say {e:?}"
                );
            }
            match e {
                Error::UnexpectedEof { at } => assert_eq!(
                    at.bit,
                    src.len() as u64 * 8,
                    "{who}: an early end is at the end of the input"
                ),
                Error::Corrupt { at, .. } => assert!(at.bit <= src.len() as u64 * 8),
                _ => {}
            }
        }
    }
    if got.is_err() {
        assert!(out == prefix, "{who}: an error must leave `out` as it was");
    }
    match want {
        Multi::Ok(_) => "oracle ok",
        Multi::Exceeds => "oracle exceeds limit",
        Multi::Err(io::ErrorKind::UnexpectedEof) => "oracle unexpected eof",
        Multi::Err(_) => "oracle invalid",
    }
}

fn run_all(d: Decoder, src: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    d.with_reused(|x| x.decompress_all(src, out, limit))
}

fn run_all_fresh(d: Decoder, src: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    d.make().decompress_all(src, out, limit)
}

// ---------------------------------------------------------- decompress_prefix

/// A valid stream followed by anything: `read` is the stream's length, the
/// bytes are the data, and nothing past `written` is touched.
pub fn prefix_with_trailing(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let trailing = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => tc.draw(gs::binary().max_size(3000)),
        1 => tc.draw(inputs::valid_stream()).stream,
        _ => tc.draw(junk()),
    };
    let mut src = v.stream.clone();
    src.extend_from_slice(&trailing);
    let slack = below(tc, 400);
    for d in decoders {
        let mut out = vec![0xA5; v.data.len() + slack];
        let got = d.with_reused(|x| x.decompress_prefix(&src, &mut out));
        let want = Decoded {
            written: v.data.len(),
            read: v.stream.len(),
        };
        assert_eq!(got, Ok(want), "{}", d.name());
        assert!(out[..want.written] == v.data[..], "{}: bytes", d.name());
        assert!(
            out[want.written..].iter().all(|&b| b == 0xA5),
            "{}: wrote past `written`",
            d.name()
        );
    }
}

/// On any input, `decompress_prefix` follows the oracle's single-stream
/// verdict and its `written` is `decompress`'s result; `read` is the least
/// prefix of the input that decodes (one byte less ends early).
pub fn prefix_agrees(tc: &TestCase, decoders: &[Decoder]) {
    let s = tc.draw(source());
    let cap = tc.draw(inputs::output_cap(s.hint));
    let want = reference(&s.src, cap);
    for d in decoders {
        let mut out = vec![0xA5; cap];
        let got = d.with_reused(|x| x.decompress_prefix(&s.src, &mut out));
        let n = got.map(|g| g.written);
        tc.event(check_outcome(&d.name(), &s.src, &want, &n, &out, cap));
        let mut plain = vec![0xA5; cap];
        assert_eq!(n, d.run(&s.src, &mut plain), "{}: vs decompress", d.name());
        assert!(out == plain, "{}: bytes differ from decompress", d.name());
        if let Ok(Decoded { written, read }) = got {
            assert!(
                read <= s.src.len() && read >= 1,
                "{}: read {read}",
                d.name()
            );
            let mut again = vec![0xA5; written];
            assert_eq!(
                d.run(&s.src[..read], &mut again),
                Ok(written),
                "{}: src[..read]",
                d.name()
            );
            assert!(
                matches!(
                    reference(&s.src[..read - 1], written),
                    Reference::Err(io::ErrorKind::UnexpectedEof)
                ),
                "premise: libbz2 needs the last byte of the stream"
            );
            assert!(
                matches!(
                    d.run(&s.src[..read - 1], &mut again),
                    Err(Error::UnexpectedEof { .. })
                ),
                "{}: one byte short of `read` must end early",
                d.name()
            );
        }
    }
}

// ----------------------------------------------------------- decompress_all

pub fn all_agrees_into_vec(tc: &TestCase, decoders: &[Decoder]) {
    all_agrees(tc, decoders, false);
}

pub fn all_to_vec_agrees(tc: &TestCase, decoders: &[Decoder]) {
    all_agrees(tc, decoders, true);
}

/// `decompress_all` (or `_to_vec`) on any input, appending to a prefix,
/// against `MultiBzDecoder`.
fn all_agrees(tc: &TestCase, decoders: &[Decoder], to_vec: bool) {
    let s = tc.draw(source());
    tc.event(s.what);
    let limit = tc.draw(inputs::output_cap(s.hint));
    let prefix = if to_vec {
        Vec::new()
    } else {
        tc.draw(gs::binary().max_size(40))
    };
    for d in decoders {
        let (got, out) = if to_vec {
            let r = d.with_reused(|x| x.decompress_all_to_vec(&s.src, limit));
            (
                r.as_ref().map(Vec::len).map_err(|e| *e),
                r.unwrap_or_default(),
            )
        } else {
            let mut out = prefix.clone();
            (run_all(*d, &s.src, &mut out, limit), out)
        };
        tc.event(check_all(&d.name(), &s.src, limit, &prefix, &out, &got));
    }
}

/// Too small a limit is `OutputTooSmall { capacity: limit }` and leaves `out`
/// alone; the truth is the length the encoder was given.
pub fn all_limit_too_small(tc: &TestCase, decoders: &[Decoder]) {
    let (src, data) = join(&tc.draw(valid_streams()));
    tc.assume(!data.is_empty());
    let limit = if tc.draw(gs::booleans()) {
        data.len() - 1
    } else {
        below(tc, data.len())
    };
    let prefix = tc.draw(gs::binary().max_size(40));
    for d in decoders {
        let mut out = prefix.clone();
        assert_eq!(
            run_all(*d, &src, &mut out, limit),
            Err(Error::OutputTooSmall { capacity: limit }),
            "{}",
            d.name()
        );
        assert!(out == prefix, "{}: `out` changed", d.name());
        assert_eq!(
            d.with_reused(|x| x.decompress_all_to_vec(&src, limit)),
            Err(Error::OutputTooSmall { capacity: limit }),
            "{}: to_vec",
            d.name()
        );
    }
}

/// A limit of at least the output's length succeeds and appends exactly the
/// data after what `out` held.
pub fn all_limit_enough(tc: &TestCase, decoders: &[Decoder]) {
    let (src, data) = join(&tc.draw(valid_streams()));
    let limit = match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => data.len(),
        1 => usize::MAX,
        _ => data.len() + below(tc, 100_000),
    };
    let prefix = tc.draw(gs::binary().max_size(40));
    for d in decoders {
        let mut out = prefix.clone();
        assert_eq!(
            run_all(*d, &src, &mut out, limit),
            Ok(data.len()),
            "{}",
            d.name()
        );
        assert!(out[..prefix.len()] == prefix[..], "{}: prefix", d.name());
        assert!(out[prefix.len()..] == data[..], "{}: bytes", d.name());
    }
}

/// A decoder that failed on other inputs gives what a fresh one gives (and
/// what the oracle says).
pub fn all_reuse_independent(tc: &TestCase, decoders: &[Decoder]) {
    let steps: Vec<(Source, usize)> = (0..2 + below(tc, 4))
        .map(|_| {
            let s = tc.draw(source());
            let l = tc.draw(inputs::output_cap(s.hint));
            (s, l)
        })
        .collect();
    for d in decoders {
        for (s, limit) in &steps {
            let (mut a, mut b) = (vec![7u8; 3], vec![7u8; 3]);
            let got = run_all(*d, &s.src, &mut a, *limit);
            let fresh = run_all_fresh(*d, &s.src, &mut b, *limit);
            assert_eq!(got, fresh, "{} ({}): reused vs fresh", d.name(), s.what);
            assert!(a == b, "{} ({}): bytes differ from fresh", d.name(), s.what);
            check_all(&d.name(), &s.src, *limit, &[7; 3], &a, &got);
        }
    }
}

/// `From<Error> for io::Error`: the kind, and the error inside.
pub fn into_io_error(tc: &TestCase, decoders: &[Decoder]) {
    let s = tc.draw(source());
    for d in decoders {
        let mut out = Vec::new();
        if let Err(e) = run_all(*d, &s.src, &mut out, 1 << 22) {
            let io: io::Error = e.into();
            let kind = if matches!(e, Error::UnexpectedEof { .. }) {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            };
            assert_eq!(io.kind(), kind, "{}: {e:?}", d.name());
            assert_eq!(error_of(&io), Some(e), "{}", d.name());
        }
    }
}

// ------------------------------------------------------------------- reader

fn error_of(e: &io::Error) -> Option<Error> {
    e.get_ref()?.downcast_ref::<Error>().copied()
}

#[derive(Debug, Clone, Copy)]
pub struct Piece {
    size: usize,
    interrupt: bool,
}

/// An inner reader that returns drawn-size pieces, sometimes `Interrupted`,
/// and fails once with `ConnectionReset` when it reaches `fail_at`.
pub struct Chunked {
    data: Vec<u8>,
    pos: usize,
    plan: Vec<Piece>,
    next: usize,
    last_interrupted: bool,
    fail_at: Option<usize>,
    pub failures: usize,
}

impl Read for Chunked {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let p = self.plan[self.next % self.plan.len()];
        self.next += 1;
        if p.interrupt && !self.last_interrupted {
            self.last_interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.last_interrupted = false;
        let mut n = p.size.min(buf.len()).min(self.data.len() - self.pos);
        if let Some(f) = self.fail_at {
            if self.pos == f {
                self.fail_at = None;
                self.failures += 1;
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "injected"));
            }
            if self.pos < f {
                n = n.min(f - self.pos);
            }
        }
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// How an input is cut into reads and read from. Pieces are not tiny for big
/// inputs: the reader asks `inner` for a megabyte on every call, so a read
/// count in the thousands is slow however it is implemented.
#[derive(Debug, Clone)]
pub struct Plan {
    pieces: Vec<Piece>,
    ops: Vec<u32>,
    fail: Option<usize>,
}

#[hegel::composite]
pub fn plan(tc: &TestCase, len: usize) -> Plan {
    let min = if len <= 600 { 1 } else { len / 100 };
    let pieces = tc
        .draw(gs::vecs(gs::integers::<u32>()).min_size(1).max_size(8))
        .into_iter()
        .map(|v| {
            let bound = [1usize, 7, 100, 4096, 70_000, 1 << 21][(v >> 8) as usize % 6];
            Piece {
                size: (1 + v as usize % bound).max(min),
                interrupt: v & 0x80 != 0,
            }
        })
        .collect();
    let ops = tc.draw(gs::vecs(gs::integers::<u32>()).min_size(1).max_size(8));
    let fail = tc
        .draw(gs::booleans())
        .then(|| tc.draw(gs::integers::<u32>()) as usize % (len + 1));
    Plan { pieces, ops, fail }
}

impl Plan {
    fn inner(&self, data: &[u8], with_failure: bool) -> Chunked {
        Chunked {
            data: data.to_vec(),
            pos: 0,
            plan: self.pieces.clone(),
            next: 0,
            last_interrupted: false,
            fail_at: if with_failure { self.fail } else { None },
            failures: 0,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub bytes: Vec<u8>,
    /// The data error that ended the reading.
    pub err: Option<Error>,
    /// Reached the end of the data, or a data error.
    pub done: bool,
    /// Errors from `inner` that were passed on.
    pub foreign: usize,
}

fn same_error(a: &io::Result<impl Sized>, b: &Error) -> bool {
    matches!(a, Err(e) if error_of(e).as_ref() == Some(b))
}

/// Reads `r` by the drawn operations (reads of several sizes, `fill_buf` with
/// partial `consume`) until the end, a data error, or `stop_after`
/// operations. Foreign errors are counted and retried.
pub fn drain<R: Read>(
    r: &mut DecompressReader<R>,
    ops: &[u32],
    stop_after: Option<usize>,
) -> Outcome {
    let mut o = Outcome {
        bytes: Vec::new(),
        err: None,
        done: false,
        foreign: 0,
    };
    for i in 0..10_000_000usize {
        if stop_after == Some(i) {
            return o;
        }
        let op = ops[i % ops.len()];
        // Every third operation is a plain read, so there is always progress.
        let kind = if i % 3 == 2 { 0 } else { op % 4 };
        let arg = (op >> 2) as usize;
        let res: io::Result<bool> = match kind {
            0 => {
                let mut buf = vec![0u8; 1 + arg % 5000];
                r.read(&mut buf).map(|n| {
                    o.bytes.extend_from_slice(&buf[..n]);
                    n > 0
                })
            }
            1 => r
                .fill_buf()
                .map(|a| {
                    let k = if a.is_empty() { 0 } else { 1 + arg % a.len() };
                    a[..k].to_vec()
                })
                .map(|taken| {
                    o.bytes.extend_from_slice(&taken);
                    r.consume(taken.len());
                    !taken.is_empty()
                }),
            2 => match r.read(&mut []) {
                Ok(0) => Ok(true),
                other => panic!("an empty read gave {other:?}"),
            },
            _ => r.fill_buf().map(<[u8]>::to_vec).and_then(|a1| {
                let a2 = r.fill_buf()?.to_vec();
                assert!(a1 == a2, "fill_buf twice gave different data");
                o.bytes.extend_from_slice(&a1);
                r.consume(a1.len());
                Ok(!a1.is_empty())
            }),
        };
        match res {
            Ok(true) => {}
            Ok(false) => {
                o.done = true;
                assert_eq!(r.read(&mut [0u8; 8]).ok(), Some(0), "read after the end");
                assert!(
                    r.fill_buf().is_ok_and(<[u8]>::is_empty),
                    "fill_buf after the end"
                );
                return o;
            }
            Err(e) => match error_of(&e) {
                Some(err) => {
                    let kind = if matches!(err, Error::UnexpectedEof { .. }) {
                        io::ErrorKind::UnexpectedEof
                    } else {
                        io::ErrorKind::InvalidData
                    };
                    assert_eq!(e.kind(), kind, "{err:?}");
                    assert!(same_error(&r.read(&mut [0u8; 8]), &err), "sticky read");
                    assert!(same_error(&r.fill_buf(), &err), "sticky fill_buf");
                    assert!(
                        same_error(&r.read(&mut [0u8; 8]), &err),
                        "sticky read again"
                    );
                    o.err = Some(err);
                    o.done = true;
                    return o;
                }
                None => {
                    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset, "foreign: {e:?}");
                    assert_eq!(e.to_string(), "injected");
                    o.foreign += 1;
                }
            },
        }
    }
    panic!("the reader makes no progress");
}

fn read_all(d: Decoder, data: &[u8], p: &Plan, with_failure: bool) -> Outcome {
    let mut r = d.reader(p.inner(data, with_failure));
    let o = drain(&mut r, &p.ops, None);
    assert_eq!(
        o.foreign,
        r.get_ref().failures,
        "{}: every injected error reaches the caller once",
        d.name()
    );
    o
}

/// Valid concatenations through any chunking and operation mix equal
/// `MultiBzDecoder`'s bytes and the data that was compressed.
pub fn reader_valid(tc: &TestCase, decoders: &[Decoder]) {
    let (src, data) = join(&tc.draw(valid_streams()));
    let p = tc.draw(plan(src.len()));
    let (oracle, _) = multi_partial(&src, usize::MAX - 1);
    assert!(
        matches!(oracle, Multi::Ok(ref b) if *b == data),
        "premise: oracle"
    );
    for d in decoders {
        let o = read_all(*d, &src, &p, true);
        assert!(o.err.is_none() && o.done, "{}: {:?}", d.name(), o.err);
        assert!(o.bytes == data, "{}: bytes differ", d.name());
    }
}

/// On any input the reader errors iff `decompress_all` does, with the same
/// error (positions from the first byte), and its bytes before the error
/// agree with `MultiBzDecoder`'s: one is a prefix of the other (libbz2 drops
/// the bytes of the call that fails, the reader gives whole blocks).
pub fn reader_agrees_with_all(tc: &TestCase, decoders: &[Decoder]) {
    let s = tc.draw(source());
    tc.event(s.what);
    let p = tc.draw(plan(s.src.len()));
    let limit = 1 << 25;
    let (oracle, partial) = multi_partial(&s.src, limit);
    tc.assume(!matches!(oracle, Multi::Exceeds));
    for d in decoders {
        let mut all = Vec::new();
        let want = run_all(*d, &s.src, &mut all, limit);
        tc.assume(!matches!(want, Err(Error::OutputTooSmall { .. })));
        let o = read_all(*d, &s.src, &p, true);
        assert!(o.done, "{}", d.name());
        match want {
            Ok(_) => {
                assert!(
                    o.err.is_none(),
                    "{}: reader fails with {:?}",
                    d.name(),
                    o.err
                );
                assert!(
                    o.bytes == all,
                    "{}: bytes differ from decompress_all",
                    d.name()
                );
            }
            Err(e) => {
                assert_eq!(o.err, Some(e), "{}", d.name());
                let n = o.bytes.len().min(partial.len());
                assert!(
                    o.bytes[..n] == partial[..n],
                    "{}: bytes before the error disagree with the oracle",
                    d.name()
                );
            }
        }
    }
}

/// A stream of one block per non-empty piece.
struct Blocks {
    pieces: Vec<Vec<u8>>,
    stream: Vec<u8>,
    starts: Vec<usize>,
}

fn blocks(tc: &TestCase) -> Blocks {
    let pieces: Vec<Vec<u8>> = tc
        .draw(inputs::pieces())
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    tc.assume(!pieces.is_empty());
    let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    let stream = compress_pieces(&refs, tc.draw(inputs::level()));
    let starts = block_starts(&stream);
    assert_eq!(starts.len(), pieces.len(), "premise: a block per piece");
    Blocks {
        pieces,
        stream,
        starts,
    }
}

/// The reader returns exactly the blocks that are whole before the cut and
/// then `UnexpectedEof` at the end of the input, in the block the cut is in;
/// the truth is the pieces that were compressed.
pub fn reader_truncated(tc: &TestCase, decoders: &[Decoder]) {
    let b = blocks(tc);
    let head = tc
        .draw(gs::booleans())
        .then(|| tc.draw(inputs::valid_stream()));
    let min = usize::from(head.is_some());
    let cut = min + below(tc, b.stream.len() - min);
    let mut src: Vec<u8> = head.iter().flat_map(|h| h.stream.clone()).collect();
    src.extend_from_slice(&b.stream[..cut]);
    let end = stream_crc_bit(&b.stream).expect("premise: end marker");
    let mut marks = b.starts.clone();
    marks.push(end - 48);
    let block = marks.iter().rposition(|&m| m <= cut * 8).unwrap_or(0);
    let mut want: Vec<u8> = head.iter().flat_map(|h| h.data.clone()).collect();
    want.extend(b.pieces[..block].concat());
    assert!(
        multi_partial(&src, usize::MAX - 1)
            .0
            .is_err_kind(io::ErrorKind::UnexpectedEof),
        "premise: the oracle ends early"
    );
    let p = tc.draw(plan(src.len()));
    let err = Error::UnexpectedEof {
        at: Position {
            block: block as u32,
            bit: src.len() as u64 * 8,
        },
    };
    for d in decoders {
        let o = read_all(*d, &src, &p, true);
        assert_eq!(o.err, Some(err), "{}", d.name());
        assert!(o.bytes == want, "{}: bytes before the cut", d.name());
        let mut out = Vec::new();
        assert_eq!(
            run_all(*d, &src, &mut out, usize::MAX),
            Err(err),
            "{}",
            d.name()
        );
    }
}

/// A wrong CRC in block `k` gives blocks `0..k` and then `BlockCrc` for
/// block `k`: none of its bytes, and nothing after it.
pub fn reader_block_crc(tc: &TestCase, decoders: &[Decoder]) {
    let b = blocks(tc);
    let head = tc.draw(inputs::valid_stream());
    let tail = tc
        .draw(gs::booleans())
        .then(|| tc.draw(inputs::valid_stream()));
    let k = below(tc, b.pieces.len());
    let crc_bit = b.starts[k] + 48;
    let computed = read_bits(&b.stream, crc_bit, 32) as u32;
    let flip = below(tc, 32);
    let mut bad = b.stream.clone();
    flip_bit(&mut bad, crc_bit + flip);
    let mut src = head.stream.clone();
    src.extend_from_slice(&bad);
    src.extend(tail.iter().flat_map(|t| t.stream.clone()));
    let mut want = head.data.clone();
    want.extend(b.pieces[..k].concat());
    let err = Error::BlockCrc {
        block: k as u32,
        stored: computed ^ (0x8000_0000 >> flip),
        computed,
    };
    let p = tc.draw(plan(src.len()));
    for d in decoders {
        let o = read_all(*d, &src, &p, true);
        assert_eq!(o.err, Some(err), "{}", d.name());
        assert!(o.bytes == want, "{}: bytes before the bad block", d.name());
        let mut out = vec![9];
        assert_eq!(
            run_all(*d, &src, &mut out, usize::MAX),
            Err(err),
            "{}",
            d.name()
        );
        assert_eq!(out, [9]);
    }
}

/// One life of a reader: an input read by a plan, abandoned part way or not.
#[derive(Debug, Clone)]
struct Life {
    s: Source,
    plan: Plan,
    stop: Option<usize>,
}

#[hegel::composite]
fn life(tc: &TestCase) -> Life {
    let s = tc.draw(source());
    let plan = tc.draw(plan(s.src.len()));
    let stop = tc.draw(gs::booleans()).then(|| below(tc, 6));
    Life { s, plan, stop }
}

/// A reader reset onto new inputs (some abandoned part way, some failed)
/// reads each as a fresh reader does, and as `decompress_all` says.
pub fn reader_reset(tc: &TestCase, decoders: &[Decoder]) {
    let lives = tc.draw(gs::vecs(life()).min_size(2).max_size(5));
    for d in decoders {
        let mut reused = d.reader(lives[0].plan.inner(&lives[0].s.src, true));
        for (i, l) in lives.iter().enumerate() {
            if i > 0 {
                reused.reset(l.plan.inner(&l.s.src, true));
            }
            let got = drain(&mut reused, &l.plan.ops, l.stop);
            let mut fresh = d.reader(l.plan.inner(&l.s.src, true));
            let want = drain(&mut fresh, &l.plan.ops, l.stop);
            assert_eq!(
                (&got.bytes, got.err, got.done, got.foreign),
                (&want.bytes, want.err, want.done, want.foreign),
                "{} ({}): reset vs fresh",
                d.name(),
                l.s.what
            );
            if got.done {
                let mut all = Vec::new();
                match run_all(*d, &l.s.src, &mut all, 1 << 25) {
                    Ok(_) => assert!(got.err.is_none() && got.bytes == all, "{}", d.name()),
                    Err(Error::OutputTooSmall { .. }) => {}
                    Err(e) => assert_eq!(got.err, Some(e), "{}", d.name()),
                }
            }
        }
    }
}

/// A block of 17 to 20 bit codes, about 900 000 symbols: over 1.9 MB of
/// compressed input for one block, more than the reader's 1 MiB read-ahead.
#[hegel::composite]
pub fn long_code_block(tc: &TestCase) -> forge::Spec {
    let mut r = tc.draw(forge::recipe(9, 3000, OrigClass::InRange));
    r.shape = forge::Shape::Random;
    r.alphabet = 256;
    r.n = forge::max_rows(9) - below(tc, 2000);
    r.orig_ptr = below(tc, r.n) as u32;
    r.extra_selectors = 0;
    r.varied = false;
    r.defect = None;
    r.table_extra = (0..2 + below(tc, 5))
        .map(|_| (128 + 12 * below(tc, 10) + below(tc, 4)) as u8)
        .collect();
    forge::Spec {
        digit: 9,
        blocks: vec![r],
    }
}

/// A block that does not fit the reader's read-ahead decodes to what
/// `decompress_all` and the oracle say, however the input arrives.
pub fn reader_large_block(tc: &TestCase, decoders: &[Decoder]) {
    let f = forge(&tc.draw(long_code_block()));
    assert!(
        f.stream.len() > (1 << 20) + 50_000,
        "premise: the block is longer than the read-ahead"
    );
    let mut src = f.stream.clone();
    let mut more = Vec::new();
    if tc.draw(gs::booleans()) {
        let v = tc.draw(inputs::valid_stream());
        src.extend(v.stream);
        more = v.data;
    }
    let p = tc.draw(plan(src.len()));
    let (oracle, _) = multi_partial(&src, usize::MAX - 1);
    for d in decoders {
        let mut all = Vec::new();
        let want = run_all(*d, &src, &mut all, usize::MAX);
        check_all(&d.name(), &src, usize::MAX - 1, &[], &all, &want);
        let o = read_all(*d, &src, &p, true);
        match (&oracle, want) {
            (Multi::Ok(b), Ok(_)) => {
                assert!(
                    o.err.is_none() && o.bytes == *b,
                    "{}: large block",
                    d.name()
                );
            }
            (_, Err(e)) => assert_eq!(o.err, Some(e), "{}", d.name()),
            _ => panic!("{}: oracle {oracle:?} vs ours disagree", d.name()),
        }
        if let Some(e) = &f.expected {
            assert!(
                o.bytes == [e.as_slice(), &more].concat(),
                "{}: forged expectation",
                d.name()
            );
        }
    }
}
