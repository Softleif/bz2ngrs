# Baseline speed: bz2-ng vs libbz2 1.0.8 vs libbz2-rs-sys

Measured on an Apple M4 Pro (`sysctl -n machdep.cpu.brand_string`), macOS (Darwin 27.0.0), Apple clang 21.0.0 (clang-2100.3.34.2), cmake 4.4.4, rustc 1.96.0 (2026-05-25). bz2-ng's bench selects the `neon` kernel. Nothing else was pinned or quiesced (macOS has no `taskset`), so expect a few percent of noise; the spread column of the raw runs was 1-8% except one outlier noted below.

## Setup

- bz2-ng at `/Users/pascal/code/tfenne/bz2-ng` was not modified; everything was built out of tree under the session scratchpad (`.../scratchpad/bench/`).
- Corpora: `data/corpora/1kg/NA12878/{MC,XA,QUAL}.bin` are real content, not LFS stubs (16.06 MB, 16.50 MB, 30.20 MB; SHA-256 verified against `data/corpora/1kg/SHA256SUMS`). Silesia not used.
- All runs: level 9 (`-l 9` / `Compression::best()`), independent streams (each chunk is a self-contained bz2 stream, like CRAM slices), 5 rounds. Numbers are the median over rounds of MB/s of uncompressed data, MB = 10^6 bytes, whole-corpus throughput per round.
- `libbz2` = stock 1.0.8 as linked by bz2-ng's bench (`test/reference`, -O2). `rs-sys` = `bzip2` 0.6.1 default features, which pulls `libbz2-rs-sys` 0.2.5 (confirmed with `cargo tree`: `bzip2 v0.6.1 default -> libbz2-rs-sys v0.2.5 rust-allocator`). `ng` = bz2-ng C. For decompression bz2-ng has two memory layouts (`fit`, `wide`); the ratios below use the faster of the two (they differ by 1-2%).
- Each chunk uses a fresh codec state (Init/End per stream in C; `Compress::new` / `Decompress::new` per chunk in Rust), single-threaded, one-shot over the whole stream. Rust compress is `Compress::compress_vec(.., Action::Finish)` into a preallocated Vec, decompress is `Decompress::decompress_vec` into a preallocated Vec.
- Correctness: bz2-ng's bench itself checks bz2-ng output is byte-identical to libbz2 and that decodes round-trip before timing. The Rust bench asserts round-trip for every chunk, and its output was byte-identical to `bzip2 -9` (CLI 1.0.8) on 7 MC 64 KiB chunks and 1 XA 1 MiB chunk.

## Results (MB/s, median of 5 rounds)

Chunk is the independent-stream size; "whole" = one stream per file (bz2 still splits it into 900 kB blocks internally).

| data | chunk | op | libbz2 C | libbz2-rs-sys | bz2-ng C | ng / rs-sys | ng / libbz2 | rs-sys / libbz2 |
|---|---|---|---|---|---|---|---|---|
| MC | 64 KiB | compress | 11.3 | 12.1 | 71.7 | 5.94x | 6.33x | 1.06x |
| MC | 64 KiB | decompress | 183.4 | 167.8 | 434.7 | 2.59x | 2.37x | 0.91x |
| MC | 1 MiB | compress | 10.2 | 11.0 | 70.6 | 6.42x | 6.96x | 1.08x |
| MC | 1 MiB | decompress | 122.3 | 118.5 | 462.7 | 3.91x | 3.78x | 0.97x |
| MC | whole | compress | 10.0 | 10.8 | 70.1 | 6.48x | 7.04x | 1.09x |
| MC | whole | decompress | 117.0 | 115.0 | 465.7 | 4.05x | 3.98x | 0.98x |
| XA | 64 KiB | compress | 25.5 | 26.6 | 73.3 | 2.76x | 2.88x | 1.04x |
| XA | 64 KiB | decompress | 84.8 | 76.6 | 269.9 | 3.52x | 3.18x | 0.90x |
| XA | 1 MiB | compress | 24.5 | 26.1 | 74.5 | 2.86x | 3.04x | 1.07x |
| XA | 1 MiB | decompress | 72.8 | 68.1 | 283.9 | 4.17x | 3.90x | 0.94x |
| XA | whole | compress | 23.8 | 25.6 | 74.1 | 2.89x | 3.12x | 1.08x |
| XA | whole | decompress | 71.5 | 66.7 | 281.9 | 4.23x | 3.94x | 0.93x |
| QUAL | 64 KiB | compress | 78.7 | 74.4 | 245.6 | 3.30x | 3.12x | 0.95x |
| QUAL | 64 KiB | decompress | 330.2 | 284.4 | 1087.3 | 3.82x | 3.29x | 0.86x |
| QUAL | 1 MiB | compress | 130.0 | 144.3 | 424.9 | 2.94x | 3.27x | 1.11x |
| QUAL | 1 MiB | decompress | 335.6 | 311.3 | 1252.1 | 4.02x | 3.73x | 0.93x |
| QUAL | whole | compress | 127.9 | 143.4 | 445.9 | 3.11x | 3.49x | 1.12x |
| QUAL | whole | decompress | 324.7 | 305.7 | 1300.3 | 4.25x | 4.00x | 0.94x |

The QUAL 64 KiB rs-sys decompress row had a 33% min-max spread across rounds (one slow round); the median is still representative of the ordering. Full per-round data is in the scratchpad (`c.tsv`, `rs.tsv`).

## Conclusions

libbz2-rs-sys is essentially at parity with C libbz2 1.0.8: 4-12% faster to compress and 2-14% slower to decompress on this M4 Pro, so "the speed seqair has today" is roughly stock libbz2. bz2-ng is far ahead of both: about 3x-6.5x faster to compress (the gap is largest on MC, which is highly repetitive and slow for libbz2's sort, ~70 MB/s vs ~11 MB/s; it is ~2.8x on XA and ~3x on QUAL) and about 2.6x-4.3x faster to decompress (smaller gap at 64 KiB chunks on MC, 2.6x, otherwise 3.5x-4.3x). Chunk size matters little for compression, but small chunks narrow the decompress gap somewhat. A Rust port that "keeps the performance" therefore means at least matching roughly 70 MB/s compress on MC/XA, 250-450 MB/s on QUAL, and 270-460 MB/s decompress on MC/XA, 1.1-1.3 GB/s on QUAL; merely matching libbz2-rs-sys would throw away 3-6x. Caveat: single machine (Apple Silicon, NEON kernel), no CPU pinning, 5 rounds, and bz2-ng's gain comes largely from non-portable kernels (libsais-style sort, SIMD), which is what a port would have to reproduce.

## Commands

```sh
S=/private/tmp/claude-501/-Users-pascal-Code-softleif-bz2ngrs/aa7a03aa-7bfe-4ba1-83b2-7e23e2dac2db/scratchpad/bench
D=/Users/pascal/code/tfenne/bz2-ng/data/corpora/1kg/NA12878

# C side (no tests needed for the bench target)
cmake -S /Users/pascal/code/tfenne/bz2-ng/c -B $S/cbuild -DCMAKE_BUILD_TYPE=Release -DBZ2NG_BUILD_BENCH=ON -DBZ2NG_BUILD_TESTS=OFF
cmake --build $S/cbuild --target bench -j8
cd $S
for c in 65536 1048576 0; do for n in MC XA QUAL; do
  ./cbuild/bench/bench -r 5 -l 9 -c $c -o c.tsv ${n}_$c=$D/$n.bin
done; done          # c.tsv columns: set level chunk kernel impl op round MB/s

# Rust side
(cd rsbench && cargo build --release)
mkdir -p dump
for c in 65536 1048576 0; do for n in MC XA QUAL; do
  DUMP=$S/dump ./rsbench/target/release/rsbench 5 $c ${n}_$c=$D/$n.bin >> rs.tsv
done; done          # rs.tsv columns: set impl op round MB/s

# byte-identity spot check against the bzip2 1.0.8 CLI
dd if=$D/MC.bin bs=65536 skip=3 count=1 2>/dev/null | bzip2 -9 | cmp - dump/MC_65536.3.bz2
```

## Appendix: Rust bench crate

`rsbench/Cargo.toml`:

```toml
[package]
name = "rsbench"
version = "0.1.0"
edition = "2024"

[dependencies]
bzip2 = "0.6.1"

[profile.release]
lto = "fat"
codegen-units = 1
```

`rsbench/src/main.rs`:

```rust
// usage: rsbench ROUNDS CHUNK name=file ...   (CHUNK 0 = whole file); prints TSV: set impl op round MB/s
use bzip2::{Action, Compress, Compression, Decompress, Status};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rounds: usize = a[1].parse().unwrap();
    let chunk: usize = a[2].parse().unwrap();
    for spec in &a[3..] {
        let (name, path) = spec.split_once('=').unwrap();
        let data = std::fs::read(path).unwrap();
        let step = if chunk == 0 { data.len() } else { chunk };
        let raws: Vec<&[u8]> = data.chunks(step).collect();
        let mut comps: Vec<Vec<u8>> = raws.iter().map(|r| Vec::with_capacity(r.len() + r.len() / 100 + 1024)).collect();
        let mut outs: Vec<Vec<u8>> = raws.iter().map(|r| Vec::with_capacity(r.len())).collect();
        let total = data.len() as f64;
        for round in 0..rounds {
            let t = Instant::now();
            for (r, c) in raws.iter().zip(comps.iter_mut()) {
                c.clear();
                let mut e = Compress::new(Compression::best(), 30);
                let s = e.compress_vec(r, c, Action::Finish).unwrap();
                assert!(matches!(s, Status::StreamEnd));
            }
            println!("{name}\tlibbz2-rs-sys\tc\t{round}\t{:.2}", total / t.elapsed().as_secs_f64() / 1e6);
            let t = Instant::now();
            for (c, o) in comps.iter().zip(outs.iter_mut()) {
                o.clear();
                let mut d = Decompress::new(false);
                let s = d.decompress_vec(c, o).unwrap();
                assert!(matches!(s, Status::StreamEnd));
            }
            println!("{name}\tlibbz2-rs-sys\td\t{round}\t{:.2}", total / t.elapsed().as_secs_f64() / 1e6);
            if round == 0 {
                for (r, o) in raws.iter().zip(&outs) { assert!(*r == &o[..], "round-trip mismatch"); }
            }
        }
        // for byte-identity checks against C
        if let Ok(dir) = std::env::var("DUMP") {
            for (i, c) in comps.iter().enumerate() { std::fs::write(format!("{dir}/{name}.{i}.bz2"), c).unwrap(); }
        }
    }
}
```
