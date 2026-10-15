# bz2ngrs-bench

Decode and encode benchmark. Compares, on identical independent streams:

1. `bz2ngrs::Decoder::decompress` (or `Encoder::compress`) on one `Decoder` (`Encoder`) reused for the whole run (`ours`), as a reader (writer) keeps one
2. the same on a new `Decoder` (`Encoder`) per call (`fresh`), which allocates per call as the other two do
3. the `bzip2` 0.6.1 crate, i.e. libbz2-rs-sys (`bzip2`)
4. bz2-ng C (`C`), the target, linked in-process

Decoding (`--op decode`, the default): each stream is decoded in one call into a buffer of the known size. For C that is
`BZ2_bzDecompressInit`, one `BZ2_bzDecompress`, `BZ2_bzDecompressEnd`, as bz2-ng's `bench.c` does.

Encoding (`--op encode`): each chunk is compressed in one call at `--level`. For C that is
`BZ2_bzBuffToBuffCompress`, for `bzip2` a new `Compress` and one `compress_vec` with `Action::Finish`; both
allocate their block arrays per call, as `fresh` does, where `ours` keeps them in the reused `Encoder`.

## Run

Every run stops within `--time-limit` seconds (default 60): no set starts after three quarters of it, and no
round after nine tenths, so on a slow machine the later sets are skipped (each skip is noted on stderr).

```sh
# the defaults: the first 4 MB of each corpus file, within a minute
cargo run --release -p bz2ngrs-bench
cargo run --release -p bz2ngrs-bench -- --op encode

# everything, on a dedicated machine
cargo run --release -p bz2ngrs-bench -- --max-bytes 0 --time-limit 0
cargo run --release -p bz2ngrs-bench -- --op encode --max-bytes 0 --time-limit 0

# quick (seconds): MC and QUAL, 16 KiB and 64 KiB chunks, 3 rounds
cargo run --release -p bz2ngrs-bench -- --sets MC,QUAL --chunks 16k,64k --rounds 3

# level 6 (what htslib writes), TSV output
cargo run --release -p bz2ngrs-bench -- --level 6 --tsv out.tsv
```

Flags (all take a value):

| flag | default | meaning |
|---|---|---|
| `--op` | `decode` | `decode` or `encode` |
| `--time-limit` | 60 | seconds the whole run may take; 0 for no limit |
| `--sets` | `MC,XA,QUAL,cram` | substring filter on set names (`NA12878/MC`, `cram/<dir>`) |
| `--chunks` | `16k,64k,1m,whole,slices,blocks` | `slices` cuts at the `*.slices-10k.txt` sidecar; `blocks` is the CRAM manifest sets |
| `--samples` | `NA12878` | 1000 Genomes samples to load, e.g. `NA12878,NA19141` |
| `--rounds`, `-r` | 5 | rounds; the decoder order rotates each round |
| `--level`, `-l` | 9 | bzip2 level used to compress the corpus chunks (and the level timed with `--op encode`) |
| `--min-ms` | 30 | a pass repeats until it ran this long (keeps small sets out of timer noise) |
| `--max-bytes` | 4000000 | truncate each corpus file; 0 for all of it (the first line of the output says which) |
| `--data` | `/Users/pascal/code/tfenne/bz2-ng/data/corpora/1kg` | corpus directory |
| `--manifest` | `/Users/pascal/Code/softleif/bz2ngrs/corpus/cram-bzip2/manifest.tsv` | real CRAM bzip2 blocks, one set per source directory; skipped if absent |
| `--tsv` | none | also write a TSV |
| `--decoders` | `ours,fresh,bzip2,C` | decoders (encoders) to time (one alone, for a profile) |
| `--stat` | `median` | `max` reports the best round instead (on a loaded machine the slow rounds are interference) |

Chunks are compressed once, in parallel, before timing, with the `bzip2` crate. CRAM
blocks are used as stored; the expected bytes are the `bzip2` crate's output
(checked against the manifest's declared size if it has an `uncomp`/`declared`/`raw` column).

## Method

- Every decoder's output is compared to the original bytes first. A decoder that
  fails is reported as `FAILED` (`FAILED/stub` for ours, which errors until the real
  decoder exists) and is not timed; a note with the first error goes to stderr.
- Every encoder's stream is decoded by libbz2 first and must give back the original
  bytes, or the encoder is reported as `FAILED`. When any of our streams is not
  byte-identical to another encoder's, a note on stderr counts the identical ones for
  libbz2 (the others differ in the origPtr of exactly periodic blocks), bz2-ng and `fresh`.
- Reported: median (or best, `--stat max`) over rounds of MB/s of uncompressed data (MB = 10^6 bytes) over a
  whole-set pass, plus ratios `ours/C`, `fresh/C` and `ours/bzip2`.
- Per-call cost: ns per call on the 16 KiB sets, and the cost of decoding an empty
  stream (14 bytes) or encoding an empty input, which is init plus teardown with
  nothing to do (best of several runs of 20,000 calls, 1,000 when encoding).
- Release profile only; the root `Cargo.toml` profile (fat LTO, one codegen unit) applies
  to this workspace member.

## How C is linked

`build.rs` compiles the library from `$BZ2NG_C_DIR` (default
`/Users/pascal/code/tfenne/bz2-ng/c`, read-only) with the `cc` crate: the source list of
`c/CMakeLists.txt`, `-O2` (`-O3` for the encoder files), and the per-file ISA flags
(aarch64: `-march=armv8-a+crc`, `+crc+crypto`; x86-64: `-march=x86-64-v2/v3`,
`-msse4.1 -mpclmul`, `-mavx2 -mpclmul -mvpclmulqdq`; the x86 flags are untested).
The decoder memory layout is the CMake default (wide). `BZ2NG_C_DEBUG=1` adds `-g`
(for a line-level profile; it changes a few instructions, so not for timing).

Symbol clash: `libbz2-rs-sys` only exports `BZ2_*` when its `export-symbols` feature is
on, which it is not for `bzip2` 0.6.1's defaults, but any other crate in the graph could
turn it on. So `build.rs` force-includes (`-include`) a generated header that renames
every public bz2-ng symbol `BZ2_x` to `ng_BZ2_x`, as `test/reference_rename.h` does
for the stock libbz2. The Rust side declares the `ng_` names. bz2-ng's other globals are
`bzng_*` and cannot clash.
