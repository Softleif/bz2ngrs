# Agent guide

bz2ngrs is a general-purpose bzip2 decoder and encoder, ported from the C library of bz2-ng: one-shot calls on buffers (`Decoder`, `Encoder`) and streaming adapters over `io::Read` and `io::Write` (`DecompressReader`, `CompressWriter`). seqair's CRAM code is one user; the public API is for anyone, so it should be hard to misuse and fast by default.

## Rules

- **Correctness.**
  - No decoding API panics, whatever its input: `Decoder`'s methods, `DecompressReader`.
  - A `Decoder`'s (or reader's) results never depend on earlier calls.
  - It accepts and rejects exactly the streams libbz2 1.0.8 does. Where several concatenated streams are read (`decompress_all`, `DecompressReader`), it accepts and rejects exactly what the `bzip2` crate's `MultiBzDecoder` does.
  - On success it never touches `out` past the returned length.
  - No encoding API panics, whatever its input, and an `Encoder`'s (or writer's) results never depend on earlier calls.
  - Its output is byte-identical to libbz2 1.0.8's for one `BZ_FINISH` over the whole input (`BZ2_bzBuffToBuffCompress`), except the origPtr of an exactly periodic block, which is the last tied row (as in bz2-ng). `CompressWriter` writes the same bytes however the input is split.
- **API.**
  - Typed results and errors (`Decoded`, `Error`), no sentinel values. Bounds that protect the caller (a decode's output `limit`) are explicit parameters, not defaults.
  - The fast path is the obvious one: reuse of working memory is built into the reader and writer (`reset`), and no wrapper (`BufReader`) is needed for speed.
- **Tests.**
  - Write property tests with hegel (`hegeltest`). Follow the hegel skill; never weaken a failing property.
  - Every kernel runs at every SIMD level, `Level::fallback()` included (`Decoder::__with_level`, `Encoder::__with_level`, `DecompressReader::__with_level`, `CompressWriter::__with_level`).
  - The oracle is the `bzip2` crate, a dev-dependency.
  - For every property, ask: is it tautological? An oracle that shares the implementation's code or assumptions proves nothing. Neither does an expected value derived the way the code derives it.
- **SIMD.**
  - Write kernels once, generic over `fearless_simd::Simd`, reached via `dispatch!`. Helpers are `#[inline(always)]`, with no vector ops in closures.
  - `core::arch` is allowed only where fearless_simd lacks the operation: the CRC's carry-less multiply, which must be feature-detected and tested against the portable path, and the sorter's x86 prefetch hint, which is baseline x86-64 and changes no result.
- **`unsafe`.** Only with a measured win, and every block needs a `// SAFETY:` comment.
- **Speed.**
  - Every optimisation carries before and after numbers from `bench/`, compared against bz2-ng C in the same run, in its commit message.
  - Revert what doesn't help.
- **Sources.**
  - The crate is Apache-2.0 WITH LLVM-exception (`LICENSE`). Ported code keeps its origin's notice, and an altered bzip2 file says so.
  - bz2-ng (MIT, its history included), sais-lite (MIT) and libbz2 or libbz2-rs-sys (BSD-style) may be read and ported.
  - libsais may be read and ported only from the copy bz2-ng vendors (commit 1a696e6, Apache-2.0 WITH LLVM-exception) (`src/enc/sais_top.rs`, `LICENSE-libsais`). Its earlier releases are plain Apache-2.0: take nothing from them, nor from translations of them such as libsais-rs.
  - Never read GPL code (lbzip2, 7-Zip).

## Comments

- **A comment earns its place only by explaining a why the code cannot show.**
  - Never restate what the code says.
  - Keep a comment only when something invisible forces a non-obvious choice: a library or protocol constraint, a domain rule, a deliberate trade-off, or an invariant the types can't carry.
  - When you *do* need to say "this can't happen", say it and say why. A bare `throw` with no explanation is worse than none.
- **Never narrate the change in the source.** Explanations of why the code changed belong in the commit message or PR. A reader six months out has no "before", so the contrast resolves to nothing.
