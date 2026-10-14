# Performance round 2: profile-driven

Second round after `06-perf-log.md`'s first. Each attempt, with its numbers,
is logged there (R2.1-R2.6); this note has the per-stage profile and the
result.

## How the profile was taken

- `cargo build --profile profiling -p bz2ngrs-bench` with `BZ2NG_C_DEBUG=1`
  (C with `-g`), one `samply record --unstable-presymbolicate` per cell and
  decoder (`--decoders ours` or `--decoders C`), 4 kHz.
- Each sample is charged to the deepest frame in the bench binary, expanded
  to its inline chain with `atos -i` (Rust names through `rustfilt`), and
  classified by the innermost frame that names a stage (bz2ngrs's functions,
  or bz2-ng's `quarter_hist`/`build_t`, `lane_step`/`walk_*`,
  `bzng_symbols_neon`/`bzng_fast_group`, `bzng_unrle_neon`/`output`,
  `bzng_next_piece`, `bzng_crc32_*`, `bzng_build_htable`, `decode`). Only
  samples under the bench's `dec_ours` or `dec_c` count, not the one-off
  verification passes. The scripts are not in the repository (the method is
  above).
- Shares become time with the cell's same-run ours/C (`--stat max`): C's whole
  decode is 100 in each cell. "Before" is the first round's final state
  (d363fd5), "after" is this round's.
- The machine ran at load averages of 140-700 for much of the session, so the
  shares are approximate (memory-bound stages suffer more under contention);
  read differences under about 2 units as noise.
- C's "block setup" includes its per-call init, allocation and teardown;
  ours has none (reused scratch).

## Per stage, C = 100

| cell | stage | C | ours before | ours after |
|---|---|---:|---:|---:|
| MC 64k | block setup | 4.4 | 1.6 | 1.4 |
| | symbol loop | 14.5 | 13.9 | 14.6 |
| | table build | 37.3 | 31.3 | 31.3 |
| | walk | 39.0 | 42.5 | 33.3 |
| | gather | 0.2 | 1.2 | 1.7 |
| | un-RLE1 | 3.7 | 8.9 | 2.5 |
| | CRC | 0.9 | 0.9 | 0.8 |
| QUAL 64k | block setup | 13.5 | 10.4 | 9.8 |
| | symbol loop | 33.1 | 32.0 | 32.1 |
| | table build | 13.1 | 12.5 | 12.1 |
| | walk | 17.1 | 16.5 | 13.4 |
| | gather | 0.1 | 1.4 | 1.8 |
| | un-RLE1 | 21.2 | 14.5 | 15.6 |
| | CRC | 1.7 | 2.4 | 2.1 |
| XA 1m | block setup | 3.9 | 1.7 | 1.4 |
| | symbol loop | 37.5 | 42.3 | 37.8 |
| | table build | 18.9 | 22.0 | 16.8 |
| | walk | 34.5 | 29.1 | 31.9 |
| | gather | 0.2 | 0.7 | 1.4 |
| | un-RLE1 | 4.5 | 4.4 | 1.4 |
| | CRC | 0.5 | 0.5 | 0.5 |
| ultima-bz6 (CRAM) | block setup | 2.3 | 2.0 | 2.1 |
| | symbol loop | 42.0 | 42.5 | 46.2 |
| | table build | 15.3 | 15.2 | 14.0 |
| | walk | 24.9 | 23.3 | 14.9 |
| | gather | 0.1 | 0.9 | 0.8 |
| | un-RLE1 | 14.6 | 11.7 | 13.4 |
| | CRC | 0.7 | 0.8 | 0.8 |
| v31-small (CRAM) | block setup | 9.3 | 4.4 | 2.9 |
| | symbol loop | 40.4 | 47.0 | 46.0 |
| | table build | 19.6 | 21.5 | 19.5 |
| | walk | 26.4 | 18.1 | 18.4 |
| | gather | 0.1 | 0.8 | 0.6 |
| | un-RLE1 | 3.5 | 4.7 | 2.4 |
| | CRC | 0.4 | 0.4 | 0.4 |
| hts level-3 (CRAM) | block setup | 6.6 | 3.8 | 3.4 |
| | symbol loop | 43.8 | 47.0 | 44.3 |
| | table build | 17.4 | 20.4 | 18.4 |
| | walk | 26.6 | 21.9 | 18.9 |
| | gather | 0.1 | 0.6 | 0.8 |
| | un-RLE1 | 4.9 | 5.2 | 4.0 |
| | CRC | 0.5 | 0.5 | 0.5 |

What the "before" profile showed, and what was done about it:

- **Walk**: a third of our walk at 64 Ki rows (and up to half on the small
  CRAM sets) was the straggler phase after the lock-step loop, because each
  of 24 lanes walked only three 2048-row segments. 512-row segments (R2.2).
- **un-RLE1** on literal-heavy data (MC): 2.4x C's, an 8-byte SWAR scan
  against C's 16-byte vector scan. A 16-candidate vector test in stretches of
  literals (R2.3).
- **Table build**: same shape as C's, latency-bound on four store-to-load
  chains over runs in L. Eight parts (R2.4).
- **Symbol loop**: up to 15% over C on the small CRAM sets and XA 1m. Three
  ideas, none measurable (R2.1, R2.5); stopped.
- **Block setup**: already below C's; a 10-bit primary table trims it at 16k
  (R2.6).
- **CRC** (0.4-2.4) and **gather** (0.6-1.8) were left alone (below).

## Result

`bz2ngrs-bench --stat max --rounds 5`, level 9, MB/s, load average 9:

| set | chunk | ours | bzip2 | C | ours/C |
|---|---|---:|---:|---:|---:|
| MC | 16k | 446.3 | 155.0 | 356.1 | 1.25 |
| MC | 64k | 469.0 | 152.8 | 403.6 | 1.16 |
| MC | 1m | 496.4 | 88.6 | 424.7 | 1.17 |
| XA | 16k | 260.2 | 72.6 | 224.1 | 1.16 |
| XA | 64k | 278.9 | 70.7 | 252.1 | 1.11 |
| XA | 1m | 289.6 | 61.9 | 266.1 | 1.09 |
| QUAL | 16k | 932.0 | 202.4 | 746.3 | 1.25 |
| QUAL | 64k | 1135.1 | 261.9 | 963.5 | 1.18 |
| QUAL | 1m | 1371.7 | 281.9 | 1183.5 | 1.16 |

ours/C before -> after (two alternating runs each, `--decoders ours,C`, 7
rounds, load 13-15): MC 1.122 -> 1.258, 0.995 -> 1.166, 1.003 -> 1.134; XA
1.100 -> 1.163, 1.016 -> 1.118, 0.991 -> 1.094; QUAL 1.153 -> 1.171, 1.112
-> 1.149, 1.081 -> 1.161.

CRAM sets, ours/C before -> after (5 rounds, load 9-13): level-3 1.004 ->
1.103, level-4 1.021 -> 1.086, v30-bz1 1.027 -> 1.105, v30-bz9 1.050 ->
1.105, v31-archive 1.170 -> 1.158, v31-bzonly 1.027 -> 1.103, v31-small 1.026
-> 1.101, ultima-bz6 1.038 -> 1.082, ultima-bz1 1.039 -> 1.102, ultima-bz9
1.040 -> 1.111, ultima-archive 0.992 -> 1.091.

## Left alone, and why

- **Symbol loop.** The remaining stage over C (v31-small, ultima-bz6). The
  decode half is latency-bound on the table lookup chain and the step half on
  the MTF `tbl` chain, both as in C; removing reloads, refill bounds checks or
  the run-flush loop exit did not move them. Fusing decode and step per
  symbol, so that the two chains overlap, is the untried idea that could;
  bz2-ng's structure (decode 50, then step 50) is the same as ours.
- **Wider MTF head.** The tail move does not show among the hot lines even on
  the archive sets.
- **CRC placement.** One pass over the block's output after un-RLE1, as C
  does; at most 2.4 units, and our PMULL loop matches C's instruction for
  instruction. Fusing it into un-RLE1 would put a 1 GB/s producer in front of
  a 20+ GB/s kernel for at most 2%.
- **Gather.** C's un-RLE1 reads the pool in place; ours copies the text
  first (0.6-1.8 units). Removing the copy needs un-RLE1 state carried across
  pieces of a few hundred bytes, which costs margins in the bulk loop.
- **Lane counts, prefetch**: tuned and rejected in round 1.

## x86

No x86 machine; checked with `cargo rustc --release --lib --target
x86_64-apple-darwin -- --emit asm` (the levels are dispatched at run time, so
no `-C target-cpu`). The un-RLE1 vector test is `vpcmpeqb`/`vpand`/
`vpmovmskb`/`test` at the Avx2 level, with no new spills there (Sse4_2: 18
more stack references, in inlined bit-reader code). An 8-part table scatter
in one pass spilled about 30 times per iteration, so it runs as two passes of
four (3 and 9 stack references per iteration, against 4 before); the 8-part
histogram loop has none.
