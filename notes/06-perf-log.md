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

### 2. Walk: lanes in registers, more lanes at smaller blocks (kept)

From here on, `prof` reports its best full pass over the data (each pass is
15-40 ms), which removes almost all of the load noise (spread about 1%).

The lanes lived in a stack array, so every step's next row went through
store-to-load forwarding on top of the table load. Now each lane's hot state
(row, write position) goes by value and the cold state (bytes written,
segment, mark) sits in an array only the rare steps touch. A lane takes its
next chunk as soon as the current one fills, so there is no chunk-end field.
For 1, 2 and 8 lanes a macro writes the lock-step loop out with one local per
lane; 24 lanes keep their arrays (unrolled, they only spill).

Then lane counts, measured per size: 24 lanes from 4 Ki rows (at least 72
segments), 8 from 1 Ki (at least 24 segments). bz2-ng uses 24 from 64 Ki and
8 from 16 Ki rows.

Best pass, MB/s, base -> new:

| cell | base | new |
|---|---:|---:|
| QUAL 64k | 855 | 965 (+13%) |
| QUAL 16k | 725 | 824 (+14%) |
| MC 64k | 357 | 403 (+13%) |
| MC 16k | 325 | 380 (+16%) |
| MC 1m | 435 | 440 (=) |

Rejected along the way:
- Unrolling 24 lanes: -6% on MC 1m (spills).
- A single `live &= l` flag for the loop exit instead of the per-round dead
  mask: LLVM fused the round into one loop with a serial flag chain through
  every step, -10% on MC 1m (walk 0.84 -> 1.2+ ns/row). The per-round mask
  stays for the memory loop.
- 24 lanes from 1 Ki rows: QUAL 16k 784 vs 824 with 8 lanes.
- Unrolled 8 lanes vs the memory loop at 8: memory loop worse (1.49 vs 1.26
  ns/row).

Bench matrix (ours/C), control baseline -> new, same session:

| set | 16k | 64k | 1m |
|---|---|---|---|
| L9 MC | 0.88 -> 1.18 | 0.92 -> 1.03 | 1.03 -> 1.01 |
| L9 XA | 0.90 -> 1.07 | 0.91 -> 0.97 | 1.02 -> 1.04 |
| L9 QUAL | 0.86 -> 0.99 | 0.86 -> (glitch, 0.47) | 0.89 -> 0.89 |
| L6 MC | 0.93 -> 1.17 | 0.90 -> 1.00 | 1.00 -> 1.02 |
| L6 XA | 0.95 -> 1.06 | 0.91 -> 1.01 | 0.99 -> 0.98 |
| L6 QUAL | 0.89 -> 1.05 | 0.82 -> 0.96 | 0.87 -> 0.87 |

CRAM: level-3 0.89 -> 0.94, level-4 0.89 -> 0.80 (0.11 MB, noisy),
v30-bz1 0.93 -> 0.96, v30-bz9 0.91 -> 0.99, v31-archive 0.97 -> 1.07,
v31-bzonly 0.92 -> 0.99, v31-small 0.80 -> 1.06, ultima-bz6 0.94 -> 0.96,
ultima-bz1 0.91 -> 0.98, ultima-bz9 0.88 -> 0.93, ultima-archive 0.94 -> 0.93.

### 3. Run fill: 32-byte stores, overlapping last store (kept)

Long runs (QUAL 1m has many near the 259-byte maximum) took one 16-byte store
per loop iteration. Now 32-byte stores (`u8x32`), and a run longer than 32
ends with a store that overlaps the previous one instead of overshooting.

Best pass, MB/s, base -> new (3 alternating runs each): QUAL 1m 1096 -> 1125
(+2.7%), QUAL 64k 970 -> 984 (+1.4%).

### 4. Walk prefetch at 24 lanes (rejected)

`prfm pldl1keep` (aarch64 `asm!`) on the next row's table entry right after
each step of the 24-lane loop. Best pass, base -> new: MC 1m 440 -> 434
(-1.3%), MC 64k 411 -> 406 (-1.2%). At these sizes the table fits in the M4
Pro's L2 and the out-of-order window already overlaps the 24 lanes' loads;
the prefetch only adds instructions. Reverted (no `unsafe` added).

### 5. Run fill without a variable-length loop (kept)

The fill loop's trip count varies with every run, so its exit mispredicts on
most runs. A first bulk stage now runs while 372 text bytes remain (enough to
decode to at least 288 more output bytes): its fill is one 32-byte store,
plus eight more for runs over 32 bytes, with no loop. The 64-byte-margin
stage and the byte tail follow as before.

Best pass, MB/s, base -> new: QUAL 1m 1146 -> 1197 (+4.5%, 5 alternating
runs each, median +4.9%), QUAL 64k 978 -> 1011 (+3.3%), XA 64k 259 -> 258
(=).
