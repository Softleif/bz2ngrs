//! Writes valid bzip2 streams to `fuzz/seeds/decompress_differential/`: small
//! ones at several levels and of several shapes, multi-block ones, and (when
//! `corpus/cram-bzip2/` is found, see `tests/corpus.rs`) small real CRAM
//! blocks. Run from the repository root:
//!
//!     cargo +nightly run --manifest-path fuzz/Cargo.toml --example gen_seeds

use std::fs;
use std::path::{Path, PathBuf};

use bz2ngrs_fuzz::oracle::{compress, compress_pieces};

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn bytes(&mut self, len: usize, alphabet: u64) -> Vec<u8> {
        (0..len)
            .map(|_| u8::try_from(self.next() % alphabet).expect("alphabet <= 256"))
            .collect()
    }
}

fn corpus_dir() -> Option<PathBuf> {
    let from = std::env::var_os("BZ2NGRS_CORPUS").map(PathBuf::from);
    from.or_else(|| {
        std::env::current_dir()
            .ok()?
            .ancestors()
            .map(|dir| dir.join("corpus/cram-bzip2"))
            .find(|dir| dir.join("manifest.tsv").is_file())
    })
}

/// Up to 12 blocks of at most 4 KiB per source in the corpus manifest.
fn small_cram_blocks(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let text = fs::read_to_string(dir.join("manifest.tsv")).unwrap_or_default();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
    let column = |name: &str| header.iter().position(|h| *h == name);
    let (Some(path), Some(source), Some(len)) =
        (column("path"), column("source"), column("compressed_len"))
    else {
        return Vec::new();
    };
    let mut per_source = std::collections::HashMap::<String, usize>::new();
    let mut out = Vec::new();
    for cols in lines.map(|l| l.split('\t').collect::<Vec<_>>()) {
        let (Some(p), Some(s), Some(n)) = (cols.get(path), cols.get(source), cols.get(len)) else {
            continue;
        };
        let small = n.trim().parse::<usize>().is_ok_and(|n| n <= 4096);
        let count = per_source.entry((*s).to_owned()).or_default();
        if small
            && *count < 12
            && let Ok(bytes) = fs::read(dir.join(p))
        {
            *count += 1;
            out.push((format!("cram-{}", p.replace('/', "-")), bytes));
        }
    }
    out
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/decompress_differential");
    fs::create_dir_all(&dir).expect("create seeds directory");
    let mut seeds: Vec<(String, Vec<u8>)> = Vec::new();
    let mut rng = Rng(1);

    seeds.push(("empty-l9".into(), compress(&[], 9)));
    seeds.push(("one-byte-l1".into(), compress(b"x", 1)));
    for level in [1, 5, 9] {
        let text = b"the quick brown fox jumps over the lazy dog ".repeat(40);
        seeds.push((format!("text-l{level}"), compress(&text, level)));
        seeds.push((
            format!("noise-l{level}"),
            compress(&rng.bytes(1500, 256), level),
        ));
        seeds.push((
            format!("dna-l{level}"),
            compress(&rng.bytes(3000, 4), level),
        ));
        seeds.push((
            format!("zeros-l{level}"),
            compress(&vec![0; 100_000], level),
        ));
        seeds.push((
            format!("abab-l{level}"),
            compress(&b"ab".repeat(5000), level),
        ));
    }
    for run in [3usize, 4, 5, 255, 256, 259, 260] {
        let mut data = b"x".to_vec();
        data.resize(run + 1, b'y');
        data.push(b'z');
        seeds.push((format!("run-{run}"), compress(&data, 1)));
    }
    // Several blocks: BZ_FLUSH after each piece.
    let pieces: Vec<Vec<u8>> = (1..=12)
        .map(|i| rng.bytes(40 * i, 1 + 20 * i as u64))
        .collect();
    let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    seeds.push(("blocks-12-l9".into(), compress_pieces(&refs, 9)));
    // A real block boundary at level 1 (about 100 KB of compressible data).
    seeds.push(("two-blocks-l1".into(), compress(&rng.bytes(150_000, 16), 1)));
    // Two streams back to back: the second must be ignored.
    let mut two = compress(b"first", 9);
    two.extend(compress(b"second", 9));
    seeds.push(("two-streams".into(), two));

    match corpus_dir() {
        Some(corpus) => seeds.extend(small_cram_blocks(&corpus)),
        None => eprintln!("note: corpus/cram-bzip2 not found, no real CRAM seeds"),
    }
    for (name, bytes) in &seeds {
        fs::write(dir.join(name), bytes).expect("write seed");
    }
    println!("wrote {} seeds to {}", seeds.len(), dir.display());
}
