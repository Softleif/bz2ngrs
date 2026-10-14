# bz2-ng decoder: analysis and Rust port plan

Read-only analysis of `/Users/pascal/code/tfenne/bz2-ng` (paths below are relative to `c/lib/` unless stated otherwise), done 2026-10-14. Target: a one-shot Rust decoder, `decompress(src: &[u8], out: &mut [u8]) -> Result<usize, Error>`, built on `fearless_simd` 1.1.0. It must be panic-free on hostile input and run under fuzzing with ASAN. The consumer is seqair's CRAM reader (see `01-seqair-usage.md`). Its blocks are written by htslib at `BZh6` (600 kB maximum block), are 20–120 kB uncompressed (rarely up to about 1 MB), and almost always hold a single bzip2 block. Small-block latency therefore matters more than peak throughput.

## TL;DR

- The decoder has five stages: header parse → symbol loop (Huffman + inverse MTF + RUNA/RUNB) → inverse-BWT table build → multi-chain walk into a chunk pool → un-RLE1 + CRC. Everything the C does for the resumable `bz_stream` API can go:
  - the per-bit `case`-label state machine;
  - the 160-byte "exact path" and the re-decode to libbz2's exact error bit;
  - give-back of read-ahead input;
  - the byte-resumable output stage with `pend_len`/`ur_eq`/`corrupt_pending`/`BZNG_DISCARD`;
  - `minibuf`;
  - small mode;
  - the in-formula ("fit") layout with 3-byte entries and the guess table;
  - verbosity.
- The **chunk pool is not a resumable-output artefact.** It exists because the 24 (or 8 or 2) chains write segments whose text offsets are unknown until they have been walked. It stays for any walk with more than one lane. What goes away is the resumable *reader* of the pool. un-RLE1 can write **straight into the caller's `out` slice**. The simplest form first gathers the pool's segments in text order into one contiguous scratch buffer (one memcpy of `n` bytes), then runs a single un-RLE1 pass from it into `out`. CRC then runs once per block over the contiguous slice of `out`.
- For SIMD, everything the MTF head and un-RLE1 need maps onto `fearless_simd` 1.1 portable ops. The ops are splat, `swizzle_dyn`, `slide::<15>`, compares, `select`, `to_bitmask` and lane access. The one gap that matters is NEON movemask: `to_bitmask` is 5 instructions there, against bz2-ng's 1-instruction `shrn` nibble mask. Use `kernel!` with `vshrn_n_u16` for that. Carry-less multiply and the ARM `crc32` instructions are not in `fearless_simd`, so the CRC is ported with `core::arch`, which has stable intrinsics for all of them. No crate in the local registry does hardware CRC-32/BZIP2.
- Prefetch: x86 `_mm_prefetch` is stable and is a safe `fn` now. On aarch64, `_prefetch` and `core::hint::prefetch_read` are still unstable, so use `asm!("prfm pldl1keep, [{}]")`. **At seqair's block sizes the C does not prefetch at all**, because prefetch is only on for at least 16 lanes, which means `n >= 65,536` rows.
- Bounds checks can be removed safely by making the big scratch arrays fixed powers of two and masking the indices:
  - `L` and the pool as `[u8; 1<<20 (+slack)]`;
  - the table as `[u32; 1<<20]`;
  - the Huffman primary table as `[u16; 2048]`;
  - the MTF list as `[u8; 272]`.
  
  A valid index never exceeds `900,000 < 2^20`, so masking changes nothing for valid input, and LLVM drops the checks. The scratch is reused per thread, so its fixed size costs only the pages that are actually touched.
- Size: about 1,100 lines of C matter (the 880-line CRC table aside). Estimated Rust: about 1,400 lines of library code plus about 500 lines of tests and fuzz harness.
- Perf risks:
  - register pressure and loop shape in the symbol loop, though the clang variants apply because Rust uses LLVM;
  - getting `fearless_simd` inlining right;
  - forcing a 24-way unroll without a pragma;
  - per-call fixed costs at 20–120 kB (allocation, page faults, six 4 KB Huffman tables).

## 1. Pipeline

### 1.0 Structure in the C

- `decompress.c` holds libbz2's `BZ2_bzDecompress` driver (`decompress.c:657-741`), the header parser `decode()` (`decompress.c:287-563`) and the resumable output stage `output()` (`decompress.c:102-239`).
- `dec.h` holds the state struct `bzng_dstate` (`dec.h:135-212`), the bit reader (`dec.h:248-288`), the Huffman decode primitives and the 50-symbol group decoder (`dec.h:290-415`).
- `kern_template.h:834-1211` holds the per-tier kernels: the symbol loop `bzng_symbols_*` (`:998-1075`) and the bulk un-RLE1 `bzng_unrle_*` (`:1149-1207`). The template is instantiated by `kern_scalar.c`, `x86/kern_sse4.c` (`-march=x86-64-v2`), `x86/kern_avx2.c` (`-march=x86-64-v3`) and `arm/kern_neon.c`. Dispatch goes through self-replacing function pointers (`decompress.c:51-85`, `kern.h`, `x86/kern_impl.h`, `arm/kern_impl.h`).
- `vec.h` holds the 16-byte vector abstraction, with one NEON and one SSE4.1 implementation (`vec.h:23-191`).
- `dec_block.c` does block finishing: Huffman table build (`:30-94`), table build (`:96-336`), splitters (`:338-395`), the walk (`:397-604`), small mode (`:606-661`), the text reader (`:697-802`) and array sizing (`:804-847`).
- CRC: `crc32.c` (slice-by-16 and dispatch), `crc32_table.c` (16×256 table), `x86/crc32_pclmul.c`, `x86/crc32_vpclmul.c`, `x86/crc32_clmul.h`, `arm/crc32_armv8.c`, `arm/crc32_pmull.c`.

### 1.1 Bit reader and header parsing

Bit buffer (`dec.h:111-115`, `dec.h:250-288`): a `u64 buf` holds the next bits MSB-aligned, plus `cnt` valid bits and `p`/`end`. Three operations:

- `bzng_br_fill` (`:256`) pulls whole bytes one at a time until `n <= 32` bits are held. This is libbz2's GET_BITS and is used by the header and the exact path.
- `bzng_br_refill_fast` (`:273`) does `buf |= be64(p) >> cnt; p += (63-cnt)>>3; cnt |= 56`. It is branch-free and leaves at least 56 bits. **It reads 8 bytes at `p` without a check**, and the caller guarantees they are there.
- `bzng_br_take` (`:282`) takes `n` bits from the top.

Header (`decompress.c:293-502`), as straight-line logic once the resumable `GET_BITS_CASE` labels are removed:

1. `BZh` and a digit 1–9 (`:293-311`). The per-stream arrays are allocated here, sized by the digit.
2. Block magic `0x314159265359` or end-of-stream magic `0x177245385090` (`:313-338`, `:525-556`).
3. Block CRC (32 bits), the randomised bit, `origPtr` (24 bits, at most `10 + 100000*digit`) (`:344-368`).
4. 16-bit `in_use16` followed by up to 16×16 bits giving the `seq_to_unseq` map. `n_in_use == 0` is an error. `alpha_size = n_in_use + 2` (`:370-389`).
5. `n_groups` (3 bits, 2..6) and `n_selectors` (15 bits, ≥ 1) (`:391-400`). Selectors are unary codes with MTF undone as they arrive. **Fast path**: while 8 input bytes remain, a selector is the count of leading ones of `buf`, read with one `bsr64(~buf|1)` (`:411-422`). In Rust this is `(!buf).leading_zeros()`. Only the first 18,002 selectors are kept; the rest are read and dropped (`:435-446`).
6. Code lengths (`:454-489`): a 5-bit start, then per symbol `10`(+1)/`11`(-1)/`0`(end), range-checked to 1..20 before every step. There is a two-bits-at-a-time fast path from the 64-bit buffer.
7. Six Huffman tables are built (`:491-492`), then `eob = n_in_use+1`, `nblock_max = 100000*digit`, and the MTF list is initialised from `seq_to_unseq` (`:494-501`).

After the symbol loop, `origPtr >= nblock` is rejected (`:514-517`).

Driver details that are only for the legacy API, and need not be ported:

- `sync_input` give-back of read-ahead bytes (`:627-648`);
- `sticky` errors (`:668-669`);
- the `total_in`/`total_out` 64-bit counters;
- the 21-bit "invalid pattern" timing (`dec.h:32-33`, `dec.h:339-350`).

Block CRC check and combined-CRC fold: `combined = rotl(combined,1) ^ block_crc` (`:699-711`). The combined CRC is checked at `BZ_STREAM_END` (`:720-733`).

### 1.2 Huffman tables and the 50-symbol group fast path

Table (`dec.h:96-108`, built in `dec_block.c:30-94`):

- `prim[1<<11]` holds `u16` entries `symbol<<5 | length`, where length 0 means "longer than 11 bits or no code". K = 11 (`dec.h:30`).
- For longer codes there is a canonical slow path: `limit_lj[l]` is the exclusive end of length-`l` codes, left-justified to 20 bits, alongside `first_code[l]`, `first_idx[l]`, `perm[258]` and `maxlen`.
- The build reproduces libbz2's treatment of non-prefix-complete sets exactly. In an oversubscribed set, codes past the end of the code space are unreachable, because filling stops when the primary table is full (`dec_block.c:60-63`). In an incomplete set, unused patterns are "no code". This is worth keeping, because it costs nothing and accepts exactly the streams libbz2 accepts.
- Each table is about 4.9 kB, so all six are about 29 kB.
- The gcc-only `#if` at `dec_block.c:73-86`, which does four entries per 8-byte store, does not apply to LLVM. Write the plain fill and let LLVM vectorise it. `fill()` over a sub-slice works.

`bzng_decode_code` (`dec.h:313-331`) does one primary lookup on `buf >> 53`. On miss it runs a linear scan `l = 12..maxlen` of `v < limit_lj[l]`. The gcc-only `BZNG_CODE_LEN` bzhi trick (`dec.h:300-305`) is irrelevant for LLVM.

`bzng_fast_group` (`dec.h:371-399`) decodes up to 50 codes into `u16 syms[50]` and stops after EOB. It refills once per two codes, since one refill leaves at least 56 bits and two codes need at most 40. It returns −1 on "no code". Its precondition is `BZNG_FAST_IN_MARGIN = 160` input bytes (`dec.h:35-38`): 50×20 bits = 125 bytes plus the 8-byte refill load. The gcc `noinline` (`dec.h:358-362`) is gcc-specific. clang (and so Rust) inlines it.

### 1.3 Symbol loop and the MTF head in a vector register

`kern_template.h:998-1075`. The outer loop does "fast groups" while `group_pos == 0 && span(p,end) >= 160 && group_no+1 < n_selectors` (`:1010-1012`). It decodes one group into `syms[]` and then runs `SYMBOL_STEP` over each symbol (`:1023-1034`). On a "no code" pattern it breaks to the exact path, which finds libbz2's error position. On `BZ_DATA_ERROR` from a step it rewinds to the group start and re-decodes `k+1` codes with `bzng_skip_codes` to stop at libbz2's exact bit (`:1026-1031`). Otherwise the **exact path** (`:1036-1064`) decodes one symbol at a time from the bits really available (`bzng_decode_exact`). It returns `BZNG_NEED` to suspend.

Register-resident state: `L`, `nmax`, `eob`, `nb`, `run`, `rshift`, and `head`, a `vec_t` holding MTF entries 0..15. Entries 16..255 live in `s->mtf[16..]`, which is `u8[272]` with 16 bytes of slack (`dec.h:178`).

`SYMBOL_STEP`, vector form (`kern_template.h:901-947`), is branch-free for run and literal symbols:

1. If `sym == eob`, flush the pending run with `memset` and return EOB.
2. `is_run = sym < 2` and `flush = is_run ? 0 : run`. Error if `(is_run && rshift >= 21) || nb + flush + !is_run > nmax`. This is libbz2's N ≥ 2^21 check and its block overflow check.
3. `front = bcast_lane(head, 0)`. An **unconditional 16-byte store** of `front` goes to `L+nb`. If `flush > 16`, a loop of 16-byte stores follows. Then `nb += flush`. Stores past `nb` land in slack.
4. `idx = is_run ? 0 : sym-1`. Index 0 is a no-op move-to-front for run symbols.
   - When `idx < 16`: `cv = bcast_lane(head, idx)`, `head = sel_upto(idx, shift1(cv, head), head)`, `c = lane0(cv)`.
   - Otherwise `c = mtf_tail_move(s->mtf, idx, lane15(head))` and `head = shift1(dup(c), head)`. `mtf_tail_move` (`:849-863`) removes `m[nn]` and shifts `m[16..nn-1]` up one, 16 bytes at a time, with `shift1` carrying across vectors and `sel_upto` on the last vector.
5. `L[nb] = c` and `nb += !is_run`. `run = is_run ? run + ((sym+1) << rshift) : 0`, which is bijective base 2. `rshift = is_run ? rshift+1 : 0`.

The `IS_RUN`/`KEEP_*` macros (`:878-886`) come in two variants. gcc uses mask arithmetic and the sign of `sym-2`, because gcc branches on the random run/literal pattern. clang uses plain ternaries, which it turns into cmov, and is *slower* with masks on x86. **Rust should follow the clang variant.** If LLVM ever turns those ternaries into branches, wrap them in `core::hint::select_unpredictable` (stable since 1.88).

The portable step (`:950-986`) keeps the whole list in memory and does `memmove(mtf+1, mtf, nn)` per literal.

**Vector ops used, by tier** (`vec.h`):

| `vec.h` op | Use | SSE4 tier (`-march=x86-64-v2`) | AVX2 tier (`-march=x86-64-v3`) | NEON |
|---|---|---|---|---|
| `vec_ld`/`vec_st` (`:34-44`, `:123-133`) | MTF tail, `L` run stores, un-RLE1 | `movdqu` | `vmovdqu` | `ld1`/`st1` |
| `vec_dup(c)` (`:47`, `:136`) | carry, un-RLE1 fill | `_mm_set1_epi8`, i.e. `movd` + `pshufb` with zero | `vpbroadcastb` | `dup` |
| `vec_bcast_lane(v,k)` (`:99`, `:188`) | `front`, `cv` | `pshufb(v, set1(k))` | same, VEX | `tbl` (`vqtbl1q_u8`) with `dup(k)` |
| `vec_shift1(prev,cur)` (`:73`, `:160`) | MTF shift | `palignr(cur, prev, 15)` | `vpalignr` | `ext(prev, cur, #15)` |
| `vec_sel_upto(k,a,b)` (`:79`, `:166`) | MTF merge | `pcmpgtb(set1(k+1), iota)` + `pblendvb` | `vpcmpgtb` + `vpblendvb` | `cmhs` (`vcleq(iota, dup k)`) + `bsl` |
| `vec_lane0` (`:88`, `:177`) | MTF output byte | `movd` | `vmovd` | `umov b[0]` |
| `vec_lane15` (`:93`, `:182`) | carry into the tail | `pextrw 7` then `>> 8` | same | `umov b[15]` |
| `vec_eq`, `vec_and` (`:53-62`, `:142-151`) | un-RLE1 run candidates | `pcmpeqb`, `pand` | VEX | `cmeq`, `and` |
| `vec_mask` (`:65`, `:154`) | un-RLE1 | `pmovmskb` (1 bit/lane) | same | `shrn #4` on u16 lanes → 64-bit nibble mask (4 bits/lane), `VEC_MB=4` |

The tiers differ only in encoding. The AVX2 tier's main gain in the decoder is BMI2 (`shlx`/`shrx`) in the bit reader: a variable shift on x86 without BMI2 costs `shl r, cl`, which is 3 µops on Intel. The tier also gains VEX three-operand forms that save `movdqa` copies, `vpbroadcastb`, `lzcnt` and `movbe`. In `fearless_simd`, the `Avx2` level enables `avx2,bmi1,bmi2,lzcnt,movbe,...` (`fearless_simd-1.1.0/src/generated/avx2.rs:39`), so **the bit reader must be compiled inside the dispatched function** to get BMI2. Do not dispatch only the vector parts.

### 1.4 Inverse BWT: table build in 4 segments

`bzng_finish_block` (`dec_block.c:632-695`). The default layout (`WIDE`, `dec.h:57-87`) is the one to port.

- `quarter_hist` (`:101-114`): per-quarter symbol histograms of `L[0..n)`, with `q = n/4`, in one interleaved pass so that four counter chains run in parallel. These four "segments" of the spec's "four table-build segments" (spec §4.4) are quarters of `L`. They are not the walk's segments.
- `cursors` (`:120-134`): `cftab[257]`, the exclusive prefix sums with `cftab[256] == n`, plus per-quarter scatter cursors `cf[4][256]`.
- `build_t` WIDE (`:297-335`): `tt[cf[k][L[i]]++] = i << 8 | L[i]` for the four quarters interleaved, plus the tail. The table maps a row to its successor in text order, and the low byte is the row's F symbol. Being a scatter by exact prefix sums, `tt` is **a permutation of [0,n) for any `L`, hostile included** (`dec_block.c:9-13`), so no walk step needs a bounds check in C. The clang ordering at `:318-328` (load all four cursors, store the four entries, then store the cursors) gave about 15% on repetitive data on Granite Rapids with clang. gcc prefers `cf[k][c]++`. **Use the clang order in Rust** and check that LLVM keeps it.
- FIT layout (`:260-296`, `store3w`/`load3`, `mark_guess_misses`, `build_fsym`, `row_symbol`, `GUESS3`/`SPLIT3`) uses 3-byte entries to fit libbz2's documented memory formula. **Drop it.**

### 1.5 Splitters and the 24-chain walk

- `mark_splitters` (`:357-395`) picks splitter rows `k*n/want` with `want = n/2048` (`BZNG_SEG_ROWS`, `dec.h:40`), capped at `seg_cap-1`, inserts `origPtr`, deduplicates, and sets bit 31 of each splitter's entry. `seg_origin` is the segment that starts at `origPtr`. The lane count is **24 for `n >= 65536`, 8 for `n >= 16384`, 2 if there are at least two splitters, else 1** (`:388-394`).
- `seg_begin` (`:438-456`) records `seg_start[seg]` (pool offset), emits the splitter row's own byte and moves to its successor. `seg_end` (`:459-464`) records `seg_len[seg]` and `seg_next[seg] = seg_index(row)`, a binary search over the sorted splitter list (`:339-351`).
- `lane_step` (`:476-516`):
  1. `e = tt[p]`; if bit 31 is set, end the segment and take the next unstarted one (`w->next_split++`).
  2. Otherwise `sym = (u8)e` and `next = (e>>8) & 0xFFFFF`, with an optional `prefetchr(&tt[next])` straight away.
  3. If the chunk is full, take a new chunk (`new_chunk`, `:420-432`, which links `chunk_next[prev] = c`).
  4. `*out++ = sym` and `p = next`.
- `DEFINE_WALK(K, FIT)` (`:531-572`) walks K lanes in lockstep with `UNROLL_FULL` (`lib_common.h:79-85`, `#pragma clang loop unroll(full)`) until any lane runs out of segments. It then compacts the live lanes and finishes the stragglers round-robin. It is instantiated for K ∈ {1,2,8,24} (`:574-581`). Prefetch is on only when `K >= 16` (`PREFETCH_MIN_LANES`, `:522`). Spec §4.4 records the tunables: 24 chains at ≥ 64 Ki rows, prefetch from 16 chains, four build segments.
- Pool: 1,024-byte chunks (`BZNG_CHUNK`, `dec.h:39`), linked per lane through `chunk_next`. Capacity is `(nmax + 25*1024)/1024 + 1` chunks (`:822`), which is one partly filled chunk of slack per lane. The pool **is `L`'s own memory**: after `build_t`, `tt` holds `L[i]` in its low byte, so `L` is free (`dec_block.c:839-841`, `walk_pool w = {s->L, …}` at `:587`).
- Cycle structure: `tt` is a permutation but need not be one cycle. Rows on cycles with no splitter are never visited. **Exactly periodic blocks (`u^m`) produce `m` cycles**, and the correct text is the origin's cycle repeated. With text `abab`, for example, each cycle has length 2. The reader reproduces this by following `seg_next` from `seg_origin` and wrapping round the cycle until `n` bytes are out (`pool_piece`, `:703-719`). **The port must keep this wrap.** It is required for correct output, not only for libbz2's garbage-in behaviour. The C encoder emits periodic blocks (spec §2.5, §4.2), and so may htslib/libbz2 for a block of a single repeated pattern.

### 1.6 Output: un-RLE1 and CRC

- Text source: `bzng_next_piece` (`dec_block.c:750-802`) hands out the next contiguous pool piece, at most one chunk and at most `limit` bytes. Randomised blocks and small mode go through `minibuf[512]` byte by byte, XOR-ing `rand_mask` (`:732-743`). The mask uses libbz2's `rNums` table (`randtable.c`, 512 entries) and is 1 at text positions where the countdown hits 1.
- `output()` (`decompress.c:102-239`) is resumable at every byte. It carries `pend_len`/`pend_ch`, a run that did not fit, and `ur_b`/`ur_eq`, equal bytes carried over a piece or call boundary. It calls the bulk kernel when `rd_len >= 20 && avail >= 16`. It handles libbz2's "corrupt" case, where the text ends with four equal bytes and no count byte, so the count comes from text position `n` by wrapping (`:215-224`). It reports `BZNG_DISCARD`/`BZ_DATA_ERROR` exactly as libbz2 does. CRC is computed per call over the bytes delivered (`:228-231`).
- Bulk kernel `bzng_unrle_*` (`kern_template.h:1149-1207`). Each step loads `in[0..16)`, `in[1..17)`, `in[2..18)` and `in[3..19)`, stores the first vector to `out` unconditionally, and computes `mask = movemask(eq(v0,v1) & eq(v0,v2) & eq(v0,v3))` (`:1105-1113`). With no candidate it advances 16. Otherwise it advances `ctz(mask) >> VEC_SHIFT`, reads `c = p[0]` and `total = 4 + p[4]`, and fills `total` with 16-byte stores (`:1115-1121`). The `UNRLE_MIN_TEXT` margin (`:1142-1147`, and the comment at `:1084-1100`) exists only so that whole-vector stores past `out` never leave a byte of the caller's buffer modified beyond what has been delivered. The scalar tier uses an 8-byte SWAR `zero_bytes` (`:1123-1139`, `lib_common.h:101`).

### 1.7 What a one-shot decoder can drop, and what replaces it

| C piece | Where | One-shot replacement |
|---|---|---|
| Per-bit resumable header machine (`GET_BITS_CASE`, `s->i/j/t/curr` in state, `BZ_X_*` states) | `decompress.c:261-563` | Straight-line parse with locals over a bit reader on `src`. Keep the selector and code-length fast paths. |
| `BZNG_NEED`, `sync_input` give-back, sticky errors, `total_*`, `strm` checks | `decompress.c:611-741` | None. Truncation is `Err(UnexpectedEof)`. |
| The 160-byte fast-group margin and the whole **exact path** (`bzng_decode_exact`, `bzng_br_fill` in the loop, `group_pos` resume, `bzng_skip_codes` rewind) | `kern_template.h:1010-1064`, `dec.h:333-415` | One group decoder, monomorphised twice. The unchecked one is used while `pos + 160 <= src.len()`. The checked one refills from the tail with zero padding, through a 16-byte stack copy or a byte loop. Track bits consumed and fail if they exceed `8*src.len()` at the end of the block. Decoding zeros past the end always terminates, because groups are bounded by `n_selectors` and the block by `nmax`. On "no code" or a step error, return `Err`; the exact error bit does not matter. `group_no`/`group_pos`/`nblock`/`run`/`run_shift`/`mtf` become locals. |
| Resumable output (`pend_*`, `ur_*`, `rd_*`, `text_left`, `corrupt_pending`, `BZNG_DISCARD`, `minibuf`, `bzng_next_piece` limit logic) | `decompress.c:102-239`, `dec_block.c:697-802` | Gather the segments in text order (following `seg_next` from `seg_origin`, walking each segment's `chunk_next` list, wrapping) into a contiguous `text` scratch buffer of `n` bytes. XOR the rand mask in place if the block is randomised. Then run one un-RLE1 pass `text → out[o..]` with no carried state. A trailing four-equal run with no count byte is `Err`. Then compute `crc(out[o..o2])`. |
| Small mode (2.5-byte `ll16`+`ll4`, pointer reversal, `chain_symbol`, `area2`) | `dec_block.c:606-661`, `:721-729`, `:814-817` | Drop. |
| FIT layout (3-byte entries, guess table aliased over the Huffman tables, `fsym_shift`, the `pf` copies of `tbl` that avoid aliasing reloads) | `dec_block.c:155-245`, `:260-296`, `:476-500` | Drop. Rust's `&mut` noalias solves the aliasing problem the `tbl`/`fsh` copies work around anyway. |
| Per-digit allocation in `alloc_stream`, `bzng_size_stream_arrays`, `place_stream_arrays` | `decompress.c:246-259`, `dec_block.c:810-847` | A per-thread reusable scratch (see §3 and §5). |
| Verbosity output | throughout | Drop. |
| Exact error-position compatibility (21-bit rule, "errors after exactly the bits libbz2 reads") | `dec.h:32`, `decompress.c:29-33` | Not needed. |

What **stays**:

- the bit reader;
- the selector and code-length parse;
- the Huffman table semantics;
- the vector MTF head;
- the quarter histogram and table build;
- splitters, the multi-lane walk and the chunk pool;
- the wrap-around for periodic blocks;
- randomised blocks (rare, but legal and cheap: about 30 lines plus the 512-entry table);
- bulk un-RLE1;
- hardware CRC;
- block and combined CRC checks.

**Can the output go straight into the caller's slice?** Partly, and the part that matters does.

- un-RLE1 writes directly into `out`, so no copy is made after it.
- Check `avail` before every run or vector store. "Output would exceed `out.len()`" is `Err(OutputTooSmall)`.
- Near the end of `out`, fall back to scalar so that no 16-byte store passes `out.len()`.
- Whole-vector stores past the current write position but inside `out` are harmless while the block continues, because later output overwrites them. If the stream then ends *short* of `out.len()`, a few (fewer than 16) bytes past the returned length may hold scratch values. Either document that, or keep the `UNRLE_MIN_TEXT` rule, which is cheap.
- The walk cannot write into `out`, for two reasons. Segments are produced out of text order with unknown offsets. And un-RLE1 expands text non-uniformly: 5 text bytes can become 4 to 259 bytes, so `text` cannot even be staged in place inside `out`. The pool therefore stays for walks with more than one lane.
- A **single-lane walk** (`n < 4096`, roughly) could write `text` contiguously and skip both the pool and the gather. That is a cheap special case worth having for tiny blocks.
- Fusing un-RLE1 with the pool reader, which would carry run state across 1 KB pieces, saves the gather memcpy (about `n` bytes, roughly 1–3 µs at 120 kB). It is a later optimisation if profiles show the gather.

## 2. SIMD kernels against fearless_simd 1.1

`fearless_simd-1.1.0`: the levels are `Fallback`, `Neon`, `Sse2`, `Sse4_2` (`"sse4.2,popcnt,..."`, which implies SSSE3 and SSE4.1; `generated/sse4_2.rs:45`), `Avx2` (`avx2,bmi1,bmi2,lzcnt,movbe,fma,...`) and `Avx512`. Code is written generic over `S: Simd` under `#[simd]` or `dispatch!`, with `#[inline(always)]` helpers. Raw intrinsics are available safely inside `kernel!(fn f(t: Neon|Sse4_2|Avx2, ...))` (`src/kernel_macros.rs`).

Mapping of bz2-ng's ops to the portable API (`generated/simd_trait.rs:619-797`; implementations checked in `generated/sse4_2.rs` and `generated/neon.rs`):

| bz2-ng op | fearless_simd 1.1 | Codegen check |
|---|---|---|
| `vec_ld`/`vec_st` | `u8x16::from_slice(s, &x[i..i+16])` / `store_slice`, or `load_array_ref(&[u8;16])` / `store_array` | `from_slice` does `try_into().unwrap()`. Use `first_chunk::<16>()` / array refs so the length check folds. |
| `vec_dup` | `splat_u8x16` / `u8x16::splat` | ✔ |
| `vec_bcast_lane(v,k)` | `swizzle_dyn_u8x16(v, splat(k))` (SSE: `_mm_shuffle_epi8`, `sse4_2.rs:1490`; NEON: `vqtbl1q_u8`, `neon.rs:1227-1235`) | ✔. Indices are always < 16, so the non-precise form is fine. |
| `vec_shift1(prev,cur)` | `slide_u8x16::<15>(prev, cur)`. "Concatenate `[a,b]`, take 16 from `SHIFT`" gives `[prev[15], cur[0..15]]` (SSE `_mm_alignr_epi8(b,a,15)` via `dyn_alignr_128`, `sse4_2.rs:1460-1474`; NEON `vextq_u8::<15>` via `dyn_vext_128`, `neon.rs:1197`, `:10406`) | ✔. The `match` on the const `SHIFT` folds away. |
| `vec_sel_upto(k,a,b)` | `simd_gt_i8x16(splat((k+1) as i8), IOTA)` then `select_u8x16(mask, a, b)`. Do **not** use `simd_le_u8x16` on SSE, which is `min`+`cmpeq` (2 ops, `sse4_2.rs:1977-1983`). Alternatively take a 16×16 precomputed mask table, but building a `mask8x16` from a vector goes through `from_vector`, whose non-0/−1 lanes are "unspecified", so prefer the compare. `select` is `_mm_blendv_epi8` / `vbslq_u8` (`sse4_2.rs:2057`, `neon.rs:1694`). | ✔ |
| `vec_lane0`, `vec_lane15` | `v[0]`, `v[15]` through `Index`/`Deref` to `[u8;16]` | Should lower to `movd`/`pextrb` and `umov`. Verify in asm, because a spill to the stack would sit on the MTF critical path. |
| `vec_eq`, `vec_and` | `simd_eq_u8x16` gives a `mask8x16`; `&` on masks is `and_mask8x16` | ✔ |
| `vec_mask` (movemask) | `mask8x16::to_bitmask()`. SSE is `_mm_movemask_epi8` ✔ (`sse4_2.rs:2217`). **NEON is AND with weights, `ext`, `zip`, `addv`** (`neon.rs:1785-1799`): about 5 instructions against bz2-ng's single `shrn`. | Use `kernel!(fn …(n: Neon …))` with `vshrn_n_u16::<4>` and `ctz >> 2` in the un-RLE1 candidate step on aarch64, and portable `to_bitmask` elsewhere. |

Every op the decoder needs is portable, and `kernel!` is needed only for the NEON nibble mask. For x86 levels below `Sse4_2` (the `Sse2` level has no `pshufb`, so `swizzle_dyn` is emulated), route to the scalar MTF path as the C does: vector tiers need x86-64-v2. In `dispatch!`, the arms would be `Avx2 | Sse4_2 | Neon` → vector, and `Sse2 | Fallback` → scalar.

**A possible improvement to measure, not in bz2-ng.** MTF-to-front of index `k < 16` is a single permutation of `head`: `head' = swizzle_dyn(head, PERM[k])` with `PERM[k] = [k, 0, 1, …, k-1, k+1, …, 15]`, which is a 256-byte table. That replaces bcast + alignr + cmpgt + blend with one table load (which depends on `sym`, not on `head`) and one `pshufb`/`tbl`. The loop-carried chain through `head` drops from about 3 vector ops to 1. `c = head'[0]`. It is fully portable in `fearless_simd`.

**CRC.** `fearless_simd` has no carry-less multiply and no CRC instructions; `grep` finds only AVX-512 `clmul` mentions in the generated `avx512.rs`, with no public op. The plan is to port the ~250 lines of C with `core::arch`:

- x86_64:
  - `_mm_clmulepi64_si128` (stable 1.27, `target_feature = "pclmulqdq"`) and `_mm_shuffle_epi8` for the byte reverse. Port of `x86/crc32_pclmul.c:29-71`: 4×128-bit accumulators folded by 512 bits.
  - `_mm256_clmulepi64_epi128` (stable 1.89, `vpclmulqdq`). Port of `x86/crc32_vpclmul.c:19-71`.
  - Detect with `is_x86_feature_detected!("pclmulqdq")` / `("vpclmulqdq")`. `fearless_simd`'s `Sse4_2`/`Avx2` tokens do **not** imply PCLMULQDQ; bz2-ng's own tiers require it (`x86/kern_impl.h:22-28`).
  - Use `#[target_feature(enable = "pclmulqdq,sse4.1")] fn` (safe to define under target_feature 1.1, called after detection).
- aarch64:
  - `vmull_p64`/`vmull_high_p64` (stable 1.59, `neon,aes`) and `__crc32d`/`__crc32b` (stable 1.80, `crc`). `__rbit`/`__revll` become `u32::reverse_bits` / `u64::swap_bytes`.
  - Port of `arm/crc32_armv8.c:27-50` and `arm/crc32_pmull.c:51-120`: 8 accumulators, fold by 1,024 bits, used from 256 bytes up.
  - Detect with `is_aarch64_feature_detected!("pmull")` / `("crc")`. Both are always present on Apple and Graviton.
- Fallback: slice-by-16 (`crc32.c:10-34`) with the 16×256 table generated by a `const fn` (about 20 lines) instead of the 880-line `crc32_table.c`.
- The non-reflected trick is reusable as is. Each 16-byte block is byte-reversed, so `clmul` is plain polynomial multiplication. The 128-bit residual is finished as the table CRC of its 16 bytes from a zero register (`x86/crc32_clmul.h:41-47`). Folding constants are at `crc32.h:54-63`.
- Crates checked in `~/.cargo/registry`:
  - `crc32fast` 1.5.x is CRC-32/ISO-HDLC (reflected) only, so it cannot be used without bit-reversing every input byte.
  - `crc` 3.4.0 with `crc-catalog` 2.5.0 has `CRC_32_BZIP2` (`crc-catalog-2.5.0/src/algorithm.rs:2201`) and a `Table<16>` slice-by-16, but is software only. It is a fine oracle in tests and a possible fallback. It runs at roughly 2–4 GB/s, which against 300–1,200 MB/s decode output would cost about 10–20% compared with hardware CRC.
  - `crc-fast` (crates.io) is reported to accelerate all CRC-32 variants with PCLMUL/PMULL, but it is not in the local registry and was not checked. It is a heavier dependency than about 250 lines of our own.
- Sizes: seqair's blocks are 20–120 kB per block. The C thresholds are `pclmul` from 64 B, `vpclmul` from 256 B and `pmull` from 256 B (the `crc32` instructions below that), so all of seqair's blocks take the fast kernels. CRC over the whole block slice of `out` in one call is better than bz2-ng's per-`BZ2_bzDecompress`-call CRC.

**Prefetch.**

- x86_64: `core::arch::x86_64::_mm_prefetch::<_MM_HINT_T0>(p)` is stable and is a safe `fn` in 1.96 (`core_arch/src/x86/sse.rs:1941`). SSE is baseline, and `tt.as_ptr().wrapping_add(next)` avoids any `unsafe`.
- aarch64: `core::arch::aarch64::_prefetch` is unstable (`stdarch_aarch64_prefetch`, #117217), and so is `core::hint::prefetch_read` (`hint_prefetch`, #146941). Use `unsafe { asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly, preserves_flags)) }`, which is sound for any address.
- Relevance: prefetch is only used for walks of at least 16 lanes, which needs `n >= 65,536` text rows. Most of seqair's blocks are below that after RLE1, so this matters little here.

## 3. Bounds checks, `unsafe`, and staying panic-free

Unchecked accesses in the C, with a safe Rust strategy for each:

| C site | Invariant | Rust |
|---|---|---|
| `bzng_br_refill_fast` / `bzng_fast_group` 8-byte BE loads ahead of `p` (`dec.h:273-278`, `:379`) | Group start checks `end - p >= 160`; one group reads at most 125 + 8 bytes | (a) Safe: `src.get(pos..pos+8)` gives one compare per refill (every 2 codes), predictable, est. 1–3%. (b) A small `unsafe` helper `read_unaligned` with `debug_assert!(pos + 8 <= len)`, justified by the group-start margin. Start with (a) and measure. The tail uses the checked monomorphisation. |
| Primary table `prim[buf >> 53]` (`dec.h:316`) | Index < 2048 | `prim: [u16; 2048]` with index `(buf >> 53) as usize`. LLVM proves it in range, so the check is free. |
| Slow-path `perm[first_idx[l] + (v >> (20-l)) - first_code[l]]` (`dec.h:324-326`) | In range for every pattern that reaches it (canonical construction; checked by reasoning about `limit_lj`) | `perm.get(i).ok_or(Error::Data)?`. Slow path only, so the cost is nil. Use `i32` arithmetic, then convert. |
| `s->tab[s->selector[g]]` | Selector < `n_groups` ≤ 6 | `[HTable; 6]` indexed with `sel as usize`, which costs one check per group (nil), or store the selector pre-masked. |
| `SYMBOL_STEP` stores: 16-byte at `L+nb`, the flush loop, `L[nb]=c` (`kern_template.h:927-943`) | `nb + flush + 1 <= nmax` checked just before; `L` has ≥ 16 bytes of slack | **Masked fixed array**: `L: Box<[u8; (1<<20) + 32]>`. Index with `nb & 0xFFFFF` (`nmax ≤ 900,000 < 2^20`), so `&L[i..i+16]` is provably in range and costs one `and`, no branch. Alternatively derive a local `nmax` from `L.len() - 32` and hope LLVM relates them (it often does not). Est. 2–5% if left as checked indexing. |
| `mtf_tail_move` (`kern_template.h:849-863`) | `16 <= nn <= 255` and `m` is 272 bytes | `mtf: [u8; 272]`, `nn` as `u8`. Loads at `j <= 240` and the final `j <= 255`, so `j+16 <= 271`. Use `first_chunk`/array refs; any leftover check is on a less frequent path. |
| `build_t` scatter `tt[cf[k][c]++]` (`dec_block.c:318-333`) | Cursors < n (exact prefix sums) | `tt: Box<[u32; 1<<20]>`, indexed `j & 0xFFFFF`, so no check. `cf: [[u32;256];4]` is indexed by `u8`, so no check. Read `L` through `split_at` into four `q`-length quarters zipped together, so no check. |
| Walk `tt[p]`, `p = (e >> 8) & 0xFFFFF` (`dec_block.c:495-499`) | Permutation of [0,n) | Same fixed `[u32; 1<<20]`. The mask is already in the C. Free. |
| Walk pool store `*ln->out++` (`:506`) | `out < end` checked by the chunk logic | Pool as `[u8; 1<<20]` (capacity `≤ 900,000 + 26*1024 < 2^20`), masked offsets. `chunk_next: [u32; 1024]` and `seg_*: [u32; 512]` (`seg_cap ≤ 441`) masked too. `next_chunk` overflow is impossible (the C `ASSERT` at `:603`); keep a `debug_assert` and a masked index. |
| `seg_index` binary search (`:339-351`) | Row is a splitter | `seg_row[..n_seg].binary_search(&row)`; `Err(_) => return Err(Error::Data)`. Unreachable, but panic-free. |
| un-RLE1 vector loads `in[0..19]` and stores `out[0..16]` (`kern_template.h:1108-1120`) | `plen >= 20`, `avail >= 16` / `>= total+16` | Slices with explicit lengths, at one check per 16 bytes. Negligible. |
| CRC loads | — | `chunks_exact(16/64/128)`. No checks. |

Panic-freedom beyond indexing:

- Fuzzers usually build with debug assertions, which turn overflow and oversized shifts into panics.
  - `run + ((sym+1) << rshift)` is computed for every symbol on the branch-free path, with `rshift <= 21` and `sym <= 257`. Use `wrapping_shl`/`wrapping_add`.
  - `nb + flush` cannot overflow `u32` once `run < 2^22` is known, but use `usize`.
  - The un-RLE1 count is `4 + p[4] <= 259`.
- Header checks to keep, all from `decompress.c`:
  - block-size digit;
  - `origPtr <= 10 + 100000*digit` (`:365-368`) and `origPtr < nblock` (`:514`);
  - `n_in_use > 0` (`:387`);
  - `2 <= n_groups <= 6` (`:394`);
  - `n_selectors >= 1` (`:399`), truncated at 18,002 (`:445`);
  - selector `j < n_groups` (`:416`);
  - code length 1..20 (`:461`);
  - `group_no < n_selectors` (`kern_template.h:1039`);
  - `rshift < 21` and `nb <= nmax` (`:921-924`);
  - EOB required;
  - the "four equal bytes and no count byte" corrupt case → `Err`;
  - block CRC and combined CRC.
- Termination: the symbol loop is bounded by `n_selectors*50`. The walk visits each row at most once per lane segment, and every segment on a cycle with a splitter ends at a marked row. Gather and wrap emit exactly `n` bytes. un-RLE1 is bounded by `out.len()`.
- Memory: no allocation is sized from header fields beyond the fixed scratch, and `out` is the caller's. The 4 MiB fuzzing cap in seqair is not threatened.

**Where safe Rust is likely to cost measurable speed** if the masked-fixed-array approach is not used:

1. `SYMBOL_STEP` stores: 2 checks per symbol on the hottest loop.
2. Input refill: 1 check per 2 codes.
3. `build_t` scatter: a store-bound loop at one check per store.

The walk is latency-bound and absorbs checks. Masking fixes 1 and 3. Item 2 needs either the predictable check or a three-line documented `unsafe` read.

## 4. Size

C that matters for a one-shot decoder (non-blank, non-comment lines, approximate):

| Part | C lines | Notes |
|---|---|---|
| Bit reader, Huffman decode, group decoder (`dec.h`) | ~120 of ~240 | State struct mostly gone |
| Header parse (`decompress.c:287-563`) | ~200 | Without case labels it gets shorter |
| Symbol loop + vector step + MTF tail (`kern_template.h:834-1075`) | ~170 | Exact path dropped |
| un-RLE1 kernel (`kern_template.h:1082-1207`) | ~80 | |
| Output driver (`decompress.c:102-239`) | ~110 | Replaced by gather + one pass + CRC, about 60 Rust lines |
| Block finishing WIDE (`dec_block.c`, without small and FIT) | ~330 | Huffman build, hist, `build_t`, splitters, walk, pool reader |
| `vec.h` | ~120 | Replaced by `fearless_simd` calls |
| CRC: slice16 + dispatch, pclmul, vpclmul, armv8, pmull | ~300 | Plus an 880-line table, replaced by a `const fn` |
| Rand table | ~70 | Data |
| **Total** | **~1,100** (+ table) | |

Estimated Rust:

| Part | Rust lines |
|---|---|
| Bit reader + header | ~220 |
| Huffman | ~120 |
| Symbol loop (vector generic + scalar) | ~220 |
| Table build / splitters / walk / gather | ~300 |
| un-RLE1 (portable + NEON `kernel!`) + randomisation | ~150 |
| CRC (4 kernels + table + dispatch) | ~280 |
| API, errors, per-thread scratch | ~120 |
| **Total** | **≈ 1,400** |

Tests add about 500 lines. These would include differential tests against the `bzip2` crate (libbz2-rs-sys) or C libbz2, every level forced, periodic, randomised and incomplete-code streams, truncation at every byte, and a `cargo fuzz` target.

## 5. Tunables and fixed costs at seqair's sizes

**What `n` is.** The walk and lane count depend on `n`, the BWT length after RLE1, not on the uncompressed size. Runs of four or more equal bytes shrink by up to 51×. In `level-3.cram` (`01-seqair-usage.md` §2) the BA block is 118,988 bytes uncompressed but only 6,373 compressed. It is very repetitive, so its `n` may well fall below 16 Ki.

Lane count at these sizes (`dec_block.c:388-394`):

| `n` (text rows) | Lanes | Prefetch |
|---|---|---|
| ≥ 65,536 | 24 | yes |
| 16,384–65,535 | 8 | no |
| ≥ 2 splitters (about 4 Ki and up) | 2 | no |
| below that | 1 | no |

- Splitters number about `n/2048` (10–60 segments at 20–120 k).
- For 20–120 k rows the table is 80–480 kB, which is resident in L2 on all reference machines. Walk latency is therefore L2 latency (about 15 cycles), not DRAM. That is why prefetch is off and 8 lanes suffice below 64 Ki.
- The spec's measurements of 24 chains, prefetch at ≥ 16 and two vs four build segments (spec §4.4) were taken on 640 kB+ streams and Silesia. Spec §3 also covers 16 kB streams, but only for the never-slower rule.
- **Re-measure on seqair-like blocks:**
  - lanes ∈ {4, 8, 12, 16} for `16 Ki ≤ n < 64 Ki`, and 24 vs 16 for 64–120 Ki;
  - prefetch on/off at 64–120 Ki;
  - the 1-lane direct-to-text special case for `n < 4–8 Ki`;
  - the 2048-row segment size.

Per-block fixed costs and what to do about them:

- **Allocation and page faults dominate at this size if done per call.** The C allocates `100 kB + 5 × block size` per stream by digit. That is 3 MB at `BZh6`, with `calloc`-free `bzalloc`, but still first-touch page faults. A 4 MiB `[u32; 1<<20]` plus 1 MiB `L` plus a 1 MiB pool, allocated fresh per call with `alloc_zeroed`, costs mmap and munmap plus a fault per touched page. At `n = 120 k` that is about 720 kB touched: 180 faults of 4 KiB on x86, 45 of 16 KiB on macOS, at roughly 0.1–0.25 µs each, plus the syscalls. A 120 kB block decodes in about 250–400 µs, so that is a several-percent tax. On 20 kB blocks (about 50 µs) it is much worse.
  - **Fix:** a scratch struct that is reused. Hold it in a `thread_local!` cache behind the `decompress(src, out)` free function, and optionally expose a `Decompressor` that owns it for callers who want control. Fixed power-of-two arrays make reuse trivial, and only the pages actually touched become resident.
  - Never zero the reused scratch per call. Every byte read is written first in each block, provided the `cf`/`h` histograms are reset.
- **Huffman tables:** 6 × 2048 × 2 B = 24 kB of stores per block, plus a perm build. bzip2 picks `n_groups` by the MTF count (2 groups below 200 symbols up to 6 at 2,400 or more), so typical CRAM blocks have 6. Filling 24 kB takes about 1–2 µs, which is noticeable at 20 kB blocks (about 50 µs total). Try K = 10, which halves the fill at the cost of more slow-path decodes, and/or build tables lazily for groups never referenced by a selector. Measure first.
- **Symbol-loop setup:** the `u16 syms[50]` buffer must not be zero-initialised per group in Rust. Hoist it out of the group loop (`[0u16; 50]` once) or use `MaybeUninit`.
- **Histograms:** `quarter_hist` zeroes 4 kB (`h[4][256]`), `cursors` makes a 256-step pass, `cf` is 4 kB. That is fine (sub-µs).
- **CRC:** about 120 kB at 20+ GB/s is about 6 µs with hardware, against about 40 µs for slice-by-16. Hardware CRC matters for latency too.
- **Tail handling:** in the C, the last 160 input bytes of every call go through the exact path. For a 5 kB compressed block that is 3% of the input on the slow path. The padded-tail fast group in the one-shot design removes this entirely.
- **Dispatch:** `Level::new()` is cheap and cached in `std`, but call it once per `decompress`, not per block. Monomorphise the whole block decoder per level, so there is one `dispatch!` per call.

## 6. Risks for matching C performance

1. **Symbol-loop register pressure.** The loop holds `buf`, `cnt`, `pos`, the table base, `syms`, `k`, `L`, `nb`, `nmax`, `eob`, `run`, `rshift`, `head` and the `IOTA` constant. That is tight on x86-64's 16 GPRs. The C notes that gcc 13 spilled, while clang (Apple clang on M4, clang on Graviton) did well (`dec.h:117-124`, `:352-362`; `kern_template.h:866-877`; spec §3: gcc reaches 0.95–1.04 of clang for decode). Rust is LLVM, so the clang-tuned shapes are the right reference. Rust adds slice lengths for checks, which is one more reason to use fixed arrays with masks. Inspect the asm (`cargo asm`) on both aarch64 and x86_64.
2. **gcc-specific code to ignore:**
   - `GROUP_DECODER_INLINE` noinline (`dec.h:358`);
   - `BZNG_CODE_LEN` bzhi (`dec.h:300`);
   - mask-arithmetic `KEEP_*` (`kern_template.h:878-881`);
   - the 4-entry table fill (`dec_block.c:73`);
   - post-increment `build_t` (`dec_block.c:312-316`);
   - `BZNG_SPAN` pointer-difference form (`dec.h:117-129`).
   
   **clang-specific shapes to reproduce:** ternaries that become cmov (`select_unpredictable` if needed), and the separated cursor load/store order in `build_t` (`dec_block.c:299-308`, about 15% on Granite Rapids with clang).
3. **`fearless_simd` inlining.** Every helper must be `#[inline(always)]`, and the whole symbol loop and walk must sit under one `#[simd]`/`dispatch!` entry. Otherwise each vector op becomes a call without the target feature, which collapses performance (`fearless_simd-1.1.0/MANUAL_INLINING.md`). Lane access through `Index` must not round-trip through the stack. Codegen differs by level: on `Sse4_2`, `splat` from a runtime byte is `movd` + `pshufb` with a zero register, while `Avx2` gives `vpbroadcastb`. Same as the C tiers.
4. **No unroll pragma.** `DEFINE_WALK(24, …)` relies on `#pragma clang loop unroll(full)` (`lib_common.h:81`). In Rust, use a const-generic `K` with a `[Lane; K]` and either trust LLVM, which may refuse to unroll a 24× body past its threshold, or generate the 24 calls with a `macro_rules!` repetition. The lane state (24 × {p, out, end, …}) lives in memory either way. What unrolling buys is independent loads in flight. At seqair's sizes the 8-lane walk is the one that matters, and it is easier for LLVM.
5. **Prefetch on aarch64 via `asm!`.** LLVM cannot schedule it. That is probably fine, and it only runs at `n >= 64 Ki`.
6. **NEON movemask.** If the portable `to_bitmask` is used in un-RLE1, it costs about 4 extra instructions per 16 bytes on Apple and Graviton. Use the `kernel!` `shrn` path.
7. **x86 without BMI2 (`Sse4_2` level).** Variable shifts in the bit reader go through `cl`, as in the C SSE4 tier, which is within 1–3% of AVX2 per spec §4.5.
8. **`fearless_simd` maturity.** Version 1.1 is recent. Pin the version and keep a test that runs every level, including `Fallback` via `force_support_fallback` (seqair already does this; see `01-seqair-usage.md`).
9. **LLVM version drift.** bz2-ng's numbers come from specific Apple clang, clang 18+ and gcc 13–16 builds. rustc 1.96 ships its own LLVM. Expect a few-percent swing in either direction that only benchmarking will reveal. The baseline numbers to beat are in `05-baseline-bench.md`. bz2-ng C decodes MC 64 KiB chunks at about 435 MB/s on the M4 Pro, and libbz2-rs-sys at about 168 MB/s.
