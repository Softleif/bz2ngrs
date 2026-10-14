# Performance log: closing the gap to bz2-ng C

Goal: at least 0.95x bz2-ng C in every cell of `bz2ngrs-bench` (MC, XA, QUAL
at 16k, 64k, 1m, levels 9 and 6) and on the CRAM corpus sets. Machine: M4 Pro,
shared with a fuzz campaign on 3 cores, so absolute MB/s swing by 10-40%
between runs.

## Method

- The bench (`bz2ngrs-bench`) interleaves ours, the bzip2 crate and C within
  each round; its ours/C ratio is the target metric, but it still moves by
  about +-4% between runs here.
- For a single change, `examples/prof.rs` (ours only) is run alternately from a
  saved baseline binary and the new build, 1.5 s each, 5 times. Under this
  load the slow outliers are interference, so the comparison is the best of
  the 5 runs (the median is also noted).
- Line-level profiles: samply, then `atos -i` on the sampled addresses (the
  decoder is one inlined function, so function-level profiles say little).

## Baseline (ba8f092)

ours/C, level 9 / level 6:

| set | 16k | 64k | 1m |
|---|---|---|---|
| MC | 0.94 / 0.94 | 0.90 / 0.91 | 1.00 / 1.00 |
| XA | 0.94 / 0.95 | 0.92 / 0.91 | 0.98 / 0.99 |
| QUAL | 0.87 / 0.87 | 0.83 / 0.83 | 0.89 / 0.89 |

CRAM sets (repeated runs): hts-specs-level-3 0.85-0.88, level-4 0.82-0.85,
v30-bz1 0.92, v30-bz9 0.91-0.92, v31-archive 0.98, v31-bzonly 0.96,
v31-small 0.88, ultima-bz6 0.80, ultima-bz1 0.94, ultima-bz9 0.89,
ultima-v31-archive 0.91.

Profile, QUAL 64k: walk step 23%, un-RLE1 15% (most of it run fills), symbol
step 11%, table build 9%, Huffman table builds 4%.

## Changes

### 1. Run fill: two unconditional 16-byte stores, loop only past 32 (kept)

QUAL runs average about 33 output bytes, so the old loop of
`ceil(total / 16)` stores mispredicted its exit on most runs. Now two stores
always, then a loop for longer runs; MARGIN goes from 48 to 64 text bytes to
cover the larger overshoot.

Best of 5, MB/s, base -> new: QUAL 16k 659 -> 678 (+3%), 64k 788 -> 816
(+3.5%), 1m 1028 -> 1056 (+2.7%).
