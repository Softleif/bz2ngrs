# Test-suite review

A review of every test for weak or tautological oracles, against `AGENTS.md`, the hegel skill and its 12-item review checklist, followed by mutation runs to see which tests catch which defects, and the fixes. Written for the suite as it stood at `d363fd5`, then merged with `port` (e5226bb, the `Decoder` API), whose new tests are in the ledger too. The "Action" column says what changed. The tree has 76 test functions now (72 in the test binaries, 4 doc tests).

## Method

- **Contract.** What the test claims, and where that is written down: the rustdoc of `decompress` (`src/lib.rs`), `Error`, `Position`, `Corruption` (`src/error.rs`), libbz2 1.0.8 behaviour (the `bzip2` crate, libbz2-rs-sys 0.2.5), or the bzip2 format.
- **Tautological?** Does the expected value come from the code under test, or from something that shares its code, constants or assumptions? Three oracles are independent of the crate: the `bzip2` crate (libbz2), a bit-by-bit CRC in `tests/common/forge.rs` (and now in `src/`'s unit tests), and the naive reference decoder in `forge.rs`, which is written from the format and uses no crate code.
- **Checklist.** The `hegel-review` items: 1 narrowed generator, 2 fixed inputs as a property, 3 missing reject direction, 4 several properties in a test, 5 evidence-free property, 6 tolerance hack, 7 weakened oracle, 8 `should_panic` or ignored, 9 pinned config knob, 10 untested variant, 11 silent-triage residue, 12 resource bound in the domain. Items not listed for a test pass. Items 6, 8 (beyond the benchmark) and 11 pass everywhere: there is no tolerance, no `should_panic`, no commented-out assert and no TODO.
- **Mutations.** Temporary edits of `src/` in a throw-away copy of the tree, each run against the suite; see "Mutation runs". Nothing was committed from a mutated tree.

## Ledger

`P` and `N` rows are hegel properties stamped by `surface_properties!` in `tests/common/mod.rs` into `tests/decoder.rs`. Each one loops over `all_decoders()`: `Decoder::new()` and a `Decoder::__with_level` decoder at `Level::fallback()`, the best level and (on x86) the SSE2, SSE4.2 and AVX2 levels. (At `d363fd5` the same properties ran from two files, `decompress.rs` and `decompress_at.rs`, 23 properties each.) `Decoder::run` uses one long-lived decoder per thread and level, so every call after the first in a test thread is a reuse, as in a reader; `run_fresh` makes a new one.

### Properties (`tests/common/props.rs`)

| # | Test | Contract and source | Tautological? | Checklist | Action |
|---|---|---|---|---|---|
| P1 | `roundtrip_arbitrary_bytes` | Data compressed at any level decodes byte for byte into an `out` of its exact size (lib.rs: "returns how many bytes it wrote"; format). Oracle: libbz2's encoder and the data itself. | No. | 1: data capped at 20 000 bytes, hegel's own limit; multi-block streams are P5, P6, P20. | none |
| P2 | `roundtrip_runs` | Same, for runs around the RLE1 boundaries. | No. | 10: the generator names 259 as a boundary, but libbz2's encoder ends a run at 255 bytes (count byte at most 251), so counts 252 to 255 are never produced and a decoder that mishandles only them passes (mutation M08). | Doc comment now says so; counts 252 to 255 are covered by the forged streams (P19) and the new `unrle` property. |
| P3 | `roundtrip_periodic` | Same, for one repeating pattern of 1 to 12 bytes up to 70 000 bytes. | No. | none | none |
| P4 | `roundtrip_tiny_alphabet` | Same, for 1 to 4 distinct bytes. | No. | none | none |
| P5 | `roundtrip_many_blocks` | Same, for 2 to 40 ragged blocks (`BZ_FLUSH` after each piece). | No. | The "one block per piece" premise is asserted in P13 and P14 but not here; it only affects what the test reaches, not its verdict. | none |
| P6 | `roundtrip_block_boundary` | Same, for data sized to within 300 bytes of a block's capacity, a run across the boundary, a tail (format: a block holds 100 000 x level - 19 bytes). | No. | 9: count pinned to 60 for speed (`pinned::cases`, overridable by `BZ2NGRS_PINNED_CASES`); that is a budget, not a narrowing. | none |
| P7 | `roundtrip_expanding` | Same, for one byte repeated 2^e plus or minus 2 times, e up to 22 (4 MB). | No. | none | none |
| P8 | `larger_out_gives_same_prefix` | A longer `out` still gives `Ok(len)`, the same bytes, and leaves `out[len..]` untouched (lib.rs: "bytes of `out` past the returned length are untouched"). | No. | 1: the slack was drawn uniformly from 1 to 70 000, so 1 to 288 bytes (the range in which the SIMD run fills overshoot and the bulk loops fall back to the tail loop) came up in under 1% of cases. | Slack is now 1 to 300 half the time. |
| P9 | `short_out_is_output_too_small_with_capacity` | An `out` shorter by k >= 1 gives exactly `Err(OutputTooSmall { capacity: out.len() })` (error.rs). | No. | 1: k was uniform over 1 to len, so a miss by one byte was rare for big data. | k is now 1 to 8 half the time. |
| P10 | `trailing_bytes_are_ignored` | Bytes after the end-of-stream marker (noise, a whole second stream, a header) change nothing (lib.rs). | No. | none | none |
| P11 | `mutated_stream_agrees_with_reference` | Accept and reject exactly as libbz2 (lib.rs, AGENTS.md); 1 to 3 mutations of a valid stream (bit flips, byte sets, truncation, insert, delete, splice, digit, CRC bits), output cap sometimes tiny. | No (libbz2 is the oracle). | 3 and 7: for an error it only asserted `is_err`, so a stream libbz2 calls truncated could come back as `Corrupt`, and `OutputTooSmall` could be returned for anything. | `check_against_reference` now compares classes (see below). |
| P12 | `truncated_stream_is_unexpected_eof_in_its_block` | A proper prefix is `UnexpectedEof { at }` with `at.bit == prefix.len() * 8` and `at.block` the block the cut falls in (error.rs, `Position`). The block is worked out from the bit offsets of the block magics, found by scanning the stream. | No: the expected position comes from the stream's layout, not from the decoder. | none | none |
| P13 | `corrupt_block_crc_is_block_crc_with_values` | Overwriting the stored CRC of block k gives `BlockCrc { block: k, stored: what was written, computed: the CRC of the piece }`. `computed` is the bit-by-bit CRC in `forge.rs`, and the premise that the encoder stored the same value is asserted. | No. | none | none |
| P14 | `corrupt_stream_crc_is_stream_crc_with_values` | Overwriting the combined CRC gives `StreamCrc { stored, computed }` with `computed` equal to the blocks' CRCs combined by rotate-and-xor (asserted against the encoder's own value). | No. | none | none |
| P15 | `wrong_header_is_not_bzip2` | A first four bytes that are not `BZh1` to `BZh9` are `NotBzip2` once the wrong byte is in, wherever the input is cut after it (error.rs). | No. | 10: input that ends inside the four bytes (documented as `UnexpectedEof`) is reached by P12 for short prefixes and by the unit test `bad_headers`, not here. | none |
| P16 | `hostile_arbitrary_bytes_agree_with_reference` | Differential against libbz2 on up to 4 KB of arbitrary bytes. | No. | 3, 7: as P11. | as P11 |
| P17 | `hostile_bzh_header_agrees_with_reference` | The same behind `BZh1` to `BZh9`. | No. | as P16 | as P11 |
| P18 | `hostile_block_header_agrees_with_reference` | The same behind a header and the block magic, so the block-header parser is reached. | No. | as P16 | as P11 |
| P19 | `forged_block_agrees_with_reference` | One forged block (valid framing and CRCs around an arbitrary BWT column and origPtr): the naive reference decoder, libbz2 and the decoder all agree, bytes or rejection. | No: the writer and the reference decoder use no crate code; libbz2 is a third opinion and the property fails if the first two disagree. | 10: every Huffman table used one code length for all its symbols, so the table builder was never fed mixed lengths from a forged stream; 10: no selectors beyond what the symbols need, and libbz2 1.0.8 reads and ignores up to 32 767. | `Recipe` gained `varied` (canonical codes over the leaves of a random tree, complete or not) and `extra_selectors`. |
| P20 | `forged_multi_block_stream_agrees_with_reference` | The same for 2 to 4 blocks of up to 70 000 rows. | No. | as P19 | as P19 |
| P21 | `forged_origptr_out_of_range_is_corrupt_orig_ptr` | A last block whose origPtr is not below its rows (or beyond the digit's limit) is `Corrupt { OrigPtr }` in that block, earlier blocks valid. | No. | 3: `at.bit` was never looked at. | `at.bit` must lie from the block's first bit to the end of the input (`Position` says only "at or just after the offending field"). |
| P22 | `forged_oversize_block_is_corrupt_block_too_long` | A last block of 1 to 3 rows more than the digit allows is `Corrupt { BlockTooLong }` in that block. | No. | 3: as P21. | as P21 |
| P23 | `failed_decode_leaves_no_state` | A failed decode does not make the next, valid one fail or differ (lib.rs: per-thread scratch is reused). | No. | Weak against stale-read bugs only when the failed stream gets as far as dirtying the scratch; mutated streams usually do. | none |

### Properties from `port` (the `Decoder` API)

| # | Test | Contract and source | Tautological? | Checklist | Action |
|---|---|---|---|---|---|
| P24 | `reuse_independent_of_earlier_calls` | "A result never depends on earlier calls: a `Decoder` that has decoded other streams, or failed on them, gives the same answer as a new one" (`Decoder` rustdoc). A sequence of 2 to 6 steps (valid, mutated, truncated, hostile, a 150 kB to 900 kB block) on the reused decoder; each result must equal a fresh decoder's, bytes included, and satisfy the differential rule against libbz2. | No: the fresh decoder is the same code, but the contract is exactly that comparison, and libbz2 is the second opinion that keeps both from being wrong the same way. | 9: count pinned to 100 (the sequences decode up to 5 MB). 10: sequences of at most 6 calls; leftover state that needs a longer history is not reached. | The oracle call now passes `src` (the class rule needs it). |
| P25 | `to_vec_equals_oracle_with_actual_length` | `decompress_to_vec(src, size)` with `size` at or above the stream's length is the data, cut to its length, in a `Vec` of capacity `size` (rustdoc: "The `Vec` keeps capacity `size` even when shorter"). | No: compared with the data that was compressed, and libbz2 decodes it to the same (premise). | 7: only bytes and length were compared; the documented capacity was not. | Asserts the capacity; compares with the original data. |
| P26 | `to_vec_below_actual_length_is_output_too_small` | A `size` below the length is `OutputTooSmall { capacity: size }`. | No. | 1: k was uniform, so a miss by one byte was rare. | k is now 1 to 8 half the time. |
| P27 | `to_vec_mutated_agrees_with_reference` | Differential rule for `decompress_to_vec` on mutated streams. | No. | 7: the `Vec` is passed as `out`, so "nothing written past the length" holds trivially; the length is the only thing it adds. | Class-aware through `check_outcome`. |
| A1 | `allocation::decoder_allocates_on_first_decode_only` | `Decoder::new` and `default` allocate nothing, the first decode allocates, the second and a failed one do not; `Decoder: Send` (rustdoc). A counting global allocator. | No. | 2: one stream, by design, and it counts allocations on one thread only (one test function, because the counter is global). It does not cover `decompress_to_vec`'s own allocation of `size` bytes. | none |

### Properties added by this review

| # | Test | Contract and source | Tautological? | Notes |
|---|---|---|---|---|
| N1 | `forged_bad_block_magic_is_corrupt_block_magic` | A 48-bit value that is neither magic where a block starts is `Corrupt { BlockMagic }` in that block (error.rs). | No: the stream is written from the format; libbz2 must reject it (premise); the bit range comes from the writer. | Magic is the block magic or the end magic with a bit flipped, or any value. |
| N2 | `forged_empty_symbol_map_is_corrupt_no_symbols` | An empty symbol map is `Corrupt { NoSymbols }`. | No. | |
| N3 | `forged_table_count_out_of_range_is_corrupt_group_count` | A table count of 0, 1 or 7 is `Corrupt { GroupCount }`. | No. | |
| N4 | `forged_zero_selectors_is_corrupt_selector_count` | A selector count of 0 is `Corrupt { SelectorCount }`. | No. | |
| N5 | `forged_selector_beyond_tables_is_corrupt_selector` | A selector's unary code with `tables` or more ones is `Corrupt { Selector }`, also among the selectors past 18 002. | No. | |
| N6 | `forged_code_length_out_of_range_is_corrupt_code_length` | A first code length of 0 or 21 to 31, or a step from 1 to 0 or from 20 to 21, is `Corrupt { CodeLength }`. | No. | |
| N7 | `forged_unassigned_code_is_corrupt_huffman_code` | A symbol written with a bit pattern that no code of its table has is `Corrupt { HuffmanCode }`. | No. | Flat tables longer than needed leave patterns unassigned. |
| N8 | `forged_missing_end_of_block_is_corrupt_no_end_of_block` | A last group of 50 symbols with no end-of-block symbol is `Corrupt { NoEndOfBlock }`. | No. | Blocks with a single distinct byte cannot be padded; the property discards them (`applied`). |
| N9 | `forged_run_length_boundary_is_run_too_long_from_22_symbols` | A run of 19 to 30 RUNB symbols: from 22 symbols (2^21 or more, error.rs) it is `RunTooLong`, before that `BlockTooLong`. | No: the boundary is the documented "2^21 or more". | |
| N10 | `forged_excess_selectors_are_ignored` | A block with more than 18 002 selectors decodes like the same block without the extra ones (libbz2 1.0.8, `decode.rs` `MAX_SELECTORS`). | No: the naive reference ignores the extras, libbz2 is the premise. | Accept-direction twin of the header defects; M18 is only caught here. |
| N11 | `forged_defect_cut_off_agrees_with_reference` | A defective stream cut at a random byte, around the defect or anywhere from the bad block on, gets libbz2's error class (early end or bad input), `UnexpectedEof` with `bit == len * 8` or a data error. | No. | Reaches the "21 bits in" boundary of a bad Huffman code (M19). |

| N12 | `forged_largest_block_agrees_with_reference` | A block of 899 950 to 900 000 rows at digit 9, every row a literal (`Shape::Cycle`), so it needs 18 000 or 18 001 selectors: the same three-way agreement as P19. | No. | The only test that fails when `MAX_SELECTORS` is 18 000 (M18); the encoder never writes a block that big. |

Each of N1 to N8 is a test of its own (checklist 4: one rule per failure signal), generated by `header_defect_properties!`.

### Oracle: `check_against_reference` (`tests/common/oracle.rs`, used by P11, P16 to P18, N11 and the forged properties)

The differential rule asserted that ours is `Ok` with the same bytes when libbz2 is, and otherwise that ours is *some* `Err` (and, for `Ok`, that nothing past the length changed). "Some error" holds for any implementation that rejects. It now also requires the class to match:

- libbz2 reports an early end: ours is `UnexpectedEof` and `at.bit` is the input's length in bits.
- libbz2 reports bad input: ours is `NotBzip2`, `Corrupt`, `BlockCrc` or `StreamCrc` (or `OutputTooSmall` if libbz2 rejects the whole stream anyway, the case where the output passes `cap` before the CRC is checked), and a `Corrupt` position is not past the input.
- libbz2 passes `cap` bytes: ours is `OutputTooSmall { capacity: cap }`, unless libbz2 rejects the stream as a whole, when ours may report the corruption (the documented "a corrupt stream may report its corruption first").

The last two lean on a second libbz2 run with no cap (`reference_outcome`). `check_outcome` (from `port`, shared by P24 and P27) takes `src` for this. No decoder disagreement came out of the stricter rule.

### Scale, corpus (`tests/scale.rs`, `tests/corpus.rs`)

| Test | Contract and source | Tautological? | Checklist | Action |
|---|---|---|---|---|
| `thirty_megabytes_of_mixed_data_at_level_9` | A 30 MB multi-block stream decodes exactly at every level. | No. | 2: it is one fixed input, by design (the scale probe); its premise assertion counted blocks in the first megabyte only, so it said nothing about the stream being multi-block. | Premise is now >= 30 blocks in the whole stream. |
| `fifty_megabytes_of_zeros_into_an_exact_out` | 50 MB of zeros (about 3 000 blocks' worth of RLE) decode into an exact `out`. | No. | 2 (by design) | none |
| `fifty_megabytes_of_zeros_into_an_out_one_byte_short` | The same into `out` one byte short is `OutputTooSmall`, by kind only. | No. | 7: the capacity is not compared. | Kind-only is the helper's contract; P9 checks the value. none |
| `hundreds_of_tiny_blocks` | 500 pieces at levels 1 and 9 give at least 400 blocks and decode exactly. | No. | 2 (by design) | none |
| `real_cram_blocks_decode_like_the_reference` | Every real CRAM bzip2 block of the corpus decodes to its manifest length and to libbz2's bytes, at every level. | No. | 2: fixed inputs by design. 8-like: with no corpus it prints a note and passes, so a CI without the corpus checks nothing. | `BZ2NGRS_CORPUS_REQUIRED=1` turns a missing corpus into a failure. |

### Unit tests in `src/` (`src/tests.rs` and per-module)

| Test | Contract and source | Tautological? | Checklist | Action |
|---|---|---|---|---|
| `tests::edge_inputs` | Round trip, exact fit, one byte short, past-the-end untouched, at every level, for 9 fixed inputs at levels 1, 6, 9. | No. | 2: fixed inputs; not a property, but a labelled regression sweep and every input is also in a generator. | none |
| `tests::runs_and_text` | Same for runs of every length 0 to 299 between literals, noise, text. | No. | 2 | none |
| `tests::sizes_across_lane_counts` | Same for 13 sizes around the BWT lane thresholds (2 047 to 899 000). | No. | 2; the thresholds are also reached by the forged generators. | none |
| `tests::multi_block` | Same for 350 kB at level 1 and 2 MB of zeros. | No. | 2 | none |
| `tests::trailing_bytes_are_ignored` | Garbage after the stream is ignored. | No. | 7: only the returned length was checked. | Bytes are compared too. |
| `tests::truncation_at_every_byte` | Every proper prefix of one stream is `UnexpectedEof` with `bit == cut * 8`. | No. | 2 (an exhaustive sweep of one stream); the block index is checked by P12. | none |
| `tests::corruptions_match_libbz2` | Flipping bit 0, 3 or 7 of every byte of one stream: same verdict and bytes as libbz2. | No. | 7: compared `is_ok` only. | Compares the class too; allows `OutputTooSmall` where libbz2 rejects (the corrupt block grows past `out`). |
| `tests::bad_headers` | `BZh0` and `BZx9` are `NotBzip2`; `BZh9` and `` are `UnexpectedEof` at bit 32 and 0; a flipped combined CRC is `StreamCrc`. | No. | 7: `StreamCrc { .. }`, values not compared. | The expected `stored: 1, computed: 0` are now asserted. |
| `tests::randomised_block` | A block with the randomised bit set decodes to what libbz2 gives (CRCs fixed up). | **Yes, in part**: the CRC written into the stream was `crate::crc::update` of the output, so the decoder's CRC check compared the crate's CRC with itself. libbz2 (`assert_eq!(reference(&c), Ok(y))`) was a saving check, but the test should not depend on it. | 2 | CRC is now a local bit-by-bit one. |
| `tests::display_carries_the_context` | `Display` of four errors. | No. | 10: `NotBzip2`, `StreamCrc` and the `Corruption` messages were not covered. | `NotBzip2` and `StreamCrc` added; the `Corruption` kinds are checked through `Display` only for `Selector` (the strings are private and one-line each). |
| `bits::tests::reads_msb_first_and_pads_with_zeros`, `long_input_crosses_refills` | Reader semantics on two fixed inputs. | No. | 2 | none |
| `bits::tests::reads_follow_the_bit_string` (new) | Any reads of 1 to 32 bits over any bytes give the bits of the bytes, MSB first, zeros past the end; `consumed` and `overrun_by` agree with the bit count. | No: the model is indexing a bit string. | | |
| `bits::tests::reset_replays_the_reads` (new) | `reset(mark)` replays the same reads. | No. | | |
| `huffman::tests::canonical_codes`, `long_codes_and_incomplete_sets`, `oversubscribed_set_truncates` | Hand-derived canonical codes, a 1-bit code beside 12 to 20 bit codes, an oversubscribed set (libbz2's table semantics, in the module doc). | No: the expected codes are worked out by hand. | 2: three fixed sets; the table's long-code and oversubscribed paths have no other direct test (the encoder only produces complete codes to 17 bits). | `decodes_like_libbz2` added. |
| `huffman::tests::decodes_like_libbz2` (new) | For lengths 1 to 20 over 3 to 258 symbols, complete, incomplete or oversubscribed, at every level, `decode` of any bit pattern is the symbol and length of libbz2's limit/base/perm tables, or no code. | Shares libbz2's semantics with the decoder by design (the contract is "exactly libbz2"); the oracle is a transcription of `BZ2_hbCreateDecodeTables` and `GET_MTF_VAL`, not the crate's table layout. A second model check asserts that it decodes each symbol's canonical code to that symbol. | | |
| `crc::tests::check_value` | CRC of `123456789` is `0xFC891918` (CRC catalogue) for `update` and every kernel. | No: catalogue value. | 2: one input. | none |
| `crc::tests::all_lengths_and_alignments` | Every kernel, every length 0 to 300 at 16 alignments and 3 initial values, against a reference. | **Yes, in part**: the reference was `bytewise`, built on the same `TABLE[0]` as `slice16` and the kernels' tails. A wrong table would be wrong in both. | | Reference is a bit-by-bit CRC from the polynomial. |
| `crc::tests::matches_reference` | The same for any bytes up to 3 000 and any initial value. | **Yes, in part**: as above. | 1: data capped at 3 000 bytes. | as above; `a_megabyte_at_an_odd_offset` covers the long-buffer loops |
| `crc::tests::chaining` | `f(f(init, a), b) == f(init, a ++ b)` for every kernel. | No: a law of CRC, not tied to a table. | | none |
| `crc::tests::a_megabyte_at_an_odd_offset` (new) | Every kernel on 1 MiB + 13 bytes at offset 5 equals the bit-by-bit CRC. | No. | | |
| `crc::tests::speed` | Benchmark, `#[ignore]`d, run by hand. | n/a | 8: ignored, but it asserts nothing, so no bug can hide in it. | none |
| `decode::tests::perm_moves_lane_to_front_and_slide_shifts_in` | `PERM[k]` as a shuffle moves lane k to the front; `slide::<15>` shifts a byte in. | No: expected value by `Vec::remove` and `insert`. | 2 and 10: it checked `PERM[5]` at the best level only. | All 16 k at every level. |
| `decode::tests::tail_move_is_move_to_front` | The vector tail move equals a plain move-to-front for every index 16 to 255, slack untouched. | No. | 10: the fallback and best level only. | Every level. |
| `unrle::tests::unrle_matches_the_byte_by_byte_decoder` (new) | `unrle` equals libbz2's byte-at-a-time un-RLE1 on any text (complete runs, short runs, noise, four equal bytes with or without a count), at every level, into a buffer at any offset that is exactly sized, roomier or too small; writes nothing outside the output on success; `RunCount` when the bytes fit, `OutputFull` when not. | No: the model is the five-line loop of the format. | | |

### Doc tests

| Test | Contract | Tautological? | Checklist | Action |
|---|---|---|---|---|
| `src/lib.rs` `decompress` example | Round trip of 23 bytes. | No (compares to the input). | 2 (an example) | none |
| `src/error.rs` `Error` example | `OutputTooSmall { capacity: 4 }` for a short buffer; for `b"this is not bzip2"` "bad input". | No for the first half. **The second half holds for any implementation**: its `match` has an arm that prints for `Err(bad_input)` and for `Ok`, and asserts nothing. | 7 | Not changed: a doc comment is not test code, and this review edits only test code. Suggest `assert!(matches!(decompress(...), Err(Error::NotBzip2)))`; P15 and `bad_headers` cover the claim meanwhile. |
| `README.md` usage example | Round trip of 16 000 bytes. | No. | 2 | none |

## Mutation runs

Each row is a one-line edit of `src/` made in a scratch copy (never in this tree), run against the suite before and after this review. "Before" is the suite at `d363fd5`; "after" is this branch. "forged" is the group of `forged_*` properties; "unit" is `cargo test --lib`; "corpus" is `tests/corpus.rs` with the 4 403 real CRAM blocks.

| # | Stage | Mutation | Before: tests that failed | After: tests that failed |
|---|---|---|---|---|
| M01 | bit reader | overrun test `>` becomes `>=`-like (`consumed + extra + 1 > len`) | 22: `bad_headers`, `corrupt_stream_crc_is_stream_crc_with_values`, `edge_inputs`, `failed_decode_leaves_no_state` +18 | 31: `bad_headers`, `corrupt_stream_crc_is_stream_crc_with_values`, `edge_inputs`, `failed_decode_leaves_no_state` +27 |
| M02 | Huffman | long-code decode starts at length 13 instead of 12 | 10: `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` +6 | 23: `decodes_like_libbz2`, `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length` +19 |
| M03 | Huffman | selector array indexed `g % 4096` (blocks over 204 800 symbols) | 6: `forged_block_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long`, `real_cram_blocks_decode_like_the_reference` +2 | 8: `forged_block_agrees_with_reference`, `forged_largest_block_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` +4 |
| M04 | Huffman | selector move-to-front shifts one entry only | 15: `corruptions_match_libbz2`, `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr` +11 | 27: `corruptions_match_libbz2`, `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length` +23 |
| M05 | MTF | tail move index masked `& 127` | 9: `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` +5 | 21: `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_defect_cut_off_agrees_with_reference` +17 |
| M06 | BWT | splitter for `origPtr` dropped when it is in the last segment | 22: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `edge_inputs`, `failed_decode_leaves_no_state` +18 | 37: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `edge_inputs`, `failed_decode_leaves_no_state` +33 |
| M07 | BWT | remainder rows (n mod 4) not counted | 15: `corrupt_block_crc_is_block_crc_with_values`, `corruptions_match_libbz2`, `edge_inputs`, `forged_block_agrees_with_reference` +11 | 12: `corruptions_match_libbz2`, `edge_inputs`, `forged_defect_cut_off_agrees_with_reference`, `forged_missing_end_of_block_is_corrupt_no_end_of_block` +8 |
| M08 | un-RLE1 | run of 4 + count 255 written as 258 bytes | 4: `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` | 17: `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_defect_cut_off_agrees_with_reference` +13 |
| M09 | un-RLE1 | four equal bytes at the block end accepted with count 0 | **nobody** | 4: `forged_block_agrees_with_reference`, `forged_defect_cut_off_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `unrle_matches_the_byte_by_byte_decoder` |
| M10 | un-RLE1 | derandomisation off by one row | 5: `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` +1 | 17: `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_defect_cut_off_agrees_with_reference` +13 |
| M11 | CRC | stream CRC not compared | 4: `bad_headers`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `mutated_stream_agrees_with_reference` | 6: `bad_headers`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `mutated_stream_agrees_with_reference` +2 |
| M12 | CRC | block CRC not compared | 3: `corrupt_block_crc_is_block_crc_with_values`, `corruptions_match_libbz2`, `mutated_stream_agrees_with_reference` | 5: `corrupt_block_crc_is_block_crc_with_values`, `corruptions_match_libbz2`, `mutated_stream_agrees_with_reference`, `reuse_independent_of_earlier_calls` +1 |
| M13 | CRC | stream CRC combined without the rotate | 9: `corrupt_stream_crc_is_stream_crc_with_values`, `failed_decode_leaves_no_state`, `forged_multi_block_stream_agrees_with_reference`, `larger_out_gives_same_prefix` +5 | 12: `corrupt_stream_crc_is_stream_crc_with_values`, `failed_decode_leaves_no_state`, `forged_multi_block_stream_agrees_with_reference`, `larger_out_gives_same_prefix` +8 |
| M14 | error context | block index sticks at 1 | 4: `corrupt_block_crc_is_block_crc_with_values`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long`, `truncated_stream_is_unexpected_eof_in_its_block` | 13: `corrupt_block_crc_is_block_crc_with_values`, `forged_bad_block_magic_is_corrupt_block_magic`, `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_empty_symbol_map_is_corrupt_no_symbols` +9 |
| M15 | error context | early end reports bits consumed, not the input length | 3: `bad_headers`, `truncated_stream_is_unexpected_eof_in_its_block`, `truncation_at_every_byte` | 10: `bad_headers`, `forged_defect_cut_off_agrees_with_reference`, `hostile_arbitrary_bytes_agree_with_reference`, `hostile_block_header_agrees_with_reference` +6 |
| M16 | error context | `Corrupt` position bit is 0 | **nobody** | 11: `forged_bad_block_magic_is_corrupt_block_magic`, `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_empty_symbol_map_is_corrupt_no_symbols`, `forged_missing_end_of_block_is_corrupt_no_end_of_block` +7 |
| M17 | error context | `BlockCrc` stored and computed swapped | 1: `corrupt_block_crc_is_block_crc_with_values` | 1: `corrupt_block_crc_is_block_crc_with_values` |
| M18 | Huffman | `MAX_SELECTORS` 18000 instead of 18002 | **nobody** | 2: `forged_largest_block_agrees_with_reference`, `forged_oversize_block_is_corrupt_block_too_long` |
| M19 | Huffman | bad code is "early end" within 20 bits instead of 21 | **nobody** | 1: `forged_defect_cut_off_agrees_with_reference` |
| M20 | format rule | seven Huffman tables accepted | **nobody** | 5: `forged_defect_cut_off_agrees_with_reference`, `forged_table_count_out_of_range_is_corrupt_group_count`, `hostile_block_header_agrees_with_reference`, `mutated_stream_agrees_with_reference` +1 |
| M21 | format rule | code length 21 accepted | **nobody** | 2: `forged_code_length_out_of_range_is_corrupt_code_length`, `forged_defect_cut_off_agrees_with_reference` |
| M22 | format rule | run of 2^21 allowed to 2^22 | **nobody** | 1: `forged_run_length_boundary_is_run_too_long_from_22_symbols` |
| M23 | format rule | `origPtr == n` accepted | 2: `forged_block_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr` | 1: `forged_origptr_out_of_range_is_corrupt_orig_ptr` |
| M24 | format rule | zero selectors accepted | **nobody** | 2: `forged_defect_cut_off_agrees_with_reference`, `forged_zero_selectors_is_corrupt_selector_count` |
| M25 | format rule | selector with `tables` ones accepted | **nobody** | 4: `forged_defect_cut_off_agrees_with_reference`, `forged_selector_beyond_tables_is_corrupt_selector`, `mutated_stream_agrees_with_reference`, `to_vec_mutated_agrees_with_reference` |
| M26 | CRC | block CRC computed over all but the last byte | 27: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `edge_inputs` +23 | 43: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `edge_inputs` +39 |
| M27 | CRC | PMULL fold constant `K_1024` wrong in the last digit | 22: `all_lengths_and_alignments`, `corruptions_match_libbz2`, `edge_inputs`, `failed_decode_leaves_no_state` +18 | 38: `a_megabyte_at_an_odd_offset`, `all_lengths_and_alignments`, `corruptions_match_libbz2`, `edge_inputs` +34 |
| M28 | CRC | ARMv8 CRC kernel tail bytes not bit-reversed | 28: `all_lengths_and_alignments`, `chaining`, `check_value`, `corrupt_block_crc_is_block_crc_with_values` +24 | 45: `a_megabyte_at_an_odd_offset`, `all_lengths_and_alignments`, `chaining`, `check_value` +41 |
| M29 | Huffman | long-code limit compared `<=` instead of `<` | 6: `forged_block_agrees_with_reference`, `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `forged_oversize_block_is_corrupt_block_too_long` +2 | 20: `decodes_like_libbz2`, `forged_bad_block_magic_is_corrupt_block_magic`, `forged_block_agrees_with_reference`, `forged_code_length_out_of_range_is_corrupt_code_length` +16 |
| M30 | un-RLE1 | wide run fill stops at 32 bytes for a run of 33 | 5: `forged_multi_block_stream_agrees_with_reference`, `forged_origptr_out_of_range_is_corrupt_orig_ptr`, `real_cram_blocks_decode_like_the_reference`, `roundtrip_block_boundary` +1 | 11: `forged_bad_block_magic_is_corrupt_block_magic`, `forged_excess_selectors_are_ignored`, `forged_missing_end_of_block_is_corrupt_no_end_of_block`, `forged_multi_block_stream_agrees_with_reference` +7 |
| M31 | error context | early end reports block 0 | 1: `truncated_stream_is_unexpected_eof_in_its_block` | 1: `truncated_stream_is_unexpected_eof_in_its_block` |
| M32 | state | code-length table built over one stale entry from the previous block | 27: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `edge_inputs` +23 | 43: `corrupt_block_crc_is_block_crc_with_values`, `corrupt_stream_crc_is_stream_crc_with_values`, `corruptions_match_libbz2`, `edge_inputs` +39 |

Notes on the table:

- "Before" is the suite at `d363fd5`, "after" the merged tree. Lists show the number of distinct test names that failed in either `decompress.rs`/`decompress_at.rs` (before) or `decoder.rs` (after), `--lib` and `corpus`, then the first four.
- **M07** (before) hangs the integration tests, so only the unit tests and the corpus count; the run was killed after 20 CPU minutes. It is caught either way.
- **M09, M16, M18, M19, M20, M21, M22, M24, M25** were not caught by the old suite at all, and each is now caught by a test that did not exist. They are the reject direction (what libbz2 refuses) and the error context.
- **M09 survived the first version of the new tests as well.** The forge gave a block that ends in four equal bytes the CRC of what libbz2 writes there (the four bytes and a count read from past the end), so a decoder that takes the missing count to be 0 computed a different CRC and still failed, with `BlockCrc`, and the differential rule saw an error. Half the time the forge now writes the CRC of the four bytes alone (`naive_block_with`), so such a decoder sees a matching CRC and must be stopped by the run-count rule itself.
- **M08** (a count of 255) is not caught by any round trip: libbz2's encoder ends a run at 255 bytes, so a count byte above 251 never occurs in its output. Only forged streams and the `unrle` property reach it.
- **M18** needs a block of 18 001 selectors; the largest block libbz2's encoder can write has 18 000 or fewer. `forged_largest_block_agrees_with_reference` builds one.
- **M19** and **M22** are caught by one test each, written for them (`forged_defect_cut_off_agrees_with_reference`, `forged_run_length_boundary_is_run_too_long_from_22_symbols`). **M17** and **M31** by one test each that existed.
- **M23** (before) is caught by `forged_block_agrees_with_reference` and `forged_origptr_out_of_range_is_corrupt_orig_ptr`; after, by the second only. `forged_block_agrees_with_reference` draws an `origPtr` equal to the row count only rarely.
- **M27 and M28** change the CRC kernels used on aarch64 (`K_1024` of the PMULL kernel, the tail of the ARMv8 CRC kernel). The x86 kernels were not mutated: they cannot run on this machine.
- **M32** is meant to be a state leak between blocks (a stale code length in the table build). It is caught by most tests, because a stale length is also wrong in the first block of every stream on a fresh decoder; it says nothing about `reuse_independent_of_earlier_calls`, which no mutation here isolates.
- Every mutation was made in a scratch copy and reverted there; `git diff src/` is empty in this tree at every commit.

## Fixes, in the order of the problems they close

1. **Error kinds were never compared in the reject direction.** `check_against_reference` accepted any `Err`. It now compares libbz2's class. Closes part of M19, M31.
2. **No test pinned the `Corruption` kind or the block of any header rule.** Only `OrigPtr` and `BlockTooLong` were checked by value. The forge now writes a block with one defect (`Defect`) and N1 to N9 assert the kind, the block index and a bit range for each of the ten header rules.
3. **Forged streams used one code length per table.** `varied` writes canonical Huffman codes of mixed lengths, so the decoder's long-code path is reached beside the primary table, in an incomplete or complete code.
4. **The 18 002-selector quirk of libbz2 1.0.8 had no test.** `extra_selectors` (and N10) cover it.
5. **Test CRCs that called `src/crc.rs` or shared its table.** `crc` tests and `randomised_block` now use a bit-by-bit CRC.
6. **A direct oracle for the table and the un-RLE1 stage.** `decodes_like_libbz2`, `unrle_matches_the_byte_by_byte_decoder`.
7. **Variant coverage in the SIMD unit tests**: all MTF permutations at all levels.
8. **Slack distributions** in P8, P9: small slack and small shortfalls drawn on purpose.

9. **A run of four equal bytes at a block's end** (M09): the forged CRC is now sometimes that of the bytes before the missing count.
10. **The 18 001-selector block** (M18): `Shape::Cycle` and `largest_block`.
11. **Merge with `port`**: `check_outcome` takes `src`; `to_vec` properties assert the documented capacity and use small shortfalls; the scale probe's block-count premise was wrong (the data's runs shrink in the first run-length stage, 27 blocks, not 33) and now says so.

## Findings

No decoder bug was found, so there is no `tests/regressions.rs`. The surviving mutants M08, M09, M16, M18 to M22, M24 and M25 were all gaps in the tests.

## What is still weak

- The decoder is never run at a level below the best on aarch64: `Level::new()` there is NEON and the scalar `Fallback` is the only other level. Any bug in code that is the same at both levels, and any bug in a kernel for a SIMD width this machine lacks (AVX2, SSE4.2, SSE2 are tested only by CI on x86), is invisible here.
- `Position::bit` of a `Corrupt` error is documented as "at or just after the offending field", so only a range can be asserted. A decoder that reported the end of the field instead of its start would pass.
- The doc test of `Error` (see above).
- `thirty_megabytes_of_mixed_data_at_level_9` and the other scale probes are single inputs.
- A mutation that changes only the speed (the lane thresholds, `SEG_ROWS`, margins) cannot be told from the original by output; the perf log is the check for those.
