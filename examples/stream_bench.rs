//! The streaming and growing-output paths against the one-shot calls and the
//! `bzip2` crate, on whole files.
//!
//! `cargo run --release --example stream_bench -- [ROUNDS] FILE...`: each file
//! is one level-9 stream of many blocks. Prints the median MB/s (10^6
//! uncompressed bytes per second) of each way to decode it and to encode it.

use std::io::{Read, Write};
use std::time::Instant;

use bz2ngrs::{CompressWriter, Compression, Decoder, DecompressReader, Encoder};

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn time(rounds: usize, bytes: usize, mut f: impl FnMut()) -> f64 {
    median(
        (0..rounds)
            .map(|_| {
                let t = Instant::now();
                f();
                bytes as f64 / t.elapsed().as_secs_f64() / 1e6
            })
            .collect(),
    )
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let rounds = match args.first().and_then(|a| a.parse().ok()) {
        Some(r) => {
            args.remove(0);
            r
        }
        None => 5,
    };
    println!("file\tMB\tway\tMB/s");
    for path in &args {
        let data = std::fs::read(path).expect("read input");
        let name = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut encoder = Encoder::new(Compression::BEST);
        let stream = encoder.compress_to_vec(&data);
        let n = data.len();
        let row = |way: &str, mbs: f64| {
            println!("{name}\t{:.1}\t{way}\t{mbs:.1}", n as f64 / 1e6);
        };

        let mut decoder = Decoder::new();
        let mut out = vec![0; n];
        row(
            "decode: Decoder::decompress (known size)",
            time(rounds, n, || {
                assert_eq!(decoder.decompress(&stream, &mut out), Ok(n));
            }),
        );
        row(
            "decode: Decoder::decompress_to_vec (known size)",
            time(rounds, n, || {
                assert_eq!(
                    decoder.decompress_to_vec(&stream, n).map(|v| v.len()),
                    Ok(n)
                );
            }),
        );
        row(
            "decode: Decoder::decompress_all_to_vec",
            time(rounds, n, || {
                assert_eq!(
                    decoder
                        .decompress_all_to_vec(&stream, usize::MAX)
                        .map(|v| v.len()),
                    Ok(n)
                );
            }),
        );
        let mut reader = DecompressReader::new(&stream[..]);
        row(
            "decode: DecompressReader (reused)",
            time(rounds, n, || {
                reader.reset(&stream[..]);
                let mut v = Vec::new();
                reader.read_to_end(&mut v).expect("decode");
                assert_eq!(v.len(), n);
            }),
        );
        row(
            "decode: bzip2::read::MultiBzDecoder",
            time(rounds, n, || {
                let mut v = Vec::new();
                bzip2::read::MultiBzDecoder::new(&stream[..])
                    .read_to_end(&mut v)
                    .expect("decode");
                assert_eq!(v.len(), n);
            }),
        );

        let mut sink = Vec::with_capacity(stream.len() + (1 << 20));
        row(
            "encode: Encoder::compress",
            time(rounds, n, || {
                sink.clear();
                encoder.compress(&data, &mut sink);
            }),
        );
        let mut writer = CompressWriter::new(Vec::new(), Compression::BEST);
        row(
            "encode: CompressWriter, 64 KiB writes (reused)",
            time(rounds, n, || {
                for c in data.chunks(64 << 10) {
                    writer.write_all(c).expect("encode");
                }
                writer.reset(Vec::new()).expect("encode");
            }),
        );
        row(
            "encode: bzip2::write::BzEncoder, 64 KiB writes",
            time(rounds, n, || {
                let mut w = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
                for c in data.chunks(64 << 10) {
                    w.write_all(c).expect("encode");
                }
                w.finish().expect("encode");
            }),
        );
    }
}
