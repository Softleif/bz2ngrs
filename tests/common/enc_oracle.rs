//! The `bzip2` crate's encoder (libbz2 1.0.8 via `libbz2-rs-sys`) as the
//! oracle for `bz2ngrs::Encoder`, plus the encoders under test. No hegel in
//! here: the fuzz targets include this file with `#[path]`.
#![allow(dead_code)]

use std::cell::RefCell;

use bz2ngrs::Compression;
use fearless_simd::Level;

use super::oracle::{Reference, block_starts, reference};

/// One way of making a `bz2ngrs::Encoder`: `Encoder::new()`, or a fixed SIMD
/// level. Each thread keeps one long-lived encoder per variant and
/// compression level for [`Encoder::run`], so every call after the first is
/// a reuse, as in a writer.
#[derive(Clone, Copy)]
pub enum Encoder {
    New,
    At(Level),
}

thread_local! {
    static REUSED: RefCell<Vec<(String, u32, bz2ngrs::Encoder)>> = const { RefCell::new(Vec::new()) };
}

impl Encoder {
    fn make(self, level: u32) -> bz2ngrs::Encoder {
        let c = compression(level);
        match self {
            Encoder::New => bz2ngrs::Encoder::new(c),
            Encoder::At(simd) => bz2ngrs::Encoder::__with_level(c, simd),
        }
    }

    /// Compresses with this thread's long-lived encoder for `level`.
    pub fn run(self, data: &[u8], level: u32) -> Vec<u8> {
        let key = self.name();
        REUSED.with(|cell| {
            let mut all = cell.borrow_mut();
            let i = all
                .iter()
                .position(|(k, l, _)| *k == key && *l == level)
                .unwrap_or_else(|| {
                    all.push((key, level, self.make(level)));
                    all.len() - 1
                });
            all[i].2.compress_to_vec(data)
        })
    }

    /// Compresses with an encoder made for this one call.
    pub fn run_fresh(self, data: &[u8], level: u32) -> Vec<u8> {
        self.make(level).compress_to_vec(data)
    }

    pub fn name(self) -> String {
        match self {
            Encoder::New => "Encoder::new()".into(),
            Encoder::At(level) => format!("Encoder::__with_level({level:?})"),
        }
    }
}

/// The `Compression` for a level known to be 1 to 9.
pub fn compression(level: u32) -> Compression {
    Compression::new(level.clamp(1, 9)).unwrap_or_default()
}

/// The `Encoder` surface: `Encoder::new()` and every level this CPU can run,
/// the scalar `Fallback` included.
pub fn all_encoders() -> Vec<Encoder> {
    let best = Level::new();
    #[allow(unused_mut, reason = "only x86 has levels below the best one")]
    let mut levels = vec![Level::fallback(), best];
    #[cfg(target_arch = "x86_64")]
    {
        levels.extend(best.as_sse2().map(Level::Sse2));
        levels.extend(best.as_sse4_2().map(Level::Sse4_2));
        levels.extend(best.as_avx2().map(Level::Avx2));
    }
    let mut all = vec![Encoder::New];
    all.extend(levels.into_iter().map(Encoder::At));
    all
}

/// libbz2's stream for `data` at `level` in one `BZ_FINISH` call, as
/// `BZ2_bzBuffToBuffCompress` makes it. The output has libbz2's documented
/// bound (1% plus 600 bytes), so one call finishes.
#[allow(clippy::panic, reason = "an oracle failure is a test failure")]
pub fn libbz2_compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 100 + 600);
    let mut c = bzip2::Compress::new(bzip2::Compression::new(level), 30);
    match c.compress_vec(data, &mut out, bzip2::Action::Finish) {
        Ok(bzip2::Status::StreamEnd) => out,
        other => panic!("libbz2 did not finish: {other:?}"),
    }
}

/// Bits from the block magic to the origPtr field: 48 of magic, 32 of CRC,
/// 1 randomised bit.
const ORIG_PTR_OFFSET: usize = 81;

/// `ours` is libbz2's stream for `data` at `level`, byte for byte, except
/// that the origPtr of an exactly periodic block may name another of its
/// tied rows. A different origPtr that still decodes to the block's bytes
/// (the block CRC checks them) names a rotation equal to the block, so that
/// block is periodic; nothing else may differ.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn check_matches_libbz2(who: &str, ours: &[u8], data: &[u8], level: u32) {
    let theirs = libbz2_compress(data, level);
    if ours == theirs {
        return;
    }
    assert_eq!(
        ours.len(),
        theirs.len(),
        "{who}: stream length differs from libbz2's at level {level}"
    );
    let starts = block_starts(&theirs);
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        for k in 0..8 {
            if (a ^ b) & (0x80 >> k) != 0 {
                let bit = 8 * i + k;
                assert!(
                    starts.iter().any(|&s| {
                        (s + ORIG_PTR_OFFSET..s + ORIG_PTR_OFFSET + 24).contains(&bit)
                    }),
                    "{who}: differs from libbz2 at bit {bit} (level {level}), outside every origPtr"
                );
            }
        }
    }
    match reference(ours, data.len()) {
        Reference::Ok(out) => assert!(
            out == data,
            "{who}: a block with another origPtr decodes to other bytes"
        ),
        other => panic!("{who}: libbz2 rejects a stream that differs in an origPtr: {other:?}"),
    }
}

/// libbz2 decodes `stream` to exactly `data`.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn check_decodes(who: &str, stream: &[u8], data: &[u8]) {
    match reference(stream, data.len()) {
        Reference::Ok(out) => assert!(out == data, "{who}: libbz2 decodes other bytes"),
        other => panic!("{who}: libbz2 does not decode our stream: {other:?}"),
    }
}
