//! Decodes a set of streams with bz2ngrs only, for a profiler or an A/B run.
//!
//! `cargo run --release --example prof -- FILE CHUNK_BYTES LEVEL SECONDS`
//! compresses FILE in chunks first; `cram:DIR` as FILE (CHUNK and LEVEL are
//! then ignored) decodes the `.bz2` blocks in a CRAM corpus directory.

use std::io::Write;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let secs: f64 = a[3].parse().expect("seconds");
    let comps: Vec<Vec<u8>> = if let Some(dir) = a[0].strip_prefix("cram:") {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .expect("corpus dir")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "bz2"))
            .collect();
        paths.sort();
        paths
            .iter()
            .map(|p| std::fs::read(p).expect("read block"))
            .collect()
    } else {
        let data = std::fs::read(&a[0]).expect("read input");
        let chunk: usize = a[1].parse().expect("chunk");
        let level: u32 = a[2].parse().expect("level");
        data.chunks(chunk)
            .map(|r| {
                let mut e =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
                e.write_all(r).expect("compress");
                e.finish().expect("compress")
            })
            .collect()
    };
    let mut out = vec![0u8; 64 << 20];
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
