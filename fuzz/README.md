# Fuzzing bz2ngrs

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) targets. The crate pins stable in
`rust-toolchain.toml`; cargo-fuzz needs nightly flags, so run it as `cargo +nightly fuzz ...` from the
repository root.

The oracle is the `bzip2` crate (libbz2 1.0.8 via `libbz2-rs-sys`). The differential rule is the one
of the hegel properties (`tests/common/oracle.rs` is shared with the fuzz crate): for an output
capacity `cap`, if the oracle decodes the first stream within `cap` bytes then we return `Ok` with
the same bytes; if it errors or would exceed `cap` we return an `Err`. Every target runs
a long-lived `Decoder` (one per process and SIMD level, so state left by earlier inputs reaches later ones) at every level, `Level::fallback()` included. `decompress_differential` also compares each result with that of a decoder made for the input.

| Target | Input | Checks |
| --- | --- | --- |
| `decompress_differential` | arbitrary bytes | the rule above, `cap = 4 * len + 1 MB` |
| `decompress_mutated` | arbitrary data, level, and structured edits (bit flip, byte set, truncate, insert, delete, block-size digit, block CRC, stream CRC) of its compressed stream | the rule above |
| `roundtrip` | arbitrary data, level, repeat count | libbz2-compressed data decodes to the same bytes into an exact `out` |

## Run

```sh
# Seeds (valid streams, plus small real CRAM blocks if corpus/cram-bzip2 exists); committed in fuzz/seeds/
cargo +nightly run --manifest-path fuzz/Cargo.toml --example gen_seeds

mkdir -p fuzz/corpus/decompress_differential fuzz/corpus/decompress_mutated fuzz/corpus/roundtrip

# A short run of each target (no long campaigns by default)
cargo +nightly fuzz run decompress_differential fuzz/corpus/decompress_differential fuzz/seeds/decompress_differential -- -max_total_time=60 -dict=fuzz/bzip2.dict
cargo +nightly fuzz run decompress_mutated fuzz/corpus/decompress_mutated -- -max_total_time=60
cargo +nightly fuzz run roundtrip fuzz/corpus/roundtrip -- -max_total_time=60

# Reproduce / minimise a crash
cargo +nightly fuzz run decompress_differential fuzz/artifacts/decompress_differential/crash-...
cargo +nightly fuzz tmin decompress_differential fuzz/artifacts/decompress_differential/crash-...
```

`-rss_limit_mb=4096` is worth passing for the differential targets: a decoder that allocates from a
header field is an out-of-memory finding, not a nuisance. `fuzz/corpus`, `fuzz/artifacts` and
`fuzz/target` are git-ignored.
