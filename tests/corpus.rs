//! Real CRAM bzip2 blocks (`corpus/cram-bzip2/`, git-ignored, produced
//! separately): each one decoded into an `out` of its declared size, at every
//! level, and compared with the oracle; and some of their contents compressed
//! by every encoder and compared with libbz2.
//!
//! The corpus is found as `$BZ2NGRS_CORPUS`, or `corpus/cram-bzip2` in the
//! crate directory or any directory above it (so a git worktree finds the
//! main checkout's). Without it the test says so and passes, unless
//! `BZ2NGRS_CORPUS_REQUIRED` is set (for a CI that has the corpus).
//!
//! `manifest.tsv` has a header row and tab-separated columns; used here are
//! `path` (relative to the corpus directory) and `uncompressed_len`.

mod common;

use std::path::{Path, PathBuf};

use common::enc_oracle::{all_encoders, check_all};
use common::oracle::{Reference, all_decoders, reference};

fn corpus_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BZ2NGRS_CORPUS").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join("corpus/cram-bzip2"))
        .find(|dir| dir.join("manifest.tsv").is_file())
}

/// The manifest's blocks, or `None` (after saying so) without a corpus.
fn corpus_blocks() -> Option<Vec<(PathBuf, usize)>> {
    let Some(dir) = corpus_dir() else {
        assert!(
            std::env::var_os("BZ2NGRS_CORPUS_REQUIRED").is_none_or(|v| v.is_empty()),
            "BZ2NGRS_CORPUS_REQUIRED is set but corpus/cram-bzip2 was not found"
        );
        eprintln!("note: corpus/cram-bzip2 not found, nothing to check (set BZ2NGRS_CORPUS)");
        return None;
    };
    let blocks = manifest(&dir);
    assert!(
        !blocks.is_empty(),
        "{}: manifest.tsv lists no usable blocks",
        dir.display()
    );
    Some(blocks)
}

/// `(path, declared uncompressed length)` for each manifest row; rows that do
/// not parse are skipped.
fn manifest(dir: &Path) -> Vec<(PathBuf, usize)> {
    let text = std::fs::read_to_string(dir.join("manifest.tsv")).expect("readable manifest.tsv");
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
    let column = |name: &str| header.iter().position(|h| *h == name);
    let (Some(path), Some(len)) = (column("path"), column("uncompressed_len")) else {
        return Vec::new();
    };
    lines
        .filter_map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            Some((
                dir.join(cols.get(path)?),
                cols.get(len)?.trim().parse().ok()?,
            ))
        })
        .collect()
}

#[test]
fn real_cram_blocks_decode_like_the_reference() {
    let Some(blocks) = corpus_blocks() else {
        return;
    };
    let decoders = all_decoders();
    let mut failures = Vec::new();
    for (path, declared) in &blocks {
        let src = std::fs::read(path).expect("block listed in the manifest is readable");
        let Reference::Ok(want) = reference(&src, *declared) else {
            failures.push(format!(
                "{}: the reference does not decode it",
                path.display()
            ));
            continue;
        };
        if want.len() != *declared {
            failures.push(format!(
                "{}: reference gives {} bytes, manifest {declared}",
                path.display(),
                want.len()
            ));
        }
        for d in &decoders {
            let mut out = vec![0xA5; *declared];
            match d.run(&src, &mut out) {
                Ok(n) if n == *declared && out == want => {}
                got => failures.push(format!(
                    "{}: {}: {:?} (declared {declared})",
                    path.display(),
                    d.name(),
                    got
                )),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} blocks failed, first ones:\n{}",
        failures.len(),
        blocks.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Uncompressed bytes per corpus source that the encoder test compresses.
const ENCODE_BUDGET: usize = 2 << 20;

/// The contents of real CRAM blocks, each compressed as seqair writes it (one
/// stream per block) at levels 1 and 9: the first blocks of each source up to
/// [`ENCODE_BUDGET`] bytes, through [`check_all`].
#[test]
fn real_cram_data_compresses_like_libbz2() {
    let Some(blocks) = corpus_blocks() else {
        return;
    };
    let encoders = all_encoders();
    let mut spent = std::collections::HashMap::new();
    let mut failures = Vec::new();
    for (path, declared) in &blocks {
        let source = path.parent().unwrap_or(path);
        let used = spent.entry(source.to_path_buf()).or_insert(0);
        if *used >= ENCODE_BUDGET {
            continue;
        }
        let src = std::fs::read(path).expect("block listed in the manifest is readable");
        let Reference::Ok(data) = reference(&src, *declared) else {
            continue;
        };
        *used += data.len();
        for level in [1, 9] {
            if std::panic::catch_unwind(|| check_all(&encoders, &data, level)).is_err() {
                failures.push(format!("{} at level {level}", path.display()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
