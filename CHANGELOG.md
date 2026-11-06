# Changelog

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
