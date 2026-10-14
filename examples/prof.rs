//! Decodes one corpus file's chunks with bz2ngrs only, for a profiler.
//!
//! `cargo run --release --example prof -- FILE CHUNK_BYTES LEVEL SECONDS`

use std::io::Write;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&a[0]).expect("read input");
    let chunk: usize = a[1].parse().expect("chunk");
    let level: u32 = a[2].parse().expect("level");
    let secs: f64 = a[3].parse().expect("seconds");
    let comps: Vec<Vec<u8>> = data
        .chunks(chunk)
        .map(|r| {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
            e.write_all(r).expect("compress");
            e.finish().expect("compress")
        })
        .collect();
    let mut out = vec![0u8; chunk];
    // The best pass over the data: on a shared machine, slower passes are
    // other load or a stint on an efficiency core, not the decoder.
    let t = Instant::now();
    let mut best = 0f64;
    while t.elapsed().as_secs_f64() < secs {
        let p = Instant::now();
        let mut bytes = 0usize;
        for c in &comps {
            bytes += bz2ngrs::decompress(c, &mut out).expect("decode");
        }
        best = best.max(bytes as f64 / p.elapsed().as_secs_f64() / 1e6);
    }
    println!("{best:.1} MB/s");
}
