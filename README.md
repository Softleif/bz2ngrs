# bz2ngrs

Fast bzip2 in Rust, one call per stream: a decoder that decodes a whole stream from a byte slice into a buffer, and an encoder that compresses a whole input into one stream.

Ported from the decoder and the encoder of [bz2-ng](https://github.com/tfenne/bz2-ng). The encoder writes the same bytes as libbz2 1.0.8, except for the origPtr of blocks that are exactly periodic (see [`Encoder`]). Its block sorter is the induced sorter (SA-IS, derived from sais-lite) that bz2-ng used before it switched to libsais, ported to safe Rust.

```rust
use bz2ngrs::{Compression, Decoder, Encoder};

let data = b"hello hello hello hello";
let stream = Encoder::new(Compression::BEST).compress_to_vec(data);
let back = Decoder::new().decompress_to_vec(&stream, data.len()).unwrap();
assert_eq!(back, data);
```

## License

MIT. Contains code from bz2-ng (MIT, `LICENSE-bz2-ng`), libbz2 (BSD-style, `LICENSE-bzip2`) and sais-lite (MIT, `LICENSE-sais-lite`).
