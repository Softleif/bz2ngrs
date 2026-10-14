//! The public error type, and the decoder's internal failure code that is
//! turned into it.

use core::fmt;

/// Why a stream could not be decompressed.
///
/// libbz2 reports one of four codes; this carries what the decoder knew at
/// the point of failure, so a bug report can say which block broke and which
/// rule it broke. Every variant is `Copy` and allocation-free.
///
/// Only [`Error::OutputTooSmall`] is about the caller's buffer; every other
/// variant means the input is bad:
///
/// ```
/// use std::io::Write;
/// use bz2ngrs::{Error, decompress};
///
/// let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
/// enc.write_all(b"hello hello hello hello").unwrap();
/// let stream = enc.finish().unwrap();
///
/// let mut small = [0u8; 4];
/// match decompress(&stream, &mut small) {
///     Err(Error::OutputTooSmall { capacity }) => assert_eq!(capacity, 4),
///     other => panic!("expected OutputTooSmall, got {other:?}"),
/// }
///
/// let mut out = [0u8; 64];
/// match decompress(b"this is not bzip2", &mut out) {
///     Err(Error::OutputTooSmall { .. }) => unreachable!(),
///     Err(bad_input) => println!("bad input: {bad_input}"),
///     Ok(n) => println!("{n} bytes"),
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The input does not start with `BZh1` to `BZh9` (libbz2's
    /// `BZ_DATA_ERROR_MAGIC`). Input that ends inside those four bytes is
    /// [`Error::UnexpectedEof`] instead.
    NotBzip2,
    /// The stream breaks a format rule.
    Corrupt {
        /// The rule.
        kind: Corruption,
        /// Where the decoder detected it.
        at: Position,
    },
    /// A block's stored CRC disagrees with the bytes the block decoded to.
    BlockCrc {
        /// 0-based index of the block.
        block: u32,
        /// The CRC the stream holds.
        stored: u32,
        /// The CRC of the decoded bytes.
        computed: u32,
    },
    /// The stream's combined CRC, after the end-of-stream marker, disagrees
    /// with the blocks' CRCs.
    StreamCrc {
        /// The CRC the stream holds.
        stored: u32,
        /// The CRC combined from the blocks' CRCs.
        computed: u32,
    },
    /// The input ends before the end-of-stream marker and CRC.
    UnexpectedEof {
        /// Where the input ended: `bit` is `src.len() * 8`.
        at: Position,
    },
    /// The stream decodes to more bytes than `out` holds.
    OutputTooSmall {
        /// The length of `out`.
        capacity: usize,
    },
}

/// Where in a stream a failure was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0-based index of the block being read. A failure in the stream header
    /// is block 0; one in the end-of-stream marker or combined CRC is the
    /// number of blocks.
    pub block: u32,
    /// Bits of `src` consumed when the decoder noticed. It is at or just
    /// after the offending field, not necessarily its first bit.
    pub bit: u64,
}

/// The format rule a corrupt stream breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Corruption {
    /// Neither a block header nor the end-of-stream marker.
    BlockMagic,
    /// `origPtr` is not below the block's length (or is too far beyond the
    /// block size to be).
    OrigPtr,
    /// The block's symbol map is empty.
    NoSymbols,
    /// The number of Huffman tables is not in 2 to 6.
    GroupCount,
    /// The block has no selectors.
    SelectorCount,
    /// A selector's move-to-front index is not below the number of tables.
    Selector,
    /// A code length leaves the range 1 to 20.
    CodeLength,
    /// A bit pattern that no code of the selected table matches.
    HuffmanCode,
    /// The last selector's group ends without the end-of-block symbol.
    NoEndOfBlock,
    /// The block decodes to more bytes than the stream's block size allows.
    BlockTooLong,
    /// A RUNA/RUNB run of 2^21 or more.
    RunTooLong,
    /// Four equal bytes end the block with no count byte after them.
    RunCount,
}

impl Corruption {
    fn describe(self) -> &'static str {
        match self {
            Corruption::BlockMagic => "bad block header magic",
            Corruption::OrigPtr => "origPtr out of range",
            Corruption::NoSymbols => "empty symbol map",
            Corruption::GroupCount => "table count out of range",
            Corruption::SelectorCount => "no selectors",
            Corruption::Selector => "selector out of range",
            Corruption::CodeLength => "code length out of range",
            Corruption::HuffmanCode => "invalid Huffman code",
            Corruption::NoEndOfBlock => "selectors run out before the end of the block",
            Corruption::BlockTooLong => "block longer than the block size",
            Corruption::RunTooLong => "run too long",
            Corruption::RunCount => "run of four without a count byte",
        }
    }
}

impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block {}, bit {}", self.block, self.bit)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotBzip2 => f.write_str("not a bzip2 stream (no BZh1 to BZh9 header)"),
            Error::Corrupt { kind, at } => write!(f, "corrupt bzip2 stream: {kind} ({at})"),
            Error::BlockCrc {
                block,
                stored,
                computed,
            } => write!(
                f,
                "bzip2 block {block} CRC mismatch: stored {stored:#010x}, computed {computed:#010x}"
            ),
            Error::StreamCrc { stored, computed } => write!(
                f,
                "bzip2 stream CRC mismatch: stored {stored:#010x}, computed {computed:#010x}"
            ),
            Error::UnexpectedEof { at } => write!(f, "bzip2 stream ends early ({at})"),
            Error::OutputTooSmall { capacity } => {
                write!(f, "bzip2 stream decodes to more than {capacity} bytes")
            }
        }
    }
}

impl std::error::Error for Error {}

/// A failure as the decoder's helpers see it, before the block index, the
/// reader position and the output capacity are known. One byte, like the
/// flat error it replaces, so the helpers return it as cheaply.
#[derive(Clone, Copy)]
pub(crate) enum Fail {
    /// The input ends too soon.
    Eof,
    /// The stream header is not `BZh1` to `BZh9`.
    NotBzip2,
    Corrupt(Corruption),
    /// The output buffer is full.
    OutputFull,
}

/// The public error for `f`, from the reader's state at the failure. Called
/// with scalars, not the reader, so that the reader's address never escapes
/// and its fields stay in registers.
#[cold]
#[inline(never)]
pub(crate) fn raise(f: Fail, block: u32, consumed: u64, src_bits: u64, capacity: usize) -> Error {
    match f {
        Fail::Eof => Error::UnexpectedEof {
            at: Position {
                block,
                bit: src_bits,
            },
        },
        Fail::NotBzip2 => Error::NotBzip2,
        Fail::Corrupt(kind) => Error::Corrupt {
            kind,
            at: Position {
                block,
                bit: consumed,
            },
        },
        Fail::OutputFull => Error::OutputTooSmall { capacity },
    }
}
