# Agent guide

bz2ngrs is a one-shot bzip2 decoder, ported from the C decoder of bz2-ng. Its only consumer is seqair's CRAM reader.

## Rules

- **Correctness.**
  - `Decoder::decompress` must never panic, whatever its input.
  - A `Decoder`'s results never depend on earlier calls.
  - It accepts and rejects exactly the streams libbz2 1.0.8 does.
  - On success it never touches `out` past the returned length.
- **Tests.**
  - Write property tests with hegel (`hegeltest`). Follow the hegel skill; never weaken a failing property.
  - Every kernel runs at every SIMD level, `Level::fallback()` included (`Decoder::__with_level`).
  - The oracle is the `bzip2` crate, a dev-dependency.
  - For every property, ask: is it tautological? An oracle that shares the implementation's code or assumptions proves nothing. Neither does an expected value derived the way the code derives it.
- **SIMD.**
  - Write kernels once, generic over `fearless_simd::Simd`, reached via `dispatch!`. Helpers are `#[inline(always)]`, with no vector ops in closures.
  - `core::arch` is allowed only where fearless_simd lacks the operation (the CRC's carry-less multiply). It must be feature-detected and tested against the portable path.
- **`unsafe`.** Only with a measured win, and every block needs a `// SAFETY:` comment.
- **Speed.**
  - Every optimisation carries before and after numbers from `bench/`, compared against bz2-ng C in the same run, in its commit message.
  - Revert what doesn't help.
- **Sources.**
  - bz2-ng (MIT) and libbz2 or libbz2-rs-sys (BSD-style) may be read and ported.
  - Never read GPL code (lbzip2, 7-Zip).

## Comments

- **A comment earns its place only by explaining a why the code cannot show.**
  - Never restate what the code says.
  - Keep a comment only when something invisible forces a non-obvious choice: a library or protocol constraint, a domain rule, a deliberate trade-off, or an invariant the types can't carry.
  - When you *do* need to say "this can't happen", say it and say why. A bare `throw` with no explanation is worse than none.
- **Never narrate the change in the source.** Explanations of why the code changed belong in the commit message or PR. A reader six months out has no "before", so the contrast resolves to nothing.
