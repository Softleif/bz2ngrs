//! The `bzip2` crate's encoder (libbz2 1.0.8 via `libbz2-rs-sys`) as the
//! oracle for `bz2ngrs::Encoder`, plus the encoders under test. No hegel in
//! here: the fuzz targets include this file with `#[path]`.
#![allow(dead_code)]
#![allow(
    clippy::cast_possible_truncation,
    reason = "run counts are at most 255, origPtr is 24 bits"
)]

use std::cell::RefCell;

use bz2ngrs::Compression;
use fearless_simd::Level;

use super::oracle::{Decoder, Reference, block_starts, check_exact, read_bits, reference};

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
    /// A new encoder of this variant.
    pub fn make(self, level: u32) -> bz2ngrs::Encoder {
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

/// libbz2's RLE1 stage, a byte at a time: the blocks it cuts `data` into at
/// `level`, as the bytes that go on to the BWT. A block ends before the next
/// byte once it holds `100000 * level - 19` bytes; the unit pending at the
/// end of the input joins the last block, full or not.
fn rle1_blocks(data: &[u8], level: u32) -> Vec<Vec<u8>> {
    let nblock_max = 100_000 * level as usize - 19;
    let add = |block: &mut Vec<u8>, ch: u8, len: usize| {
        if len < 4 {
            block.extend(std::iter::repeat_n(ch, len));
        } else {
            block.extend([ch, ch, ch, ch, (len - 4) as u8]);
        }
    };
    let (mut ch, mut len) = (256u32, 0usize);
    let (mut block, mut blocks) = (Vec::new(), Vec::new());
    for &b in data {
        if block.len() >= nblock_max {
            blocks.push(std::mem::take(&mut block));
        }
        let c = u32::from(b);
        if c != ch && len == 1 {
            add(&mut block, ch as u8, 1);
            ch = c;
        } else if c != ch || len == 255 {
            if ch < 256 {
                add(&mut block, ch as u8, len);
            }
            ch = c;
            len = 1;
        } else {
            len += 1;
        }
    }
    if ch < 256 {
        add(&mut block, ch as u8, len);
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    blocks
}

/// For every `i`, the length of the longest common prefix of `s` and
/// `s[i..]` (`z[0]` is `s.len()`).
fn z_function(s: &[u8]) -> Vec<usize> {
    let n = s.len();
    let mut z = vec![0; n];
    if let Some(z0) = z.first_mut() {
        *z0 = n;
    }
    let (mut l, mut r) = (0, 0);
    for i in 1..n {
        if i < r {
            z[i] = (r - i).min(z[i - l]);
        }
        while i + z[i] < n && s[z[i]] == s[i + z[i]] {
            z[i] += 1;
        }
        if i + z[i] > r {
            (l, r) = (i, i + z[i]);
        }
    }
    z
}

/// The length of the primitive root `u` of `block = u^m`, from its longest
/// border.
fn root_len(block: &[u8]) -> usize {
    let n = block.len();
    let mut border = vec![0; n + 1];
    for i in 1..n {
        let mut k = border[i];
        while k > 0 && block[i] != block[k] {
            k = border[k];
        }
        border[i + 1] = k + usize::from(block[i] == block[k]);
    }
    match n - border[n] {
        p if n.is_multiple_of(p) => p,
        _ => n,
    }
}

/// How many of the RLE1 blocks of `data` at `level` are exactly periodic.
pub fn periodic_blocks(data: &[u8], level: u32) -> usize {
    rle1_blocks(data, level)
        .iter()
        .filter(|b| root_len(b) < b.len())
        .count()
}

/// The origPtr of `block`: the last row, among its sorted rotations, that
/// holds the block itself. A block `u^m` with `u` primitive has the `p = |u|`
/// rotations of `u`, each `m` times over and in their order; so the rows
/// below the block number `m * c`, `c` being the rotations of `u` below `u`
/// (read off the Z-function of `uu`), and the block's `m` tied rows are the
/// next `m`. Shares nothing with the sorter.
fn last_tied_row(block: &[u8]) -> usize {
    let n = block.len();
    let p = root_len(block);
    let uu = [&block[..p], &block[..p]].concat();
    let z = z_function(&uu);
    let below = (1..p)
        .filter(|&s| z[s] < p && uu[s + z[s]] < uu[z[s]])
        .count();
    (n / p) * (below + 1) - 1
}

/// Each block of `ours` names, as its origPtr, the last row that holds the
/// block itself: the one row there is, or the last of an exactly periodic
/// block's tied rows (as bz2-ng writes it, where libbz2 may name another).
fn check_orig_ptrs(who: &str, ours: &[u8], data: &[u8], level: u32) {
    let blocks = rle1_blocks(data, level);
    let starts = block_starts(ours);
    assert_eq!(
        starts.len(),
        blocks.len(),
        "{who}: block count at level {level}"
    );
    for (i, (&start, block)) in starts.iter().zip(&blocks).enumerate() {
        let orig = read_bits(ours, start + ORIG_PTR_OFFSET, 24) as usize;
        assert_eq!(
            orig,
            last_tied_row(block),
            "{who}: origPtr of block {i} ({} bytes) at level {level}",
            block.len()
        );
    }
}

/// `ours` is libbz2's stream for `data` at `level`, byte for byte, except
/// that the origPtr of an exactly periodic block may name another of its
/// tied rows; and each block's origPtr is the last row that holds the block
/// ([`check_orig_ptrs`]). A different origPtr that still decodes to the
/// block's bytes (the block CRC checks them) names a rotation equal to the
/// block, so that block is periodic; nothing else may differ.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn check_matches_libbz2(who: &str, ours: &[u8], data: &[u8], level: u32) {
    check_orig_ptrs(who, ours, data, level);
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

/// A fresh encoder of the first variant writes libbz2's stream for `data`
/// at `level` (up to the origPtr of exactly periodic blocks), our decoder
/// reads it back, and the long-lived encoders of the other variants write
/// the same. The stream checked against libbz2 owes nothing to earlier
/// calls, so a failure replays in a new process; a long-lived encoder that
/// disagrees depends on earlier calls, or its SIMD level disagrees.
pub fn check_all(encoders: &[Encoder], data: &[u8], level: u32) {
    let Some((first, rest)) = encoders.split_first() else {
        return;
    };
    let stream = first.run_fresh(data, level);
    check_matches_libbz2(&first.name(), &stream, data, level);
    check_exact(Decoder::New, &stream, data);
    for e in rest {
        assert!(
            e.run(data, level) == stream,
            "{} (long-lived) and a fresh {} write different streams at level {level}",
            e.name(),
            first.name()
        );
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
