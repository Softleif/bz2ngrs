# Error type: design

libbz2 reports only `BZ_DATA_ERROR`, `BZ_DATA_ERROR_MAGIC`, `BZ_UNEXPECTED_EOF` or `BZ_OUTBUFF_FULL`. The decoder knows far more at the point of failure: which block failed, where in the input, which rule was broken, and the two CRCs. That context is free to carry, because it is built only on the error path. It turns a CRAM bug report ("bzip2 failed") into something that can be acted on.

## Shape

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// `src` does not start with `BZh1`..`BZh9` (libbz2: BZ_DATA_ERROR_MAGIC).
    NotBzip2,
    /// The stream breaks a format rule.
    Corrupt { kind: Corruption, at: Position },
    /// A block's stored CRC disagrees with the bytes it decoded to.
    BlockCrc { block: u32, stored: u32, computed: u32 },
    /// The end-of-stream combined CRC disagrees with the blocks' CRCs.
    StreamCrc { stored: u32, computed: u32 },
    /// The input ends before the end-of-stream marker.
    UnexpectedEof { at: Position },
    /// The stream decodes to more than `capacity` (`out.len()`) bytes.
    OutputTooSmall { capacity: usize },
}

/// Where in the stream: the 0-based block index, and the bits of `src`
/// consumed when the decoder noticed (for EOF, `src.len() * 8`). The bit is at
/// or just after the offending field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position { pub block: u32, pub bit: u64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Corruption {
    BlockMagic,        // neither a block header nor the end-of-stream marker
    OrigPtr,           // origPtr out of range for the block
    NoSymbols,         // empty symbol bitmap
    GroupCount,        // nGroups outside 2..=6
    SelectorCount,     // nSelectors == 0
    Selector,          // selector MTF index >= nGroups
    CodeLength,        // a code length left 1..=20
    HuffmanCode,       // a bit pattern with no symbol (incomplete code set)
    NoEndOfBlock,      // the last selector's group ends without the EOB symbol
    BlockTooLong,      // more bytes than the block size allows (literals or a run)
    RunTooLong,        // a RUNA/RUNB run of 2^21 or more
    RunCount,          // un-RLE1: four equal bytes end the block without a count
}
```

- **Merged and added kinds:** a run that overflows the block is `BlockTooLong`
  like a literal that does (libbz2 has one check for both); `RunTooLong` is
  libbz2's separate 2^21 run-length limit. `NoEndOfBlock` is new. A bad block
  size digit is `NotBzip2`, as in libbz2 (`BZ_DATA_ERROR_MAGIC`).
- **Block index:** `Position.block` is the block being read; for the header 0,
  for the end-of-stream marker and combined CRC the number of blocks.
- **Display:** one line with the context, e.g. `corrupt bzip2 stream: selector out of range (block 3, bit 18203)` or `bzip2 block 0 CRC mismatch: stored 0x1234abcd, computed 0x89ab0123`.
- **No allocation and `Copy`:** seqair wraps it in `CramError` as `source`.
- **Hot path:** the error path is cold. Return through `#[cold]` constructors so error construction stays out of the symbol loop's registers. Measure that the bench does not move.
- **Accept/reject parity is unchanged.** Only the payload is new, and the `OutputTooSmall` precedence stays as tested.
- **Tests:**
  - Map each variant to the oracle's classes where the oracle distinguishes them: magic, data, EOF, output full.
  - Assert the context where it can be checked independently of the decoder. The block index of a CRC corrupted in block k, and the stored/computed values of a deliberately corrupted CRC, are not tautological. Asserting the bit offset the decoder itself reports would be, unless the test computes it from the stream layout it built.
- **seqair:** maps `OutputTooSmall { .. }` to its size errors and everything else to `Bzip2DecompressionFailed { source }`.
