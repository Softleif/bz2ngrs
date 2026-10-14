//! Decode throughput against the `bzip2` crate, as in notes/05-baseline-bench.md.
//!
//! `cargo run --release --example decode_bench -- [ROUNDS] FILE...`: each file
//! is cut into 64 KiB and 1 MiB chunks, each compressed at level 9 as its own
//! stream. Prints the median MB/s (10^6 bytes of output per second).

use std::io::Write;
use std::time::Instant;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
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
    println!("file\tchunk\tbz2ngrs\tbzip2\tratio");
    for path in &args {
        let data = std::fs::read(path).expect("read input");
        let name = std::path::Path::new(path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        for chunk in [64 << 10, 1 << 20] {
            let raws: Vec<&[u8]> = data.chunks(chunk).collect();
            let comps: Vec<Vec<u8>> = raws
                .iter()
                .map(|r| {
                    let mut e =
                        bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
                    e.write_all(r).expect("compress");
                    e.finish().expect("compress")
                })
                .collect();
            let mut decoder = bz2ngrs::Decoder::new();
            let mut out = vec![0u8; chunk];
            for (c, r) in comps.iter().zip(&raws) {
                let n = decoder.decompress(c, &mut out).expect("decode");
                assert!(out[..n] == **r, "round trip");
            }
            let total = data.len() as f64;
            let ours = median(
                (0..rounds)
                    .map(|_| {
                        let t = Instant::now();
                        for c in &comps {
                            std::hint::black_box(decoder.decompress(c, &mut out).expect("decode"));
                        }
                        total / t.elapsed().as_secs_f64() / 1e6
                    })
                    .collect(),
            );
            let mut v = Vec::with_capacity(chunk);
            let theirs = median(
                (0..rounds)
                    .map(|_| {
                        let t = Instant::now();
                        for c in &comps {
                            v.clear();
                            let mut d = bzip2::Decompress::new(false);
                            d.decompress_vec(c, &mut v).expect("decode");
                        }
                        total / t.elapsed().as_secs_f64() / 1e6
                    })
                    .collect(),
            );
            println!(
                "{name}\t{}K\t{ours:.1}\t{theirs:.1}\t{:.2}x",
                chunk >> 10,
                ours / theirs
            );
        }
    }
}
