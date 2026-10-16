# bz2ngrs

This is a bzip2 decoder and encoder, ported from [bz2-ng](https://github.com/tfenne/bz2-ng) to Rust.
It matches its speed (faster on Apple silicon, about even on x86-64), several times that of the `bzip2` crate (libbz2 in Rust).

If you know the decompressed size (as when reading a CRAM file),
`decompress_to_vec` allocates it once instead of growing the output,
which is a bit faster on highly compressible data.

```rust,no_run
use std::{fs::File, io::{self, BufWriter}};
use bz2ngrs::{CompressWriter, Compression, DecompressReader};

// Decompress a .bz2 file, one stream or several back to back.
let mut reader = DecompressReader::new(File::open("data.bz2")?);
io::copy(&mut reader, &mut io::stdout())?;

// Compress one.
let mut writer = CompressWriter::new(BufWriter::new(File::create("out.bz2")?), Compression::BEST);
io::copy(&mut File::open("data")?, &mut writer)?;
writer.finish()?;
# Ok::<(), io::Error>(())
```

You can use `Decoder`/`Encoder` multiple times to reuse their internal buffer.

## Compatibility

- The decoder accepts exactly the streams libbz2 1.0.8 accepts and returns an [`Error`], never a panic, for anything else. Concatenated streams decode as the `bzip2` crate's `MultiBzDecoder` decodes them.
- The encoder writes the same bytes as libbz2 1.0.8 (`bzip2 -1` to `-9`), however the input is split into writes, except for the origPtr of an exactly periodic block (see [`Encoder`]). Its block sorter is SA-IS, ported from libsais and from the sais-lite-derived sorter bz2-ng used before it, in safe Rust apart from a prefetch hint on x86.
- One thread per stream.
- Rust 1.89 or later.

## License

Apache-2.0 WITH LLVM-exception (`LICENSE`). Contains code ported from libsais (same licence, `LICENSE-libsais`), bz2-ng (MIT, `LICENSE-bz2-ng`), libbz2 (BSD-style, `LICENSE-bzip2`) and sais-lite (MIT, `LICENSE-sais-lite`), whose notices it keeps.
