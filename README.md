# bz2ngrs

Fast one-shot bzip2 decompression in Rust.

## What it is

`bz2ngrs::Decoder` decodes one bzip2 stream from a byte slice into a caller-sized buffer and returns how many bytes it wrote. Keep one `Decoder` per reader or thread and reuse it: it owns the working memory. It accepts and rejects exactly the streams libbz2 1.0.8 does, and it never panics, whatever the input.

It is a port of the decoder of bz2-ng, a fast libbz2 replacement in C. The scope is deliberately narrow: decoding only, one call per stream, no streaming interface and no encoder. CRAM blocks carry their uncompressed size, so a caller reading them can size `out` exactly and needs nothing more. The crate was written for a CRAM reader (seqair).

## Usage

```rust
use std::io::Write;

// Stand-in for a bzip2 payload read from a file (here made with the `bzip2` crate).
let original = b"ACGTACGTACGTACGT".repeat(1000);
let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
enc.write_all(&original).unwrap();
let compressed = enc.finish().unwrap();

// One decoder per reader or thread, kept for all the blocks it reads.
let mut decoder = bz2ngrs::Decoder::new();

// A block whose uncompressed size is declared, as a CRAM block header says:
// allocate exactly that, decode, cut to what was written.
let block = decoder.decompress_to_vec(&compressed, original.len()).unwrap();
assert_eq!(block, original);

// Or into a buffer you own, for example one you pool and keep at full length:
let mut out = vec![0u8; original.len()];
let n = decoder.decompress(&compressed, &mut out).unwrap();
assert_eq!(&out[..n], &original[..]);
```

For a single stream, `Decoder::new().decompress(src, &mut out)` works and frees its memory afterwards; it pays the allocation on every call.

On success the first `n` bytes of `out` hold the data and the rest is untouched. Bytes after the stream's end-of-stream marker are ignored, as `bzip2::read::BzDecoder` and libbz2's `BZ2_bzDecompress` ignore them. On error the contents of `out` are unspecified. `decompress_to_vec(src, size)` accepts a stream shorter than `size` (the `Vec` is cut to the length written) and returns `OutputTooSmall { capacity: size }` for a longer one. `size` comes from the file, so a caller that distrusts it bounds it first.

A result never depends on earlier calls: a `Decoder` that has decoded other streams, or failed on them, answers as a new one does. `Decoder` is `Send`, and `Decoder::new()` and `Default` allocate nothing.

## Errors

`Error` is `Copy`, allocation-free and `#[non_exhaustive]`. libbz2 reports four codes; this type adds what the decoder knew where it failed.

| Variant | Meaning |
|---|---|
| `NotBzip2` | The input does not start with `BZh1` to `BZh9`. |
| `Corrupt { kind, at }` | A format rule is broken. `kind` is a `Corruption` (bad block magic, `origPtr` out of range, bad code length, invalid Huffman code, run too long, and so on); `at` is a `Position` with the 0-based block index and the bits of input consumed. |
| `BlockCrc { block, stored, computed }` | A block's CRC disagrees with the bytes it decoded to. |
| `StreamCrc { stored, computed }` | The combined CRC after the end-of-stream marker disagrees with the blocks. |
| `UnexpectedEof { at }` | The input ends before the end-of-stream marker and CRC. |
| `OutputTooSmall { capacity }` | The stream decodes to more than `out.len()` bytes. |

`OutputTooSmall` is the only variant about the caller's buffer. A caller that knows the uncompressed size can pass it to `decompress_to_vec` and treat `OutputTooSmall` as a bad block (the data is longer than its header claims), and a returned length below the size as the same. For a stream libbz2 accepts, `OutputTooSmall` is returned as soon as the output would pass `out.len()`; a corrupt stream may report its corruption first.

## Performance

Decode speed in MB/s of uncompressed data (MB = 10^6 bytes), median of 7 rounds, single thread, each chunk an independent stream written at level 9, on the 1000 Genomes NA12878 MC, XA and QUAL tag data. "bzip2" is the `bzip2` 0.6.1 crate (libbz2-rs-sys); "C" is bz2-ng's C library, linked into the same process.

| set | chunk | bz2ngrs | bzip2 crate | bz2-ng C | ours / C |
|---|---|---:|---:|---:|---:|
| MC | 16 KiB | 452 | 156 | 355 | 1.27 |
| MC | 64 KiB | 476 | 154 | 411 | 1.16 |
| MC | 1 MiB | 502 | 101 | 435 | 1.15 |
| XA | 16 KiB | 263 | 74 | 225 | 1.17 |
| XA | 64 KiB | 281 | 72 | 251 | 1.12 |
| XA | 1 MiB | 292 | 62 | 269 | 1.08 |
| QUAL | 16 KiB | 933 | 206 | 798 | 1.17 |
| QUAL | 64 KiB | 1134 | 262 | 994 | 1.14 |
| QUAL | 1 MiB | 1365 | 283 | 1186 | 1.15 |

On 11 sets of real bzip2 blocks extracted from CRAM files (htslib levels 1, 6 and 9 and CRAM 3.1 profiles; 73 B to 1.9 MB per block), bz2ngrs runs at 1.07 to 1.19 times bz2-ng C and 2.1 to 4.4 times the `bzip2` crate.

- **Machine.** Apple M4 Pro (aarch64, NEON), with desktop applications running, so expect a few percent of noise between runs. On an AMD Ryzen 9 3950X (Zen 2, AVX2), bz2ngrs runs at 0.99 to 1.22 times bz2-ng C at 16 and 64 KiB, but 0.88 to 1.06 times at 1 MiB, and 0.91 to 1.07 times on the CRAM sets (`notes/12-x86.md`).
- **Per-call cost.** The "bz2ngrs" column is one `Decoder` reused for every stream, as a reader would use it. Decoding an empty 14-byte stream with a reused `Decoder` takes about 16 ns here, against about 150 ns for the C library and about 15 µs for the `bzip2` crate, because the working memory is kept between calls (see Memory).
- **Reproduce.** The `bench` crate links bz2-ng's C sources, which are not in this repository. Point `BZ2NG_C_DIR` at the `c/` directory of a bz2-ng checkout and the `--data` flag at its `data/corpora/1kg` corpora, then run `cargo run --release -p bz2ngrs-bench`. See `bench/README.md` for flags, and `notes/05-baseline-bench.md` and `notes/06-perf-log.md` for method and history.

## How it works

The decoder follows bz2-ng's design; `notes/02-decoder.md` describes it, and `notes/06-perf-log.md` records each optimisation with its measurements.

- A 64-bit bit reader that refills with one 8-byte load.
- Table-driven Huffman decoding over groups of 50 symbols.
- The move-to-front list's head lives in a SIMD register, written once against `fearless_simd` and dispatched at run time (capped at AVX2 on x86; a scalar `Fallback` level exists for every kernel).
- The inverse Burrows-Wheeler transform builds its successor table in interleaved quarters, cuts the text into segments at splitter rows, and walks several segments in lock step to overlap the dependent loads.
- The first run-length stage is undone straight into the caller's buffer, with wide stores for runs.
- The block CRC uses hardware carry-less multiply or CRC instructions where present (PMULL or the ARMv8 CRC instructions on aarch64, PCLMULQDQ on x86-64), picked once at run time, with a portable slice-by-16 fallback.

## Memory

The `Decoder` owns the working memory: about 6 MiB of address space, allocated zeroed on the first decode (not in `Decoder::new()`) and freed when the `Decoder` is dropped. It is never shrunk in between, and nothing is kept outside the `Decoder`: no thread-local, no global. Only the pages a block touches become resident: about 6 bytes per byte of the largest block (after bzip2's first run-length stage) it has decoded, plus about 40 KiB. That is at most about 5.5 MB for `BZh9` streams and 3.7 MB for `BZh6`. A failure to allocate it aborts, as Rust allocation failures do. A reader that never meets bzip2 pays nothing.

## Safety and verification

- **`unsafe`.** The crate denies `unsafe_code`. The only exception is three blocks in `src/crc.rs` that call the `#[target_feature]` CRC kernels, each reached only after the CPU features were detected. Clippy lints require a `// SAFETY:` comment on each.
- **Property tests.** Hegel property tests (`tests/`) compare every kernel at every SIMD level, the scalar `Fallback` included, with the `bzip2` crate (libbz2-rs-sys) as oracle. They cover valid, mutated, truncated and hostile streams, and reuse: one `Decoder` decodes a sequence of streams and each result must equal a new decoder's and the oracle's, and forged streams: a small independent bzip2 block writer produces arbitrary BWT columns and `origPtr` values with valid CRCs, and the result is checked against a naive reference decoder that shares no code with the crate.
- **Real blocks.** `tests/corpus.rs` decodes a local corpus of real CRAM bzip2 blocks. The corpus is not in the repository (`corpus/cram-bzip2`, git-ignored), and the test passes with a note when it is absent.
- **Fuzzing.** Three cargo-fuzz targets (`fuzz/`): arbitrary bytes and structured mutations against the oracle, and a round trip, each through one long-lived `Decoder` per process. The first campaign, 45 minutes per target on aarch64 under AddressSanitizer, found no crash, mismatch, timeout or out-of-memory (`notes/07-fuzz-campaign.md`). CI runs each target for 60 seconds, and a nightly workflow for 10 minutes, on x86_64 and aarch64.
- **Review.** An adversarial review of the decoder against libbz2 1.0.8 found no panic and no accept/reject difference (`notes/08-review.md`).
- **CI.** Tests run on x86_64 (with and without AVX2 enabled at compile time) and aarch64, in debug and release, with Clippy, rustdoc and `cargo deny`.

The 45-minute campaign and the review ran on aarch64 (the review also on x86_64 under Rosetta, which has no AVX2); the x86 levels are covered by the CI tests and the nightly fuzz runs.

## Development

```sh
cargo test                                   # unit, property and doc tests
HEGEL_DEFAULT_PROFILE=thorough cargo test --release   # more cases; profiles are in hegel.toml
BZ2NGRS_CORPUS=/path/to/corpus/cram-bzip2 cargo test --test corpus
cargo run --release -p bz2ngrs-bench         # needs bz2-ng's C sources, see Performance
cargo +nightly fuzz run decompress_mutated   # see fuzz/README.md
./fuzz/run_all.sh 60                         # every fuzz target, 60 s each
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
```

- Tests also reach the SIMD level through `bz2ngrs::Decoder::__with_level`, a `#[doc(hidden)]` hook that is not part of the API.
- The corpus is made with `tools/cram-extract`, which pulls the bzip2 payloads out of CRAM files; its README gives the steps.
- `AGENTS.md` has the rules for changes: correctness contract, test and SIMD conventions, benchmark requirements and comment style.

## Licence and provenance

MIT, see `LICENSE`.

- The decoder is ported from bz2-ng (MIT, Copyright (c) 2026 Tim Fennell; `LICENSE-bz2-ng`). This includes the CRC kernels.
- The 512-entry block randomisation table in `src/unrle.rs` (libbz2's `BZ2_rNums`; also in `tests/common/forge.rs`) is taken from libbz2, which is under a BSD-style licence (Copyright (C) 1996-2019 Julian R Seward; `LICENSE-bzip2`). The format's behaviour follows libbz2 1.0.8, which was read for reference through libbz2-rs-sys. The CRC lookup table is computed at compile time, not copied.
