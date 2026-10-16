//! The `bzip2` crate (libbz2 1.0.8 via `libbz2-rs-sys`) as an oracle, plus the
//! decoders under test. No hegel in here: the fuzz targets include this file
//! with `#[path]`.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::{self, Read, Write};

use bz2ngrs::Error;
use bzip2::Compression;
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use fearless_simd::Level;

/// One way of making a `bz2ngrs::Decoder`: `Decoder::new()`, or a fixed SIMD
/// level. Each thread keeps one long-lived decoder per variant for
/// [`Decoder::run`], so every call after the first is a reuse, as in a reader.
#[derive(Clone, Copy)]
pub enum Decoder {
    New,
    At(Level),
}

thread_local! {
    static REUSED: RefCell<Vec<(String, bz2ngrs::Decoder)>> = const { RefCell::new(Vec::new()) };
}

impl Decoder {
    pub fn make(self) -> bz2ngrs::Decoder {
        match self {
            Decoder::New => bz2ngrs::Decoder::new(),
            Decoder::At(level) => bz2ngrs::Decoder::__with_level(level),
        }
    }

    pub fn with_reused<T>(self, f: impl FnOnce(&mut bz2ngrs::Decoder) -> T) -> T {
        let key = self.name();
        REUSED.with(|cell| {
            let mut all = cell.borrow_mut();
            let i = all.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
                all.push((key, self.make()));
                all.len() - 1
            });
            f(&mut all[i].1)
        })
    }

    /// Decodes with this thread's long-lived decoder.
    pub fn run(self, src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        self.with_reused(|d| d.decompress(src, out))
    }

    /// A `DecompressReader` over `inner` at this decoder's SIMD level.
    pub fn reader<R: Read>(self, inner: R) -> bz2ngrs::DecompressReader<R> {
        match self {
            Decoder::New => bz2ngrs::DecompressReader::new(inner),
            Decoder::At(level) => bz2ngrs::DecompressReader::__with_level(inner, level),
        }
    }

    /// Decodes with a decoder made for this one call.
    pub fn run_fresh(self, src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        self.make().decompress(src, out)
    }

    /// `Decoder::decompress_to_vec`, on the long-lived decoder.
    pub fn run_to_vec(self, src: &[u8], size: usize) -> Result<Vec<u8>, Error> {
        self.with_reused(|d| d.decompress_to_vec(src, size))
    }

    /// [`Decoder::run`] with the error reduced to its [`Kind`].
    pub fn run_kind(self, src: &[u8], out: &mut [u8]) -> Result<usize, Kind> {
        self.run(src, out).map_err(|e| kind(&e))
    }

    pub fn name(self) -> String {
        match self {
            Decoder::New => "Decoder::new()".into(),
            Decoder::At(level) => format!("Decoder::__with_level({level:?})"),
        }
    }
}

/// The kinds of error the tests tell apart. Every comparison that ignores the
/// payload goes through [`Decoder::run_kind`], so a change to the shape of
/// `Error` is made here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NotBzip2,
    Corrupt,
    BlockCrc,
    StreamCrc,
    UnexpectedEof,
    OutputTooSmall,
    Other,
}

pub fn kind(e: &Error) -> Kind {
    match e {
        Error::NotBzip2 => Kind::NotBzip2,
        Error::Corrupt { .. } => Kind::Corrupt,
        Error::BlockCrc { .. } => Kind::BlockCrc,
        Error::StreamCrc { .. } => Kind::StreamCrc,
        Error::UnexpectedEof { .. } => Kind::UnexpectedEof,
        Error::OutputTooSmall { .. } => Kind::OutputTooSmall,
        _ => Kind::Other,
    }
}

/// The `Decoder` surface: `Decoder::new()` and every level this CPU can run,
/// the scalar `Fallback` included (the dev-dependency compiles it in on every
/// target).
pub fn all_decoders() -> Vec<Decoder> {
    let best = Level::new();
    #[allow(unused_mut, reason = "only x86 has levels below the best one")]
    let mut levels = vec![Level::fallback(), best];
    #[cfg(target_arch = "x86_64")]
    {
        levels.extend(best.as_sse2().map(Level::Sse2));
        levels.extend(best.as_sse4_2().map(Level::Sse4_2));
        levels.extend(best.as_avx2().map(Level::Avx2));
    }
    let mut all = vec![Decoder::New];
    all.extend(levels.into_iter().map(Decoder::At));
    all
}

/// Compresses `data` as one stream at bzip2 level `level` (1 to 9).
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    compress_pieces(&[data], level)
}

/// Compresses the pieces into one stream, ending the current block after each
/// piece (`BZ_FLUSH`) when there is more than one, so every non-empty piece
/// gets its own block.
pub fn compress_pieces(pieces: &[&[u8]], level: u32) -> Vec<u8> {
    let mut enc = BzEncoder::new(Vec::new(), Compression::new(level));
    for piece in pieces {
        enc.write_all(piece).expect("write to Vec");
        if pieces.len() > 1 {
            enc.flush().expect("flush to Vec");
        }
    }
    enc.finish().expect("finish into Vec")
}

/// What the oracle makes of a stream when it may produce at most `cap` bytes.
#[derive(Debug)]
pub enum Reference {
    /// One stream decoded, to at most `cap` bytes.
    Ok(Vec<u8>),
    /// The output reached `cap + 1` bytes.
    Exceeds,
    /// libbz2 rejected the stream: `UnexpectedEof` for input that ends early,
    /// `InvalidInput` for everything else.
    Err(io::ErrorKind),
}

pub fn reference(src: &[u8], cap: usize) -> Reference {
    let mut out = Vec::with_capacity(cap.saturating_add(1));
    // `BzDecoder` (not `MultiBzDecoder`) stops after one stream and ignores
    // what follows, which is our contract.
    match BzDecoder::new(src)
        .take(cap as u64 + 1)
        .read_to_end(&mut out)
    {
        Ok(_) if out.len() > cap => Reference::Exceeds,
        Ok(_) => Reference::Ok(out),
        Err(e) => Reference::Err(e.kind()),
    }
}

/// How libbz2 ends `src` when it may write as much as it likes (up to 256 MiB,
/// beyond which the stream is taken to be fine): `Ok` for a stream it
/// accepts, else the kind of its error.
fn reference_outcome(src: &[u8]) -> Result<(), io::ErrorKind> {
    const LIMIT: usize = 256 << 20;
    let mut dec = BzDecoder::new(src);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0;
    while total < LIMIT {
        match dec.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => total += n,
            Err(e) => return Err(e.kind()),
        }
    }
    Ok(())
}

/// The class of an error: what libbz2 can tell apart (the input ended early,
/// the input is bad), and the caller's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    EarlyEnd,
    BadInput,
    OutputFull,
}

pub fn class(e: &Error) -> Class {
    match e {
        Error::UnexpectedEof { .. } => Class::EarlyEnd,
        Error::OutputTooSmall { .. } => Class::OutputFull,
        _ => Class::BadInput,
    }
}

pub fn io_class(kind: io::ErrorKind) -> Class {
    if kind == io::ErrorKind::UnexpectedEof {
        Class::EarlyEnd
    } else {
        Class::BadInput
    }
}

/// The differential rule: decode `src` into `cap` bytes and compare with the
/// oracle's verdict. Oracle `Ok` means ours is `Ok` with the same bytes. An
/// oracle error or overflow means ours is an `Err` of the same class:
/// - input that ends early is `UnexpectedEof`, with `bit` the input's length;
/// - input libbz2 rejects is `NotBzip2`, `Corrupt`, `BlockCrc` or `StreamCrc`,
///   or `OutputTooSmall` if the output passed `cap` first (libbz2 checks the
///   last byte of a block against the CRC in the call that writes it);
/// - a stream that passes `cap` is `OutputTooSmall { capacity: cap }` if
///   libbz2 accepts it, and otherwise may be that or its own error (a corrupt
///   stream may report its corruption first).
///
/// Returns what the oracle said, for coverage statistics.
pub fn check_against_reference(decoder: Decoder, src: &[u8], cap: usize) -> &'static str {
    let want = reference(src, cap);
    let mut out = vec![0xA5; cap];
    let got = decoder.run(src, &mut out);
    check_outcome(&decoder.name(), src, &want, &got, &out, cap)
}

/// The long-lived decoder and a decoder made for this call give the same
/// result on `src`, into `cap` bytes.
pub fn check_fresh_agrees(decoder: Decoder, src: &[u8], cap: usize) {
    let (mut reused, mut fresh) = (vec![0xA5; cap], vec![0xA5; cap]);
    let a = decoder.run(src, &mut reused);
    let b = decoder.run_fresh(src, &mut fresh);
    assert_eq!(a, b, "{}: reused vs fresh decoder", decoder.name());
    if let Ok(n) = a {
        assert!(
            reused[..n] == fresh[..n],
            "{}: bytes differ from a fresh decoder's",
            decoder.name()
        );
    }
}

/// The differential rule applied to a finished call on `src`: `got` is its result and
/// `out` the `cap` bytes it wrote into, filled with 0xA5 beforehand so that a
/// write past the returned length shows.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn check_outcome(
    who: &str,
    src: &[u8],
    want: &Reference,
    got: &Result<usize, Error>,
    out: &[u8],
    cap: usize,
) -> &'static str {
    match (want, got) {
        (Reference::Ok(bytes), Ok(n)) => {
            assert_eq!(*n, bytes.len(), "{who}: length (cap {cap})");
            assert!(
                out[..*n] == bytes[..],
                "{who}: bytes differ from the reference"
            );
            assert!(
                out[*n..].iter().all(|&b| b == 0xA5),
                "{who}: wrote past the returned length"
            );
        }
        (Reference::Ok(bytes), Err(e)) => {
            panic!(
                "{who}: reference decodes {} bytes, we fail with {e:?} (cap {cap})",
                bytes.len()
            )
        }
        (Reference::Exceeds | Reference::Err(_), Ok(n)) => {
            panic!("{who}: reference gives {want:?}, we return Ok({n}) (cap {cap})")
        }
        (Reference::Exceeds, Err(e)) => match (reference_outcome(src), class(e)) {
            (_, Class::OutputFull) => assert_eq!(
                *e,
                Error::OutputTooSmall { capacity: cap },
                "{who}: reference exceeds cap {cap}"
            ),
            (Err(kind), c) => {
                assert_eq!(
                    io_class(kind),
                    c,
                    "{who}: reference exceeds cap, we say {e:?}"
                )
            }
            (Ok(()), _) => {
                panic!("{who}: reference accepts a stream longer than cap {cap}, we say {e:?}")
            }
        },
        (Reference::Err(kind), Err(e)) => {
            let c = class(e);
            if c == Class::OutputFull {
                assert!(
                    reference_outcome(src).is_err(),
                    "{who}: reference accepts the stream, we say {e:?}"
                );
            } else {
                assert_eq!(
                    io_class(*kind),
                    c,
                    "{who}: reference gives {kind:?}, we say {e:?}"
                );
            }
            match e {
                Error::UnexpectedEof { at } => assert_eq!(
                    at.bit,
                    src.len() as u64 * 8,
                    "{who}: an early end is at the end of the input"
                ),
                Error::Corrupt { at, .. } => assert!(
                    at.bit <= src.len() as u64 * 8,
                    "{who}: position {at} is past the input"
                ),
                _ => {}
            }
        }
    }
    match want {
        Reference::Ok(_) => "reference ok",
        Reference::Exceeds => "reference exceeds cap",
        Reference::Err(io::ErrorKind::UnexpectedEof) => "reference unexpected eof",
        Reference::Err(_) => "reference invalid input",
    }
}

/// Decodes into exactly `expected.len()` bytes and demands the exact bytes.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn check_exact(decoder: Decoder, src: &[u8], expected: &[u8]) {
    let mut out = vec![0xA5; expected.len()];
    match decoder.run(src, &mut out) {
        Ok(n) => {
            assert_eq!(n, expected.len(), "{}: length", decoder.name());
            assert!(out == expected, "{}: bytes differ", decoder.name());
        }
        Err(e) => panic!("{}: valid stream rejected with {e:?}", decoder.name()),
    }
}

/// `len` bits of `bytes` from bit `start`, most significant bit first.
pub fn read_bits(bytes: &[u8], start: usize, len: usize) -> u64 {
    (start..start + len).fold(0, |acc, i| {
        (acc << 1) | u64::from((bytes[i / 8] >> (7 - i % 8)) & 1)
    })
}

/// Overwrites the `len` bits from bit `start` with the low `len` bits of
/// `value`, most significant bit first.
pub fn write_bits(bytes: &mut [u8], start: usize, len: usize, value: u64) {
    for i in 0..len {
        let bit = (value >> (len - 1 - i)) & 1;
        let at = start + i;
        bytes[at / 8] = (bytes[at / 8] & !(0x80 >> (at % 8))) | ((bit as u8) << (7 - at % 8));
    }
}

/// Flips bit `at` (counted from the most significant bit of byte 0).
pub fn flip_bit(bytes: &mut [u8], at: usize) {
    bytes[at / 8] ^= 0x80 >> (at % 8);
}

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;

/// Bit offset of the first block's stored CRC (the 32 bits after the block
/// magic, which is byte aligned in the first block), if there is a block.
pub fn first_block_crc_bit(stream: &[u8]) -> Option<usize> {
    (stream.len() >= 14 && read_bits(stream, 32, 48) == BLOCK_MAGIC).then_some(80)
}

/// Bit offset of the stream's combined CRC: the 32 bits after the end-of-stream
/// magic, followed by 0 to 7 padding bits.
pub fn stream_crc_bit(stream: &[u8]) -> Option<usize> {
    (0..8usize).find_map(|pad| {
        let crc = (stream.len() * 8).checked_sub(pad + 32)?;
        let magic = crc.checked_sub(48).filter(|&m| m >= 32)?;
        (read_bits(stream, magic, 48) == END_MAGIC).then_some(crc)
    })
}

/// Bit offsets of the block magics of `stream`, found at any bit offset. A
/// false positive needs 48 matching bits inside compressed data.
pub fn block_starts(stream: &[u8]) -> Vec<usize> {
    let mut window = 0u64;
    let mut starts = Vec::new();
    for (i, bit) in stream
        .iter()
        .flat_map(|b| (0..8).map(move |k| (b >> (7 - k)) & 1))
        .enumerate()
    {
        window = ((window << 1) | u64::from(bit)) & ((1 << 48) - 1);
        if i >= 47 + 32 && window == BLOCK_MAGIC {
            starts.push(i - 47);
        }
    }
    starts
}

/// How many blocks `stream` holds.
pub fn count_blocks(stream: &[u8]) -> usize {
    block_starts(stream).len()
}
