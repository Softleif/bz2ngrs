# Changelog

## 0.1.2

Faster decoding and encoding. Output and behaviour are unchanged: the
encoder writes the same bytes, and the decoder accepts and rejects the same
streams with the same errors.

### Changed

- Decoding is about 12-60% faster than 0.1.1, most on small streams and
  repetitive data, and 1.1-1.4x the speed of bz2-ng's C library at level 9
  (measured on an Apple M-series machine with CRAM tag and quality data).
  Each Huffman code is now stepped through move-to-front as soon as it is
  decoded, the inverse BWT walks its lanes with less bookkeeping per step,
  and block headers and table builds cost less per block.
- Encoding is about 10-15% faster than 0.1.1. The block sorter now has
  libsais 2.12.0's resolution of tied LMS names before recursing, and its
  64-byte classification of suffix types; the move-to-front and Huffman
  code-length stages take fewer unpredictable branches.
- Inputs with long runs of one byte compress up to 4x faster, whole or
  through `CompressWriter` in small writes.

## 0.1.1

Hardening against hostile input.

### Fixed

- `Decoder::decompress_to_vec` no longer aborts the process when `size` is
  huge: it allocates at most 64 MiB up front and grows past that only as the
  stream really decodes to more. `size` is still the output limit.
- `DecompressReader` no longer moves its unread input on every stream and
  block, which made input of many tiny concatenated streams cost about a
  second of CPU per MiB. Each input byte is now moved about once.

## 0.1.0 - 2026-10-16

First release.
