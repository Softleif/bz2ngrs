# bz2-ng → Rust: summary and plan

## Outcome (2026-10-14)

Done: a decode-only `bz2ngrs::Decoder` (`decompress`, `decompress_to_vec`), in use on seqair's `bz2ngrs` branch through a reader-owned `CodecScratch`.

- **Speed (M4 Pro).** 1.08 to 1.27 times bz2-ng C, and 2.9 to 5.0 times the `bzip2` crate, on the 1000 Genomes MC/XA/QUAL data at 16k to 1m (README). On real CRAM blocks: 1.07 to 1.19 times C. Reading the re-encoded Ultima CRAMs through seqair is 2.1 to 3.3 times faster than seqair `main`.
- **Safety.** The only `unsafe` is three CRC kernel dispatch blocks. Hegel suite and fuzzing are in notes 07, 08 and 10.
- **Not done.**
  - x86 speed has not been measured.
  - CI has not run, since there is no remote yet.
  - The encoder is out of scope.

The plan below is the original one, kept for context.

Written 2026-10-14. Source: `/Users/pascal/code/tfenne/bz2-ng` (C, read-only). Consumer: `/Users/pascal/Code/bsblabludwig/seqair`. Detail is in the other notes:

- `01-seqair-usage.md`: what seqair needs from bzip2.
- `02-decoder.md`: the decoder in depth, and how to port it.
- `04-rust-ecosystem.md`: the Rust baseline and building blocks.
- `05-baseline-bench.md`: speeds measured on this machine.

There is no `03-encoder.md`. Its analysis was stopped once it was clear that seqair never compresses.

## Verdict

**Yes, and it is a much smaller job than "translate bz2-ng".** seqair is a CRAM reader and only ever *decompresses* bzip2. It does so at two call sites, both of the form "one stream from a buffer into an output of known size" (`01`). That means the following all drop out of scope:

- the whole encoder, including libsais (8.5k lines of C) and the byte-identical-output contract;
- the resumable `bz_stream` state machine;
- the stdio layer and small mode;
- the in-formula memory layout.

What is left is about 1,100 lines of C, which should become an estimated 1,400 lines of Rust plus about 500 lines of tests (`02` §4).

Nothing found so far rules out keeping C-level speed:

- **SIMD:** every vector operation the decoder kernels use has a portable fearless_simd 1.1 equivalent. The one gap is the NEON movemask: the portable `to_bitmask` takes 5 instructions where the C's `shrn` nibble mask takes 1. That one is written with `kernel!` (`02` §2).
- **Compiler:** bz2-ng was tuned for clang, which is the case that carries over to rustc, since both use LLVM.
- **CRC and prefetch:** both need `core::arch` outside fearless_simd. Every intrinsic involved is stable, except the aarch64 prefetch, which needs one `asm!` line. At seqair's block sizes the C does not prefetch at all anyway.

## What "keep the performance" means

Measured on this M4 Pro (NEON), level 9, independent streams, decompression (`05`):

| Data | Block size | `libbz2-rs-sys` (today) | bz2-ng C (target) | Speed-up |
|---|---|---|---|---|
| MC tags | 64 KiB | 168 MB/s | 435 MB/s | 2.6× |
| MC tags | 1 MiB | 119 MB/s | 463 MB/s | 3.9× |
| XA tags | 64 KiB | 77 MB/s | 270 MB/s | 3.5× |
| XA tags | 1 MiB | 68 MB/s | 284 MB/s | 4.2× |
| Quality strings | 64 KiB | 284 MB/s | 1087 MB/s | 3.8× |
| Quality strings | 1 MiB | 311 MB/s | 1252 MB/s | 4.0× |

`libbz2-rs-sys` runs at roughly the speed of stock C libbz2. **Target: within about 5% of bz2-ng C on this table**, on this machine and on one x86-64 AVX2 machine. seqair's blocks are 20–120 kB, almost always a single bzip2 block written by htslib at `BZh6`. So the 64 KiB rows and the fixed cost of each call matter most.

## Shape of the crate

```rust
/// Decompresses one bzip2 stream from `src` into `out`, whose length is the
/// expected size; returns the number of bytes written. Errors (never panics)
/// on corrupt or truncated input, or if the stream would not fit in `out`.
pub fn decompress(src: &[u8], out: &mut [u8]) -> Result<usize, Error>;
```

- **Reusable scratch:** keep about 5 MB of working buffers per thread (a thread-local, or a `Decoder` value reused across calls). Without that, allocation and page faults dominate on small blocks.
- **Multi-block and concatenated streams:** they must still work, because the format allows them and hostile input will contain them. They are just not the fast case.
- **Bounds checks:** fixed power-of-two scratch arrays with masked indices remove them in the symbol loop, table build and walk without `unsafe`. The input read-ahead takes either one predictable check or one small documented `unsafe` read (`02` §3).
- **seqair conventions to follow:**
  - `Level::new()` → `dispatch!` → `#[simd] fn k<S: Simd>`, with helpers `#[inline(always)]`;
  - tests that run every kernel at every level, including `Fallback` via `force_support_fallback`;
  - MSRV 1.98.1.
- **Licence:** bz2-ng's own code is MIT. The one-shot decoder needs none of the files bz2-ng took from libbz2's BSD-licensed code (the legacy driver), and seqair's `deny.toml` allows `bzip2-1.0.6` in any case.

## Plan

1. **Correct before fast.** Write a safe scalar one-shot decoder with the bz2-ng structure:
   - a 64-bit bit reader;
   - 50-symbol group Huffman decode;
   - the segmented inverse-BWT table build;
   - the multi-chain walk;
   - un-RLE1 straight into `out`.

   Test it differentially against the `bzip2` crate (a dev-dependency, which also generates the inputs at every level), including periodic blocks, randomised blocks and multi-block streams. Add a cargo-fuzz target for round-trip and hostile input.
2. **Benchmark harness.** Turn the scratch crate from `05` into a `benches/` target over the 1000 Genomes MC/XA/QUAL corpora, with bz2-ng C linked as the yardstick via `cc` (dev-only). Measure here how much of the gap the algorithmic changes alone close before any SIMD.
3. **SIMD.**
   - The move-to-front list head in a vector register.
   - The un-RLE1 run scan.
   - The NEON `kernel!` mask.

   Everything goes through fearless_simd dispatch, and tests compare every level with the scalar one.
4. **CRC.**
   - Start with slice-by-16.
   - Then port bz2-ng's PMULL and ARMv8-crc32 kernels (about 180 lines) and the PCLMUL one (about 75 lines) with `core::arch` and their own feature detection.
   - Alternative: the `crc-fast` crate, which supports CRC-32/BZIP2 with PCLMUL/PMULL. That is a dependency to weigh against about 250 lines, and it is not yet inspected.
5. **Tune at seqair sizes.**
   - Chain count below 65,536 rows.
   - Per-call fixed costs, especially building the Huffman tables.
   - bz2-ng's tunables were set on large blocks.
6. **Swap into seqair.** Replace `cram/block.rs:191` and `cram/arith.rs:305` with the new crate and keep `bzip2` as a dev-dependency for test encoding. Run seqair's nightly fuzz targets.

## Quick win, independent of the port

seqair's `[profile.dev.package]` sets `bzip2.opt-level = 3` but not `libbz2-rs-sys.opt-level = 3`, and package overrides do not reach dependencies. Tests and debug builds therefore decode bzip2 unoptimised.

## Risks and open points

- **Symbol loop:** register pressure and loop shape in Rust. This is the most likely place to lose a few percent. Check the assembly early.
- **fearless_simd inlining:** a helper that fails to inline inside `#[simd]` code silently costs a lot.
- **24-way walk:** Rust has no unroll pragma, so the walk has to be written out by hand or with a macro.
- **Benchmarks:** only one machine (M4 Pro, NEON) has been measured. x86 speed is unverified.
- **Untested reasoning:** nothing has been prototyped yet. All Rust speed claims above are reasoning, not measurement.
