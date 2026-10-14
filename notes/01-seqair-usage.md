# What seqair needs from bzip2

Survey of `/Users/pascal/Code/bsblabludwig/seqair` (read-only), done 2026-10-14. Paths below are relative to that root unless absolute.

## TL;DR

- seqair is a **reader** for CRAM. In production code it only ever **decompresses** bzip2. It never compresses bzip2 outside `#[cfg(test)]` code. There is no CRAM writer in seqair.
- The whole production surface is "decompress one single-stream bzip2 buffer that is already in memory, into an output whose maximum size is known in advance, and say whether the output would have been larger". That is two call sites, both of the form `BzDecoder::new(&[u8]).take(limit).read_to_end(&mut vec_with_capacity)`.
- Compression appears only in unit tests (to make inputs), at `Compression::fast()` (level 1) and `Compression::default()` (level 6). Nothing checks that the compressed bytes are identical to those from libbz2/htslib. **Byte-identical output is not required by seqair.** It needs only a valid stream that round-trips.
- Current backend: `bzip2` 0.6.1 with its default features, i.e. **`libbz2-rs-sys` 0.2.5 (pure Rust)**. The C `bzip2-sys` is in the lockfile only because the dev-dependency `rust-htslib` pulls it in through `hts-sys`.
- Licences: `deny.toml` already allows `bzip2-1.0.6`, `Apache-2.0`, `BSD-3-Clause`, `MIT`, `Zlib`. It does **not** list `Apache-2.0 WITH LLVM-exception`, which is libsais's licence as bz2-ng uses it. That would need one added line. seqair already depends on C code through `cc` in its normal (non-dev) dependencies: `libdeflater` builds `libdeflate-sys`, and `xz2` builds `lzma-sys`. So a C dependency would not be new. The project does lean toward pure Rust, though: the README says "Pure-Rust BAM/SAM/CRAM/FASTA reader".
- fearless_simd: the lockfile resolves `fearless_simd` to **1.1.0** and `fearless_simd_macros` to 0.1.0. The idiom is a `#[simd]` entry function, generic over `S: Simd`, reached through `dispatch!(Level::new(), simd => f(simd, ..))`. Helpers are `#[inline(always)]`. No vector ops go in closures. Tests run every kernel at every level, including `Fallback`, which needs the dev feature `force_support_fallback`.

## 1. Where and how the `bzip2` crate is used

Dependency declarations:

- `crates/seqair/Cargo.toml:38` has `bzip2 = "0.6.1"`, a normal dependency with default features.
- `crates/seqair/Cargo.toml:54-59` is the "bzip2" in a list that you asked about. It is the **feature list of the `rust-htslib` dev-dependency** (`["libdeflate", "bzip2", "lzma", "static"]`), used as the test oracle. That feature makes htslib link a static C libbz2 via `hts-sys` → `bzip2-sys` 0.1.13+1.0.8 (`Cargo.lock:382`, `Cargo.lock:1123`). It plays no part in seqair's own decoding.
- `Cargo.toml:82` has `bzip2.opt-level = 3` under `[profile.dev.package]`, next to the same override for `xz2`, `flate2`, `libdeflater` and others. It exists only so dev and test builds are not painfully slow. Note that it applies to the `bzip2` wrapper crate only, not to `libbz2-rs-sys`, where the actual work happens.
- `Cargo.lock:373-379`: `bzip2 0.6.1` depends only on `libbz2-rs-sys` (`Cargo.lock:1444`, v0.2.5, licence `bzip2-1.0.6`). The crate's features are `default = ["dep:libbz2-rs-sys"]` with `bzip2-sys` optional, so seqair runs on the **pure-Rust backend**. `fuzz/Cargo.lock:69,226` shows the same setup.

Production call sites (all decompression):

| Site | What | API |
|---|---|---|
| `crates/seqair/src/cram/block.rs:191-193` | CRAM block method 2 (BZIP2) | `read_capped(bzip2::read::BzDecoder::new(compressed), uncompressed_size)` |
| `crates/seqair/src/cram/block.rs:229-235` | `read_capped` | `Vec::with_capacity(size)`, then `decoder.take(size + 1).read_to_end(&mut output)` |
| `crates/seqair/src/cram/arith.rs:305-320` (`decode_ext`) | The arithmetic coder's (method 6) EXT transform, whose body is a bzip2 stream starting with `BZh` | Same pattern inlined: `Vec::with_capacity(len)`, `BzDecoder::new(body).take(len+1).read_to_end` |

Test-only and fuzz-only sites:

- `crates/seqair/src/cram/arith/reference.rs:290-299`: the reference arith decoder (`#[cfg(any(test, feature = "fuzz"))]`, `arith.rs:17-20`) does the same `BzDecoder::new(data).take(len+1).read_to_end`.
- Compression, for test inputs only: `crates/seqair/src/cram/arith.rs:434-440` uses `bzip2::read::BzEncoder::new(data, Compression::fast())` then `read_to_end`. `crates/seqair/src/cram/block.rs:353-359` uses `bzip2::write::BzEncoder` with `Compression::default()`. `block.rs:474-483` uses `write::BzEncoder` with `Compression::fast()` on 1 MiB of zeros, and on `b"short"`.

API shape:

- **One-shot over an in-memory buffer.** The input is a whole `&[u8]` and the output is a whole `Vec<u8>`. Nothing streams across reads, and the streaming `Read` adaptor is used only to get the "stop after N+1 bytes" cap.
- **Single stream.** `bzip2::read::BzDecoder` (not `MultiBzDecoder`) stops at the first end-of-stream and ignores whatever follows (`~/.cargo/registry/src/*/bzip2-0.6.1/src/bufread.rs:177-221`, `multi: false`). A single stream can still hold several 100–900 kB bzip2 blocks.
- **Truncated input** gives an `io::ErrorKind::UnexpectedEof` ("decompression not finished but EOF reached"). Corrupt data gives `InvalidInput`.
- Overhead today: `read::BzDecoder` wraps a `BufReader` (8 KiB) around the slice. `read_to_end` then goes through the `Take` adaptor in chunks. A native `decompress_into(&[u8], &mut [u8]) -> Result<usize, _>` would avoid both.

**Minimal surface for a port:** a function that decompresses one bzip2 stream from `&[u8]` into a caller buffer of known capacity. It must report (a) the number of bytes written, (b) "output buffer full and more data remains", which is htslib's `BZ_OUTBUFF_FULL` and seqair's `BlockDecodesPastSize`, and (c) a typed error for corrupt or truncated input. Concatenated streams, streaming input, `Read`/`Write` adaptors and the compressor are all unnecessary for production. Tests would still need *some* encoder at levels 1 and 6. The existing `bzip2` crate could stay as a dev-dependency to supply it, which would also act as an independent oracle.

## 2. Data context

- The codec is CRAM external block compression method 2 (`docs/spec/2-cram-1-reader.md:294-295`, `r[cram.codec.bzip2]`: "MUST be supported for v3.0 compatibility"). It is also used inside the arithmetic coder's EXT transform (`docs/spec/2-cram-1-reader.md:347`: "bzip2 (`BZh`) is the only one defined").
- **Uncompressed size is known in advance, and seqair uses it.** The CRAM block header stores it. `block.rs:144-145` checks it against `MAX_CODEC_OUTPUT`, which is 256 MiB normally and 4 MiB under `cfg(fuzzing)` (`reader.rs:306,321-334`). A size of 0 skips decoding (`block.rs:149-150`). `read_capped` preallocates `Vec::with_capacity(size)`. The arith EXT path also preallocates `len`.
- Size semantics, matching htslib (`docs/spec/2-cram-1-reader.md:115`, `block.rs:233-246` `check_decoded_size`): bzip2 output **may be shorter** than the declared size, because htslib decodes into a buffer of that size and keeps what it gets. It **must not be longer**. The decoder must stop one byte past the declared size, so that a small bomb cannot inflate before the check runs. For arith EXT the output must also fit in `len` (`arith.rs:317-319`). The reference decoder demands exactly `len`.
- Typical sizes: in the hts-specs conformance file `tests/data/hts-specs/cram/3.1/passed/level-3.cram` (20 000 reads, one container), `samtools cram-size -v` shows 5 bzip2 blocks:

  | Series | Uncompressed | Compressed | Header |
  |---|---|---|---|
  | BF | 30 882 | 5 845 | `bzip2-6` |
  | MF | 19 708 | 1 203 | `bzip2-6` |
  | TS | 37 062 | 20 162 | `bzip2-6` |
  | NP | 40 656 | 20 514 | `bzip2-6` |
  | BA | 118 988 | 6 373 | `bzip2-6` |

  `level-4.cram` has 3 bzip2 blocks. None of the other test CRAMs (`tests/data/test*.cram`, `unplaced_multiref.cram`) contain any. Expect blocks of **tens of kB up to roughly 1 MB**, nearly always a single bzip2 block (`BZh6` is htslib's choice at these profiles, giving 600 kB blocks). That makes **decoder latency on small inputs** and per-call setup cost, such as table allocation, matter more than peak throughput on huge files. htslib writes bzip2 only under the `small` and `archive` CRAM 3.1 profiles, and v3.0 files can contain it too. The default htslib profile does not use bzip2, so in real-world seqair workloads bzip2 is a minority codec.

## 3. Byte-identical output requirement

**None.** I searched tests, docs, fuzz and tools:

- Every test that involves bzip2 is a decode test. `crates/seqair/tests/cram_hts_specs_levels.rs:212-218` (`level_3_matches_htslib`) checks that seqair decodes the same *records* as htslib. `crates/seqair/tests/cram_bam_equivalence_properties.rs:327-361` has htslib/samtools write CRAM 3.1 `small`/`archive` profiles and compares the decoded records with the BAM. `crates/seqair/src/cram/arith/htscodecs_oracle.rs` uses htscodecs' C encoder to produce streams, which can include bzip2 EXT, and checks that seqair's decoders recover the input.
- Unit tests compress with the `bzip2` crate, then decode and compare against the original *plaintext* (`block.rs:350-372`, `block.rs:470-487`, `arith.rs:716-730`).
- There is no CRAM writer in `crates/seqair/src` and no golden compressed files.
- A port that produced different (valid) compressed bytes would break nothing in seqair. bz2-ng's byte-identity is a nice property but not a requirement here.

## 4. Error handling and hostile input

- The error variant is `CramError::Bzip2DecompressionFailed { source: std::io::Error }` (`crates/seqair/src/cram/reader.rs:102-103`). It wraps the `io::Error` from `BzDecoder`. A port would want a typed error enum: CLAUDE.md says "never use `io::Error::other("message")` — add a typed variant", and error fields are typed, never `String`. Swapping the `source` type to the port's own error type would be a small, welcome change.
- Related variants: `BlockDecodesPastSize { method, expected }` (output too long), `ArithExtUnknownCodec` (`reader.rs:202`, not `BZh`) and `ArithLengthMismatch`.
- `docs/spec/0-0-general.md` `r[io.fuzz.no_panic]` says parsers MUST NOT panic on any input. `r[io.fuzz.alloc_limits]` and `r[io.fuzz.codec_output_cap]` bound allocations. The decoder must therefore never panic or over-allocate on garbage. The test `block.rs:610-625` (`bzip2_decompression_failed_on_garbage`) and the bomb test `block.rs:466-479` (1 MiB of zeros declared as 100 bytes) pin this.
- Fuzz targets that reach the bzip2 decoder:
  - `fuzz/fuzz_targets/fuzz_cram_block.rs`, which calls `parse_block` directly.
  - `fuzz_cram_container.rs`, `fuzz_cram_decode_full.rs` and `fuzz_reader_indexed.rs`, through block parsing.
  - `fuzz_cram_codecs.rs` and `fuzz_cram_arith.rs`, through arith EXT. These also cross-check against the reference decoder, which uses `bzip2` too.

  They run nightly under ASAN via `fuzz/run_all.sh` (CLAUDE.md "Fuzzing"). A Rust port would sit directly behind these targets, so it should be fuzzed on its own as well.
- Linking note: test builds link both the pure-Rust `libbz2-rs-sys` and the static C libbz2 from `hts-sys`. If a port exported C symbols named `BZ2_*` (as a libbz2 drop-in would), it could clash with that static library in seqair's test binaries. A plain Rust API avoids the problem.

## 5. fearless_simd conventions

Versions:

- Workspace `Cargo.toml:14-15` has `fearless_simd = "1.0.0"` and `fearless_simd_macros = "0.1.0"`. `crates/compair/Cargo.toml:16-17` has `"1.1"`. `Cargo.lock:842-851` resolves both to **fearless_simd 1.1.0** and **fearless_simd_macros 0.1.0**. A port should target `fearless_simd = "1.1"`.
- `crates/seqair/Cargo.toml:47` has the dev-dependency `fearless_simd = { workspace = true, features = ["force_support_fallback"] }`. The comment at `:45-46` explains that it "Compiles fearless_simd's scalar `Fallback` level on SIMD targets too, so the kernel tests can run it next to every vector level the CPU has". Without it, `Level::fallback()` is not available on x86-64/aarch64. The same pattern appears in `crates/seqair-types/Cargo.toml:36-38` and `crates/compair/Cargo.toml:45-47`.

The rule is `docs/spec/0-0-general.md:72-73` (`r[io.simd_portable]`), summarised:

- Write each kernel once, generic over `fearless_simd::Simd`, and enter it via `dispatch!(Level::new(), ..)` from a `#[fearless_simd_macros::simd]` function. No per-ISA `core::arch` copies, no `unsafe`, no `is_x86_feature_detected!`.
- Byte kernels use the native width (`S::u8s`) so AVX2 and AVX-512 get wide registers. Prefer `v ^ mask.select(flip, zero)` over a blend.
- **Only the entry point is `#[simd]`.** Every helper is an `#[inline(always)]` fn generic over `S`. **No vector ops in closures**, because on x86 each op would become an out-of-line call: 10–20× slower, and invisible on aarch64.
- Tests MUST run each kernel at every level the CPU supports, `Fallback` included.

`docs/spec/0-0-general.md` also has `r[io.platform_optimizations]`: the scalar fallback must give identical results, checked by property tests. `r[io.fuzz.simd+2]` says to fuzz SIMD on both aarch64 and x86-64.

Representative example, `crates/seqair/src/io/text_scan.rs:10-41`:

```rust
use fearless_simd::{Level, dispatch, prelude::*};
use fearless_simd_macros::simd;

pub(crate) fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    find_byte_at(Level::new(), hay, needle)
}

// r[impl io.simd_portable]
fn find_byte_at(level: Level, hay: &[u8], needle: u8) -> Option<usize> {
    dispatch!(level, simd => find_byte_simd(simd, hay, needle))
}

#[simd]
fn find_byte_simd<S: Simd>(simd: S, hay: &[u8], needle: u8) -> Option<usize> {
    let mut chunks = hay.chunks_exact(S::u8s::LEN);
    // ... S::u8s::from_slice(simd, chunk).simd_eq(needle), any_true(), to_bitmask() ...
}
```

The `*_at(level, ..)` split exists so tests can pass every level. `crates/seqair/src/simd_levels.rs:1-20` builds the list: `Level::fallback()` and `Level::new()`, plus on x86-64 `best.as_sse2()`, `as_sse4_2()` and `as_avx2()`. Tests loop over it, for example `text_scan.rs:144,156`. Other kernels follow the same shape:

- `crates/seqair/src/bam/seq.rs:192,201` (nibble decode via `swizzle_dyn_within_blocks`)
- `crates/seqair/src/fasta/strip.rs:20-33`
- `crates/seqair/src/bam/base_mod.rs:522-529`
- `crates/seqair-types/src/base.rs:154-171`
- `crates/seqair/src/cram/arith.rs:156-159`, which shows const-generic dispatch: `dispatch!(level, simd => decode_order::<_, false>(simd, body, len))`

The comments at `strip.rs:30-33`, `seq.rs:255` and `base.rs:201` repeat the "helpers are `#[inline(always)]`, not `#[simd]`" rule.

Other conventions relevant to a port:

- **Toolchain/MSRV:** `rust-toolchain.toml` pins `1.98.1`, and every crate has `rust-version = "1.98.1"`.
- **Lints** (`Cargo.toml:24-77`, CI runs with `-D warnings`): `panic = "deny"` and `undocumented_unsafe_blocks = "deny"`. These are warnings: `indexing_slicing`, `unwrap_used`, `arithmetic_side_effects`, the `cast_possible_truncation` family, and `allow_attributes_without_reason`. CLAUDE.md also says no indexing (use `.get()`, or `#[allow]` plus `debug_assert!`), no `unwrap`, and one typed error enum per module. These lints apply only to seqair's own crates. A separate dependency crate is not bound by them, but code vendored into the workspace would be.
- **unsafe:** used sparingly (a handful of blocks in `bam/bgzf.rs`, `bam/region_buf.rs`, `bam/seq.rs` and others, each with a `SAFETY` comment, which lint enforces). SIMD is expected to need none.
- **Testing:** property tests use `hegeltest` (`#[hegel::test]`). The preferred approach is an independent oracle, with htslib/htscodecs via `rust-htslib` as dev-dependencies, and they avoid "tautological" tests. Specs use Tracey `r[...]` rule IDs, with `// r[impl ..]` in code and `// r[verify ..]` in tests.

Licence and C-dependency policy (`deny.toml`):

- `[licenses] allow = ["0BSD", "Apache-2.0", "BSD-3-Clause", "BSL-1.0", "ISC", "MIT", "NCSA", "Unicode-3.0", "Zlib", "bzip2-1.0.6"]`, with `confidence-threshold = 0.9`. So MIT, Apache-2.0, BSD-3-Clause and bzip2's own licence are all fine.
- **libsais, as bz2-ng uses it, is `Apache-2.0 WITH LLVM-exception`** (bz2-ng README "Licensing"). cargo-deny matches licence exceptions explicitly. To my knowledge, a plain `"Apache-2.0"` entry does not satisfy an `Apache-2.0 WITH LLVM-exception` expression, so `deny.toml` would need that entry added. This is a trivial change, but someone has to make it. (Not verified by running `cargo deny`.)
- `[sources]` allows only crates.io, plus one pinned git repo (`gkl-rs`, compair's bench). A port published on crates.io is fine. A git dependency would need adding to `allow-git`.
- **Existing C dependencies (non-dev):** `libdeflater` → `libdeflate-sys` (cc, `Cargo.lock:1456-1462`), and `xz2` → `lzma-sys` (cc/pkg-config, `Cargo.lock:1551-1559`). Dev only: `rust-htslib` → `hts-sys` (static htslib + `bzip2-sys` + `libz-sys`). `flate2` uses `miniz_oxide` (pure Rust). So C through `cc` is already accepted in production. Still, the pure-Rust bzip2 backend is the status quo, and a pure-Rust port with fearless_simd would match the project's direction (`r[io.simd_portable]`, "Pure-Rust" in the README) better than shipping bz2-ng's C.

## 6. Benchmarks and profiling evidence

- **No benchmark exercises bzip2.** `crates/seqair/benches/cram.rs` benchmarks `tests/data/test.cram` (`:14`), which has no bzip2 blocks. It also benchmarks the arith/tok3 codecs on hts-specs vectors and the level-2..4 CRAMs (`:302`, `:342-360`). The tok3 bench filters raw blocks by method using the helper `crates/seqair/tests/support/cram_raw_blocks.rs` (`raw_blocks(&[u8]) -> Vec<RawBlock>`, which has method constants for 5–8, no bzip2 constant).
  - **Easiest real-world bzip2 benchmark:** reuse that helper to pull the method-2 blocks out of `tests/data/hts-specs/cram/3.1/passed/level-3.cram` and `level-4.cram`, and time them against `bzip2` (libbz2-rs-sys) and htslib's C libbz2. The blocks are the 20–120 kB ones listed above. The fuller version: have `samtools view -O cram,version=3.1,small` (or `archive`) re-encode a larger BAM such as `tests/data/test.bam` and decode it end to end. That is how `cram_bam_equivalence_properties.rs` makes its inputs.
- **Profiling:** `profile.json.gz` at the repo root is 788 bytes. It is an empty, unsymbolicated samply profile ("product":"mpileup", `"libs":[]`, `"symbolicated":false`) with no bzip2 or libbz2 symbols. There is **no evidence** about how much time bzip2 takes in seqair. Given that the default htslib profile does not emit bzip2, its share is probably small except on `small`/`archive` or v3.0-era files.

## Implications for the port

1. The decoder is what matters for seqair. Encoder work, including the libsais BWT, is irrelevant to seqair in production, and would be needed only to drop the `bzip2` dev-dependency.
2. Recommended API: `pub fn decompress_into(src: &[u8], dst: &mut [u8]) -> Result<usize, DecodeError>`, with a distinct `OutputFull` (or "more data") outcome. Optionally add a `Vec`-returning helper that reads up to a cap. Use one stream, ignore trailing bytes, and never panic.
3. Optimise for blocks of 20 kB–1 MB with a single bzip2 block each. Allocate per call cheaply, or let the caller reuse a decoder state, since seqair already threads reusable buffers (`rans_4x8_buf`, `nx16_order1_buf`) through `decompress_block`.
4. Byte-identity with libbz2 is not needed by seqair. If you keep it for the encoder anyway, that is for other users.
5. Use fearless_simd 1.1 with the `#[simd]` + `dispatch!` + `#[inline(always)]` helper pattern, a `force_support_fallback` dev feature, and tests across all levels. `deny.toml` needs `Apache-2.0 WITH LLVM-exception` if libsais-derived code comes along.
