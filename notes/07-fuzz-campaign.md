# 07 - Fuzz campaign

Date: 2026-10-14. Code under test: branch `port` at `ba8f092` (decoder as of "Merge the one-shot decoder").
Machine: Apple Silicon (aarch64), so the SIMD levels exercised are `Level::fallback()` and the best
level (NEON) plus the `decompress` entry point. The x86 levels (SSE2, SSE4.2, AVX2) were **not** run.
Toolchain: `rustc 1.100.0-nightly (2026-09-10)`, cargo-fuzz 0.13.2, AddressSanitizer (cargo-fuzz default,
`-s address`; checked that the binary links the asan runtime), release profile with debug assertions.

## Commands

```sh
cargo +nightly fuzz build            # same as `-s address`
mkdir -p fuzz/corpus/{decompress_differential,decompress_mutated,roundtrip}
# one process per target, all three at once, no -jobs/-workers
cargo +nightly fuzz run decompress_differential fuzz/corpus/decompress_differential fuzz/seeds/decompress_differential -- \
    -max_total_time=2700 -dict=fuzz/bzip2.dict -rss_limit_mb=4096 -timeout=60
cargo +nightly fuzz run decompress_mutated fuzz/corpus/decompress_mutated -- <same flags>
cargo +nightly fuzz run roundtrip          fuzz/corpus/roundtrip          -- <same flags>
```

`-timeout=60` (default is 1200 s) so a decoder hang shows up as a timeout finding; `-rss_limit_mb=4096`
so a header-driven allocation shows up as an OOM finding. Started 12:30, finished 13:15 (2701 s each).

## Results

| Target | Runs | Avg exec/s | Corpus start -> end | `cov:` start -> end | `ft:` end |
| --- | --- | --- | --- | --- | --- |
| `decompress_differential` | 570,277 | 211 | 85 files (seeds) -> 939 files, 6.3 MB | 2280 -> 2782 | 11857 |
| `decompress_mutated` | 1,523,911 | 564 | 0 -> 2145 files, 8.6 MB | 614 (first input) -> 4357 | 15491 |
| `roundtrip` | 206,184 | 76 | 0 -> 949 files, 3.8 MB | 326 (first input) -> 2664 | 11152 |

Notes on the numbers:

- `cov:` counts different binaries' edges, so only compare within a row. The two targets that start from an
  empty corpus have no meaningful "initial" coverage; the value above is the first `NEW` line.
- Throughput fell during the run: early rates were about 360 / 1900 / 930 exec/s (differential / mutated /
  roundtrip) and the averages are 40-80 % lower. Another agent was benchmarking and running its own fuzzers
  on the same machine, so the averages understate the single-core speed. `roundtrip` is slowest because
  each input compresses up to 8 MB with libbz2 and decodes it at three levels.
- Coverage was still creeping up at the end (new `REDUCE`/`NEW` lines every few seconds), so the plateau
  is not reached; `decompress_differential` grows slowest because raw bytes rarely get past the header
  and the Huffman table checks.

## Findings ledger

| # | Target | Kind | Reproducer | Verdict |
| --- | --- | --- | --- | --- |
| - | all three | none | - | No crash, mismatch, timeout or OOM in 45 min per target. |

No entries, so there is no `fuzz/regressions/` directory and nothing was minimised. No harness changes
were needed.

## Gaps

- x86 SIMD levels not covered (aarch64 host); run the same campaign on x86_64 to cover SSE2/SSE4.2/AVX2.
- Not run without ASan (`-s none`) or with `-s memory`/`thread`; the decoder has `unsafe` code that
  ASan covers, MSan would be x86-only anyway.
- 45 minutes at 76 to 564 exec/s is a smoke campaign, not saturation; `decompress_differential` in
  particular would benefit from a seed set of structured blocks (mutated-stream corpus from
  `decompress_mutated` could be fed to it as seeds).
- The oracle is libbz2-rs-sys; no oracle disagreement turned up, so libbz2 1.0.8 / bz2-ng C behaviour
  was not consulted.
