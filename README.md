# bz2ngrs

Fast bzip2 in Rust: a decoder and an encoder ported from [bz2-ng](https://github.com/tfenne/bz2-ng), for files and streams as well as buffers in memory. On the benchmark corpora (`bench/`) both are faster than bz2-ng's C on Apple silicon and about as fast on x86-64 (Zen 2), and several times faster than libbz2 (the `bzip2` crate).

```rust,no_run
use std::fs::File;
use std::io::{self, BufWriter};
use bz2ngrs::{CompressWriter, Compression, DecompressReader};

// Decompress a .bz2 file: one stream or several back to back, as pbzip2 writes them.
let mut reader = DecompressReader::new(File::open("data.bz2")?);
io::copy(&mut reader, &mut io::stdout())?;

// Compress one.
let mut writer = CompressWriter::new(BufWriter::new(File::create("out.bz2")?), Compression::BEST);
io::copy(&mut File::open("data")?, &mut writer)?;
writer.finish()?;
# Ok::<(), io::Error>(())
```

## Which API

| you have | to decompress | to compress |
|---|---|---|
| a file, a socket, any `Read` / `Write` | [`DecompressReader`] | [`CompressWriter`] |
| a whole `.bz2` file in memory | [`Decoder::decompress_all`] | [`Encoder::compress`] |
| a stream whose decompressed size is known (a container declares it, as CRAM does) | [`Decoder::decompress`], [`Decoder::decompress_to_vec`] | [`Encoder::compress`] |
| a stream with other data after it | [`Decoder::decompress_prefix`] | |

`Decoder` and `Encoder` own several megabytes of working memory, allocated on first use. Keep one per thread and reuse it; the reader and the writer keep theirs across [`DecompressReader::reset`] and [`CompressWriter::reset`].

```rust
use bz2ngrs::{Compression, Decoder, Encoder};

let data = b"hello hello hello hello";
let mut encoder = Encoder::new(Compression::BEST);
let stream = encoder.compress_to_vec(data);

let mut decoder = Decoder::new();
// The size is unknown: the output grows, up to a bound (bzip2 can expand data a thousandfold).
assert_eq!(decoder.decompress_all_to_vec(&stream, 1 << 30).unwrap(), data);
// The size is known: one allocation, no copies.
assert_eq!(decoder.decompress_to_vec(&stream, data.len()).unwrap(), data);
```

## Compatibility

- The decoder accepts exactly the streams libbz2 1.0.8 accepts (levels 1 to 9, any number of blocks, randomised blocks), and returns a typed [`Error`], never a panic, for anything else. Concatenated streams decode as the `bzip2` crate's `MultiBzDecoder` decodes them.
- The encoder writes the same bytes as libbz2 1.0.8 (`bzip2 -1` to `-9`), however its input is split into writes, except for the origPtr of blocks that are exactly periodic (see [`Encoder`]). Its block sorter is induced sorting (SA-IS), in safe Rust apart from a prefetch hint on x86: libsais's top level, with the recursion and the final induction of the sorter that bz2-ng used before it switched to libsais.
- Each stream is decoded and encoded on one thread.
- Rust 1.89 or later.

## License

Apache-2.0 WITH LLVM-exception (`LICENSE`). Contains code ported from libsais (the same licence, `LICENSE-libsais`), bz2-ng (MIT, `LICENSE-bz2-ng`), libbz2 (BSD-style, `LICENSE-bzip2`) and sais-lite (MIT, `LICENSE-sais-lite`), whose notices it keeps.
