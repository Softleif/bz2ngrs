# Adversarial review of the `port` decoder (HEAD ba8f092)

Scope: src/{bits,huffman,decode,bwt,unrle,lib,crc}.rs against libbz2 1.0.8
(read through libbz2-rs-sys 0.2.5 `decompress.rs`). Read-only; nothing in the
repo was changed. "Demonstrated" means a test program in
`/private/tmp/claude-501/-Users-pascal-Code-softleif-bz2ngrs/aa7a03aa-7bfe-4ba1-83b2-7e23e2dac2db/scratchpad/review/`
ran it (arm64 NEON and Fallback; x86_64 Sse2/Sse4_2/Fallback with PCLMUL under
Rosetta; AVX2 not available there); "reasoned" means argued from the code.

## Verdict

No Blocking and no Required findings. No panic, no accept/reject difference
from libbz2, no write outside the contract, and no stale-scratch effect was
found. Four Suggestions (test gaps and hardening) and two notes follow.

## What was run (all demonstrated, all clean)

- `forge.py`/`forge2.py`: a small bzip2 block writer with arbitrary L and
  `origPtr` and a CRC computed from a Python model of libbz2's walk and un-RLE1
  (the model agrees with the bzip2 crate on every case). About 1,200 streams:
  random (multi-cycle) permutations, sorted L (n fixed points), all-equal L,
  n = 1..900,000 (1, 3, 16383, 16384, 65535, 65536, 900000 hit every lane
  count), randomised blocks, 2..6 tables with incomplete and oversubscribed
  code sets (codes up to 20 bits), nSelectors up to 32767, 256-symbol
  alphabets. bz2ngrs is identical to the bzip2 crate in value and in
  accept/reject, in release and in debug with overflow checks.
- `edge.py`/`edge2.py`: 60 hand-built header cases, all identical to the oracle:
  digit `0`/`:`/`1`; nGroups 0, 1, 6, 7; nSelectors 0, exact, too few, 32767;
  selector unary of nGroups ones (also at index >= 18002); code length start
  0/1/20/21/31 and steps to 0 or 21 on the last symbol; empty bitmap; EOB
  first; `origPtr` = n, n-1, 10+100000*bs, +1; BZh1 with n = 100000/100001
  (literal and run forms); runs of 900000/900001; RUNA x19..22; bad block CRC;
  bad combined CRC; trailing data; zero blocks (good and bad CRC); bad end
  magic; text ending in 4 equal bytes with and without count byte.
- `mutfuzz` (debug, overflow checks, 800k mutated streams, caps 0/10/1000/1M,
  default and Fallback level): 0 panics, 0 accept/reject differences.
- `trunc`: every prefix of 60 valid streams (16,543 prefixes): ours is
  `UnexpectedEof` exactly when libbz2 reports `UnexpectedEof`.
- `tailfuzz`: 3000 run-heavy valid streams x 40 oversize `out` lengths x 2
  levels (also on x86_64): `Ok(len)`, correct bytes, and every byte past `len`
  still 0xA5. Also `out` shorter by 1..20 gives `OutputTooSmall` every time.

## Per-area findings

### 1. Panics (reasoned, plus the fuzzing above)

Every index in the hot paths is masked or bounded: `HTable::decode` indexes
`prim` with 11 bits of a 2048-entry table; `store16` masks `at` to 2^20-1 on a
2^20+64 array; `PERM[idx]` has idx < 16; `mtf[idx & 255]`; BWT arrays are
indexed with masks or `% SEGS/CHUNKS`. The `Bits` invariants hold: `cnt` is at
most 63 after `refill` (`cnt | 56`), so `>> cnt` and `63 - cnt` cannot
overflow, and `pos*8 >= cnt`. `consume` is always preceded by enough bits (8
for selectors, 2 for lengths, 56 for the symbol loop; two codes of <= 20 bits
per refill). A group never exceeds 50 symbols (k is even at loop top; syms[64]).
`run` stays below 2^22 (rshift < 21 enforced before use), so
`nb + run` cannot overflow u32. `limit_lj` < 2^28, `first_code` < 2^27.
`derandomise` has r >= 50, so `start + r - 2` cannot underflow.

### 2. Parity with libbz2 1.0.8

All items in the checklist match (demonstrated): digit 1..=9 only; block magic
0x31 vs end magic 0x17, anything else is an error; nGroups in 2..=6;
nSelectors 0 is invalid, the first 18002 are kept, the rest are read and
still checked against nGroups; code length range is checked before every
step; zero symbols in use is an error; `origPtr > 10+100000*bs` and
`origPtr >= nblock` (including nblock = 0) are errors; run digit count limit
(`rshift >= 21`, i.e. N >= 2^21) and `nblock > 100000*bs` are errors (the first
is unobservable in accept/reject, since 20 digits already exceed 900,000);
randomised blocks are accepted and derandomised before un-RLE1 as libbz2
does; block CRC is compared after the block's output; combined CRC is
`rotl(1) ^ crc`; trailing bytes are ignored; a stream with zero blocks is
valid iff the stored CRC is 0; text ending in 4 equal bytes without a count
is an error, a count byte as last byte is fine.

Oversubscribed/incomplete code sets, and stale `perm` reads: libbz2 can read
`perm[i]` for i >= alphaSize only if a code matched past its length's count,
which `zvec <= limit` rules out; the same holds for `decode_long`
(`v < limit_lj[l]` implies index < first_idx + count[l] <= alphaSize), so the
fact that `perm` is reused between blocks never matters (reasoned, and the 157
rejected "over" streams match the oracle).

### 3. `out` contract

Demonstrated: bytes past the returned length stay untouched on success
(unrle.rs:12 MARGIN argument holds), `OutputTooSmall` comes out of the tail
loop and `fill` exactly when the output would pass `out.len()`. Block CRC is
always checked: a block has n >= 1 (origPtr < n), so unrle yields >= 1 byte
and the `unwrap_or_default()` at decode.rs:117 never produces an empty slice
for a non-empty block.

### 4. Thread-local scratch (reasoned + demonstrated by mixed-size runs)

No state leaks across calls. Everything read is written first in the same
block: L[0..n) (stores from `step`), `tt[0..n)` (a full overwrite, so SPLIT
bits are fresh), pool/segment records of every walked segment (a segment in
the origin cycle is always walked), `text[0..n)` (gather), tables 0..nGroups,
`lens[t][..alpha]`, `mtf[..n_in_use]`, `selectors[..n_selectors]`. Head lanes
>= n_in_use are stale but are never moved to the front (idx < n_in_use).
Selector values are < nGroups, so `tables[7]`-style stale tables are never
used. A panic mid-decode drops the taken box (thread-local is `None`, next
call allocates anew); no poisoning. Not reentrant by construction (no
callbacks). Memory: one Scratch per thread, never shrunk.

## Findings

### S1. Suggestion: tests never reach the BWT/un-RLE1 code with a valid CRC and hostile structure

tests/common/oracle.rs/props.rs mutate real streams, which die at the CRC (or
earlier). Multi-cycle permutations (`gather` wrapping round a short cycle, the
segment bookkeeping for segments never gathered, splitter on a fixed point)
are only exercised if the CRC matches. They are correct today (about 300 forged
streams, all sizes, release+debug), but nothing keeps them so. Fix: add a
property with a tiny bzip2 block writer (`forge.py` in the scratchpad is ~90
lines; a uniform-length Huffman code is enough) that writes random L, random
`origPtr`, random randomised bit and a CRC from a naive reference walk, then
compares against the bzip2 crate. Demonstrated that it passes.

### S2. Suggestion: `larger_out` does not check what the doc promises

tests/common/props.rs:54-58 compares only `out[..n]`, while lib.rs:83-84
promises that bytes past the returned length are untouched, and unrle.rs
keeps it by an overshoot-then-overwrite argument (MARGIN). Fix: in
`larger_out`, also assert `out[n..].iter().all(|&b| b == 0xA5)`. Demonstrated
that it holds (tailfuzz above), so this is a regression guard only.

### S3. Suggestion: error kind for the un-RLE1 "4 equal bytes, no count" tail

unrle.rs:93. libbz2 reads one byte past the block (wrapping to the text start),
emits `4 + text[0]` bytes and only then returns the data error; ours returns
`Corrupt` after 4 bytes. Same verdict (reject) either way, but with
`4 + o <= out.len() < o + 4 + text[0]` libbz2 would report "output full" first
and ours says `Corrupt`. Input shape: any block whose text ends in four equal
bytes (found by fuzzing: `mism_3_25773.bz2` in the scratchpad, `cap`=10;
the bzip2 crate under `take(cap+1)` returns Ok(11 bytes), ours
Err(Corrupt)). The hegel rule (Err vs Err) accepts it. Fix: none needed; if
the doc line "OutputTooSmall as soon as the output would exceed out.len()" is
meant for corrupt streams too, say it only holds for streams libbz2 accepts.

### S4. Suggestion: unsafe hardening

- decode.rs:39: SAFETY is correct today (Scratch, Bwt, Meta, HTable are
  arrays and scalars of u8/u16/u32/i32 only; no bool, enum, reference,
  NonZero). It is not machine-checked, so a later `bool` or `&` field would
  silently be UB. Fix: a comment on `Scratch`, `Bwt`, `Meta`, `HTable` ("must
  stay all-zero-valid") or a derive of `bytemuck::Zeroable`. `new_zeroed`
  aborts (not panics) on OOM; acceptable, worth a sentence in the docs.
- crc.rs:132,137,259: `crc_checked`, `pmull_checked`, `pclmul_checked` are
  safe `pub(super)` fns that are unsound if called on a CPU without the
  feature; only `paths()` guards them. Today the sole caller is `paths()`.
  Fix: declare them `unsafe fn` (or keep private to a submodule) so the
  compiler enforces the invariant. Feature detection is otherwise right on
  every path: x86 needs pclmulqdq+sse4.1 (sse4.1 implies ssse3 for
  `pshufb`), aarch64 needs crc (+aes for PMULL), the length guards (`< 256`,
  `< 64`) make `blocks[0]` safe, and the dispatch is a `OnceLock`.

### Notes (no action)

- Error kind: `UnexpectedEof` vs `Corrupt` is exact for every prefix of a valid
  stream (demonstrated, 16.5k prefixes). For other damaged inputs it is
  best-effort, as the lib.rs doc already says.
- Scratch stays resident at up to 5.4 MB per thread for the life of the
  thread (documented in lib.rs); after thread-local teardown each call
  allocates a new 6 MiB zeroed box (reasoned; harmless).
- AVX2 kernels were not run (Rosetta has no AVX2); NEON, Sse2, Sse4_2 and
  Fallback were.
