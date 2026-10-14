//! Real CRAM bzip2 blocks (`corpus/cram-bzip2/`, git-ignored, produced
//! separately): each one decoded into an `out` of its declared size, at every
//! level, and compared with the oracle.
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
    let Some(dir) = corpus_dir() else {
        assert!(
            std::env::var_os("BZ2NGRS_CORPUS_REQUIRED").is_none_or(|v| v.is_empty()),
            "BZ2NGRS_CORPUS_REQUIRED is set but corpus/cram-bzip2 was not found"
        );
        eprintln!("note: corpus/cram-bzip2 not found, nothing to check (set BZ2NGRS_CORPUS)");
        return;
    };
    let blocks = manifest(&dir);
    assert!(
        !blocks.is_empty(),
        "{}: manifest.tsv lists no usable blocks",
        dir.display()
    );
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
