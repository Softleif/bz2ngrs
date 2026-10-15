//! The encoder's stages against references that share none of their code:
//! the sorter against a naive rotation sort and against divsufsort, the least
//! rotation against Booth's algorithm, origPtr against the rows of a naive
//! sort, MTF against libbz2's formulation, RLE1 against libbz2's byte loop,
//! and the code lengths against Huffman optimality.

use hegel::TestCase;
use hegel::generators as gs;

use super::{BitWriter, huff, mtf, rle, rotate, sais32, sort};

/// SplitMix64, for the large inputs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The rotation of `t` that starts at `r`.
fn rotation(t: &[u8], r: usize) -> Vec<u8> {
    t[r..].iter().chain(&t[..r]).copied().collect()
}

/// Row order of the rotations of `t` by direct comparison; tied rows stay in
/// the order of their start.
fn naive_rows(t: &[u8]) -> Vec<usize> {
    let n = t.len();
    let mut rows: Vec<usize> = (0..n).collect();
    rows.sort_by(|&a, &b| {
        (0..n)
            .map(|k| t[(a + k) % n])
            .cmp((0..n).map(|k| t[(b + k) % n]))
    });
    rows
}

/// Booth's least-rotation algorithm.
fn booth(s: &[u8]) -> usize {
    let n = s.len();
    let at = |i: usize| s[i % n];
    let mut f = vec![-1isize; 2 * n];
    let mut k = 0usize;
    for j in 1..2 * n {
        let sj = at(j);
        let mut i = f[j - k - 1];
        while i != -1 && sj != at(k + (i + 1) as usize) {
            if sj < at(k + (i + 1) as usize) {
                k = j - i as usize - 1;
            }
            i = f[i as usize];
        }
        if sj == at(k + (i + 1) as usize) {
            f[j - k] = i + 1;
        } else {
            if sj < at(k) {
                k = j;
            }
            f[j - k] = -1;
        }
    }
    k % n
}

/// Length of the primitive root of `t`.
fn root_len(t: &[u8]) -> usize {
    let n = t.len();
    (1..=n)
        .find(|&p| n.is_multiple_of(p) && (0..n).all(|i| t[i] == t[(i + p) % n]))
        .unwrap_or(n)
}

/// The Lyndon word whose powers are the least rotation of `t`.
fn lyndon(t: &[u8]) -> Vec<u8> {
    let mut r = rotation(t, booth(t));
    r.truncate(root_len(&r));
    r
}

/// The work array an earlier block left behind: up to twice what a block of
/// `n` bytes needs, of ints of any value. The sorter writes every int it
/// reads, so what was in it must not matter.
fn dirty_work(tc: &TestCase, n: usize) -> Vec<i32> {
    let len = tc.draw(gs::integers::<usize>()) % (2 * sort::work_len(n) + 1);
    let mut rng = Rng(tc.draw(gs::integers::<u64>()));
    (0..len).map(|_| rng.next() as i32).collect()
}

/// The sorter's results for the Lyndon word `t`, with `work` as its work
/// array: BWT, histogram, row of `q`.
fn run_bwt(t: &[u8], q: usize, mut work: Vec<i32>) -> (Vec<u8>, [i32; 256], usize) {
    let mut l = t.to_vec();
    let mut freq = [0; 256];
    let row = sort::bwt(&mut l, q, &mut freq, &mut work);
    (l, freq, row)
}

fn histogram(t: &[u8]) -> [i32; 256] {
    let mut h = [0; 256];
    for &b in t {
        h[usize::from(b)] += 1;
    }
    h
}

/// Bytes of any value, or over 1 to 4 symbols.
#[hegel::composite]
fn small_text(tc: &TestCase) -> Vec<u8> {
    let raw = tc.draw(gs::binary().max_size(400));
    match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => raw,
        1 => {
            let k = tc.draw(gs::integers::<u8>().min_value(1).max_value(4));
            raw.iter().map(|b| b % k).collect()
        }
        _ => {
            let pattern = tc.draw(gs::binary().min_size(1).max_size(6));
            let reps = tc.draw(gs::integers::<usize>().max_value(60));
            let mut t = pattern.repeat(reps);
            if let Some(&b) = raw.first()
                && !t.is_empty()
            {
                let at = usize::from(b) % t.len();
                t[at] = raw.get(1).copied().unwrap_or(0);
            }
            t
        }
    }
}

#[hegel::test(test_cases = 3000)]
fn bwt_matches_naive_rotation_sort(tc: TestCase) {
    let text = tc.draw(small_text());
    tc.assume(!text.is_empty());
    let t = lyndon(&text);
    let q = tc.draw(gs::integers::<usize>()) % t.len();
    let (l, freq, row) = run_bwt(&t, q, dirty_work(&tc, t.len()));
    let rows = naive_rows(&t);
    let n = t.len();
    let want: Vec<u8> = rows.iter().map(|&r| t[(r + n - 1) % n]).collect();
    assert_eq!(l, want, "BWT");
    assert_eq!(row, rows.iter().position(|&r| r == q).unwrap(), "row of q");
    assert_eq!(freq, histogram(&t), "histogram");
}

/// Large texts of the shapes that stress induced sorting: seeded random
/// bytes over small and full alphabets with runs, Fibonacci and Thue–Morse
/// words, a periodic text with a few changes, and long runs.
fn large_text(shape: u8, seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    match shape % 6 {
        0 | 1 => {
            let alphabet = if shape.is_multiple_of(6) {
                256
            } else {
                1 + rng.next() % 4
            };
            let mut t = Vec::with_capacity(len);
            while t.len() < len {
                let x = rng.next();
                let b = ((x >> 8) % alphabet) as u8;
                if x & 255 == 0 {
                    t.resize(len.min(t.len() + (x >> 40) as usize % 2000), b);
                } else {
                    t.push(b);
                }
            }
            t
        }
        2 => {
            let (mut a, mut b) = (vec![b'a'], vec![b'a', b'b']);
            while b.len() < len {
                let next = [b.as_slice(), a.as_slice()].concat();
                a = std::mem::replace(&mut b, next);
            }
            b.truncate(len);
            b
        }
        3 => (0..len).map(|i| (i.count_ones() & 1) as u8).collect(),
        4 => {
            let p = 1 + (rng.next() % 40) as usize;
            let pattern: Vec<u8> = (0..p).map(|_| rng.next() as u8 % 3).collect();
            let mut t: Vec<u8> = pattern.iter().copied().cycle().take(len).collect();
            for _ in 0..rng.next() % 4 {
                if !t.is_empty() {
                    let at = (rng.next() % len as u64) as usize;
                    t[at] = rng.next() as u8;
                }
            }
            t
        }
        _ => {
            let mut t = Vec::with_capacity(len);
            while t.len() < len {
                let run = 1 + (rng.next() % 5000) as usize;
                let b = (rng.next() % 3) as u8;
                t.resize(len.min(t.len() + run), b);
            }
            t
        }
    }
}

#[hegel::test(test_cases = 60)]
fn bwt_matches_divsufsort(tc: TestCase) {
    let shape = tc.draw(gs::integers::<u8>());
    let seed = tc.draw(gs::integers::<u64>());
    let len = 1 + tc.draw(gs::integers::<u32>()) as usize % 900_000;
    let t = lyndon(&large_text(shape, seed, len));
    let n = t.len();
    let q = tc.draw(gs::integers::<usize>()) % n;
    let (l, freq, row) = run_bwt(&t, q, dirty_work(&tc, n));
    // For a Lyndon word, suffix order is rotation order.
    let mut sa = vec![0; n];
    divsufsort::sort_in_place(&t, &mut sa);
    let want: Vec<u8> = sa.iter().map(|&s| t[(s as usize + n - 1) % n]).collect();
    assert!(l == want, "BWT differs from divsufsort's (n = {n})");
    assert_eq!(row, sa.iter().position(|&s| s as usize == q).unwrap());
    assert_eq!(freq, histogram(&t));
}

#[hegel::test(test_cases = 300)]
fn bwt_matches_divsufsort_on_drawn_bytes(tc: TestCase) {
    let raw = tc.draw(gs::binary().max_size(20_000));
    let k = tc.draw(gs::integers::<u16>().min_value(1).max_value(256));
    let text: Vec<u8> = raw.iter().map(|&b| (u16::from(b) % k) as u8).collect();
    tc.assume(!text.is_empty());
    let t = lyndon(&text);
    let n = t.len();
    let (l, _, row) = run_bwt(&t, n - 1, dirty_work(&tc, n));
    let mut sa = vec![0; n];
    divsufsort::sort_in_place(&t, &mut sa);
    let want: Vec<u8> = sa.iter().map(|&s| t[(s as usize + n - 1) % n]).collect();
    assert!(l == want, "BWT differs from divsufsort's (n = {n})");
    assert_eq!(row, sa.iter().position(|&s| s as usize == n - 1).unwrap());
}

/// The suffix array of `t` by direct comparison (a proper prefix sorts
/// first, as under the implicit sentinel).
fn naive_suffix_array(t: &[i32]) -> Vec<i32> {
    let mut sa: Vec<i32> = (0..t.len() as i32).collect();
    sa.sort_by(|&a, &b| t[a as usize..].cmp(&t[b as usize..]));
    sa
}

/// The recursion on a string of names, at every work-array size: from none
/// (the older recursion, its buckets allocated) to six ints per symbol and
/// more (libsais's 6k form), on random strings, on periodic strings with
/// edits, and on long strings over 1 to 4 symbols (where libsais renumbers
/// without compacting).
#[hegel::test(test_cases = 1000)]
fn recursion_matches_naive_suffix_sort(tc: TestCase) {
    let shape = tc.draw(gs::integers::<u8>().max_value(2));
    // A periodic string makes every naive comparison long, so it stays
    // short.
    let (n, k) = match shape {
        0 | 1 => {
            let n = tc.draw(
                gs::integers::<usize>()
                    .min_value(2)
                    .max_value(if shape == 1 { 3_000 } else { 40_000 }),
            );
            (
                n,
                tc.draw(gs::integers::<usize>().min_value(1).max_value(n.min(5_000))),
            )
        }
        _ => {
            let k = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
            (
                tc.draw(
                    gs::integers::<usize>()
                        .min_value(8192 * k)
                        .max_value(40_000),
                ),
                k,
            )
        }
    };
    let period = if shape == 1 {
        tc.draw(gs::integers::<usize>().min_value(1).max_value(n))
    } else {
        n
    };
    let mut rng = Rng(tc.draw(gs::integers::<u64>()));
    let base: Vec<i32> = (0..period)
        .map(|_| (rng.next() % k as u64) as i32)
        .collect();
    let mut t: Vec<i32> = (0..n).map(|i| base[i % period]).collect();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(20)) {
        let i = rng.next() as usize % n;
        t[i] = (rng.next() % k as u64) as i32;
    }
    let fs = tc.draw(gs::integers::<usize>().max_value(8 * k + 64));
    let mut work = vec![0x5A5A_5A5A; n + fs];
    let want = naive_suffix_array(&t);
    let orig = t.clone();
    match tc.draw(gs::integers::<u8>().max_value(3)) {
        0 => sais32::sort::<false, false>(&mut t, &mut work, fs, k),
        1 => sais32::sort::<false, true>(&mut t, &mut work, fs, k),
        2 => sais32::sort::<true, false>(&mut t, &mut work, fs, k),
        _ => sais32::sort::<true, true>(&mut t, &mut work, fs, k),
    }
    assert_eq!(t, orig, "the string is not restored");
    assert!(
        work[..n] == want[..],
        "suffix array differs (n = {n}, k = {k}, fs = {fs})"
    );
}

/// `t` followed by its copy and the padding, as the encoder lays it out.
fn doubled(t: &[u8]) -> Vec<u8> {
    let mut tt = t.repeat(2);
    tt.resize(2 * t.len() + rotate::PAD, 0);
    tt
}

#[hegel::test(test_cases = 3000)]
fn least_rotation_matches_booth(tc: TestCase) {
    let t = match tc.draw(gs::booleans()) {
        true => tc.draw(small_text()),
        false => {
            let shape = tc.draw(gs::integers::<u8>());
            let seed = tc.draw(gs::integers::<u64>());
            large_text(
                shape,
                seed,
                tc.draw(gs::integers::<u32>()) as usize % 50_000,
            )
        }
    };
    tc.assume(!t.is_empty());
    let tt = doubled(&t);
    for level in crate::tests::levels() {
        let mut cand = vec![0; t.len()];
        let (r, periodic) = fearless_simd::dispatch!(level, simd => rotate::least_rotation(simd, &tt, t.len(), &mut cand));
        assert!(
            rotation(&t, r) == rotation(&t, booth(&t)),
            "not the least rotation at {level:?}"
        );
        assert_eq!(periodic, root_len(&t) < t.len(), "periodicity at {level:?}");
    }
}

/// `sort_block` at every SIMD level on the block `t`, in a buffer and with a
/// work array that an earlier block left behind (the padding and the copy
/// are the sorter's to write): the BWT, the origPtr and the histogram.
fn check_sort_block(tc: &TestCase, t: &[u8], want_bwt: &[u8], want_orig: usize) {
    let n = t.len();
    let mut rng = Rng(tc.draw(gs::integers::<u64>()));
    let spare = tc.draw(gs::integers::<usize>().max_value(16));
    let mut start = t.to_vec();
    start.extend((0..n + rotate::PAD + spare).map(|_| rng.next() as u8));
    let work = dirty_work(tc, n);
    for level in crate::tests::levels() {
        let (mut tt, mut work) = (start.clone(), work.clone());
        let mut freq = [0; 256];
        let (orig, r) = fearless_simd::dispatch!(level, simd => rotate::sort_block(simd, &mut tt, n, &mut freq, &mut work));
        assert_eq!(tt[r..r + n], want_bwt[..], "BWT at {level:?}");
        assert_eq!(orig, want_orig, "origPtr at {level:?}");
        assert_eq!(freq, histogram(t), "histogram at {level:?}");
    }
}

#[hegel::test(test_cases = 3000)]
fn sort_block_gives_bwt_and_last_tied_row(tc: TestCase) {
    let t = tc.draw(small_text());
    tc.assume(!t.is_empty());
    let n = t.len();
    let rows = naive_rows(&t);
    let want: Vec<u8> = rows.iter().map(|&s| t[(s + n - 1) % n]).collect();
    // Rows whose rotation is the block itself; any is a valid origPtr, and
    // the encoder writes the last.
    let tied = (0..n).filter(|&i| rotation(&t, rows[i]) == t).max();
    check_sort_block(&tc, &t, &want, tied.unwrap());
}

/// The BWT of `t` and the last row holding `t` itself, for a `t` of thousands
/// of bytes: `t = u^m` with `u` primitive has the rotations of `u`, each `m`
/// times, in the order of the suffixes of `uu` that start in its first half
/// (a rotation's first `|u|` bytes decide, the rotations being distinct).
/// divsufsort sorts those suffixes.
fn bwt_and_last_tied_row(t: &[u8]) -> (Vec<u8>, usize) {
    let (p, m) = (root_len(t), t.len() / root_len(t));
    let u = &t[..p];
    let uu = u.repeat(2);
    let mut sa = vec![0; 2 * p];
    divsufsort::sort_in_place(&uu, &mut sa);
    let order: Vec<usize> = sa.iter().map(|&s| s as usize).filter(|&s| s < p).collect();
    let bwt = order
        .iter()
        .flat_map(|&s| std::iter::repeat_n(u[(s + p - 1) % p], m))
        .collect();
    let row = order.iter().position(|&s| s == 0).unwrap();
    (bwt, row * m + m - 1)
}

/// A pattern of 1 to 40 bytes over 1 to 256 symbols, repeated to 8000 bytes
/// or fewer, and in a third of the draws with a few bytes changed (which
/// makes the block primitive again).
#[hegel::composite]
fn periodic_text(tc: &TestCase) -> Vec<u8> {
    let p = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    let k = tc.draw(gs::integers::<u16>().min_value(1).max_value(256));
    let pattern: Vec<u8> = tc
        .draw(gs::binary().min_size(p).max_size(p))
        .iter()
        .map(|&b| (u16::from(b) % k) as u8)
        .collect();
    let m = 1 + tc.draw(gs::integers::<usize>()) % (8000 / p);
    let mut t = pattern.repeat(m);
    if tc.draw(gs::integers::<u8>().max_value(2)) == 0 {
        for _ in 0..tc.draw(gs::integers::<u8>().min_value(1).max_value(3)) {
            let at = tc.draw(gs::integers::<usize>()) % t.len();
            t[at] = tc.draw(gs::integers::<u8>());
        }
    }
    t
}

#[hegel::test(test_cases = 3000)]
fn sort_block_gives_bwt_and_last_tied_row_of_periodic_blocks(tc: TestCase) {
    let t = tc.draw(periodic_text());
    let (want, orig) = bwt_and_last_tied_row(&t);
    check_sort_block(&tc, &t, &want, orig);
}

/// libbz2's `generateMTFValues`, written from its description: a list of the
/// bytes in use, a pending count of zero ranks emitted in bijective base 2
/// (RUNA = 1, RUNB = 2), rank r >= 1 as r + 1, then EOB.
fn naive_mtf(l: &[u8]) -> (Vec<u16>, Vec<i32>) {
    let mut list: Vec<u8> = (0..=255u8).filter(|b| l.contains(b)).collect();
    let eob = list.len() + 1;
    let mut out = Vec::new();
    let mut z_pend = 0usize;
    let flush = |z: &mut usize, out: &mut Vec<u16>| {
        if *z > 0 {
            *z -= 1;
            loop {
                out.push((*z & 1) as u16);
                if *z < 2 {
                    break;
                }
                *z = (*z - 2) / 2;
            }
            *z = 0;
        }
    };
    for &c in l {
        let r = list.iter().position(|&x| x == c).unwrap();
        list.remove(r);
        list.insert(0, c);
        if r == 0 {
            z_pend += 1;
        } else {
            flush(&mut z_pend, &mut out);
            out.push((r + 1) as u16);
        }
    }
    flush(&mut z_pend, &mut out);
    out.push(eob as u16);
    let mut freq = vec![0; eob + 1];
    for &v in &out {
        freq[usize::from(v)] += 1;
    }
    (out, freq)
}

#[hegel::test(test_cases = 3000)]
fn mtf_matches_naive(tc: TestCase) {
    let l = match tc.draw(gs::booleans()) {
        true => tc.draw(small_text()),
        false => {
            let picks = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(300));
            let reps = tc.draw(gs::vecs(gs::integers::<u16>().max_value(3000)).max_size(300));
            picks
                .iter()
                .zip(reps.iter().chain(std::iter::repeat(&1)))
                .flat_map(|(&b, &r)| std::iter::repeat_n(b, usize::from(r)))
                .collect()
        }
    };
    let mut in_use = [false; 256];
    for &b in &l {
        in_use[usize::from(b)] = true;
    }
    let (want, want_freq) = naive_mtf(&l);
    for level in crate::tests::levels() {
        let mut mtfv = vec![0xBEEF; l.len() + 1 + mtf::SLACK];
        // The counts of the symbols in use start from whatever an earlier
        // block left; those beyond them from zero, and stay there.
        let mut freq = [0; mtf::MAX_ALPHA];
        freq[..want_freq.len()].fill(0x5A5A);
        let n = fearless_simd::dispatch!(level, simd => mtf::mtf(simd, &l, &in_use, &mut mtfv, &mut freq));
        assert_eq!(mtfv[..n], want[..], "values at {level:?}");
        assert_eq!(
            freq[..want_freq.len()],
            want_freq[..],
            "counts at {level:?}"
        );
        assert!(
            freq[want_freq.len()..].iter().all(|&c| c == 0),
            "a count beyond the end of block symbol at {level:?}"
        );
    }
}

/// CRC-32/BZIP2 one bit at a time, apart from the crate's CRC.
fn bitwise_crc(mut crc: u32, b: u8) -> u32 {
    crc ^= u32::from(b) << 24;
    for _ in 0..8 {
        crc = if crc & 0x8000_0000 != 0 {
            (crc << 1) ^ 0x04C1_1DB7
        } else {
            crc << 1
        };
    }
    crc
}

/// A block: its RLE1 bytes and its finalised CRC.
type Block = (Vec<u8>, u32);

/// libbz2's `ADD_CHAR_TO_BLOCK`, one byte at a time, with the block emitted
/// before a byte that finds it full and the pending unit flushed at the end.
fn naive_rle(data: &[u8], nblock_max: usize) -> Vec<Block> {
    let (mut ch, mut len) = (256u32, 0usize);
    let (mut block, mut crc) = (Vec::new(), !0u32);
    let mut blocks = Vec::new();
    let add_unit = |block: &mut Vec<u8>, crc: &mut u32, ch: u8, len: usize| {
        for _ in 0..len {
            *crc = bitwise_crc(*crc, ch);
        }
        if len < 4 {
            block.extend(std::iter::repeat_n(ch, len));
        } else {
            block.extend([ch, ch, ch, ch, (len - 4) as u8]);
        }
    };
    for &b in data {
        if block.len() >= nblock_max {
            blocks.push((std::mem::take(&mut block), !std::mem::replace(&mut crc, !0)));
        }
        let c = u32::from(b);
        if c != ch && len == 1 {
            add_unit(&mut block, &mut crc, ch as u8, 1);
            ch = c;
        } else if c != ch || len == 255 {
            if ch < 256 {
                add_unit(&mut block, &mut crc, ch as u8, len);
            }
            ch = c;
            len = 1;
        } else {
            len += 1;
        }
    }
    if ch < 256 {
        add_unit(&mut block, &mut crc, ch as u8, len);
    }
    if !block.is_empty() {
        blocks.push((block, !crc));
    }
    blocks
}

/// The same blocks through `rle::fill` at SIMD level `level`, fed in the
/// given chunks.
fn bulk_rle(
    level: fearless_simd::Level,
    data: &[u8],
    chunks: &[usize],
    nblock_max: usize,
) -> Vec<Block> {
    let mut blk = vec![0xA5; nblock_max + rle::SLACK];
    let mut st = rle::Rle::new();
    let mut blocks = Vec::new();
    let mut rest = data;
    let mut sizes = chunks.iter().copied().chain(std::iter::repeat(usize::MAX));
    while !rest.is_empty() {
        let size = sizes.next().unwrap_or(usize::MAX).clamp(1, rest.len());
        let mut chunk = &rest[..size];
        rest = &rest[size..];
        while !chunk.is_empty() {
            if st.nblock >= nblock_max {
                blocks.push((blk[..st.nblock].to_vec(), !st.crc));
                st.nblock = 0;
                st.crc = !0;
            }
            let used = fearless_simd::dispatch!(level, simd => rle::fill(simd, &mut st, chunk, &mut blk, nblock_max));
            assert!(used > 0, "no progress");
            chunk = &chunk[used..];
        }
    }
    st.flush(&mut blk);
    if st.nblock > 0 {
        blocks.push((blk[..st.nblock].to_vec(), !st.crc));
    }
    blocks
}

/// Runs and literals around the RLE1 boundaries.
#[hegel::composite]
fn rle_text(tc: &TestCase) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..tc.draw(gs::integers::<usize>().max_value(60)) {
        let byte = if tc.draw(gs::booleans()) {
            tc.draw(gs::integers::<u8>().max_value(3))
        } else {
            tc.draw(gs::integers::<u8>())
        };
        let len = match tc.draw(gs::integers::<u8>().max_value(2)) {
            0 => tc.draw(gs::sampled_from(vec![
                1usize, 2, 3, 4, 5, 254, 255, 256, 259, 510,
            ])),
            1 => tc.draw(gs::integers::<usize>().max_value(20)),
            _ => tc.draw(gs::integers::<usize>().max_value(2000)),
        };
        out.resize(out.len() + len, byte);
        out.extend(tc.draw(gs::binary().max_size(40)));
    }
    out
}

#[hegel::test(test_cases = 3000)]
fn rle_fill_matches_byte_loop(tc: TestCase) {
    let data = tc.draw(rle_text());
    // Small, to cut the data into several blocks, or any size a block has.
    let bound = if tc.draw(gs::booleans()) {
        3000
    } else {
        900_000
    };
    let nblock_max = 1 + tc.draw(gs::integers::<usize>()) % bound;
    let chunks = tc.draw(gs::vecs(gs::integers::<usize>().max_value(5000)).max_size(8));
    let want = naive_rle(&data, nblock_max);
    for level in crate::tests::levels() {
        assert_eq!(
            bulk_rle(level, &data, &chunks, nblock_max),
            want,
            "at {level:?}"
        );
    }
}

/// The cost of an optimal prefix code for `w` (Huffman's merge).
fn huffman_cost(w: &[u64]) -> u64 {
    let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<u64>> =
        w.iter().map(|&x| std::cmp::Reverse(x)).collect();
    let mut cost = 0;
    while heap.len() > 1 {
        let a = heap.pop().unwrap().0;
        let b = heap.pop().unwrap().0;
        cost += a + b;
        heap.push(std::cmp::Reverse(a + b));
    }
    cost
}

#[hegel::test(test_cases = 3000)]
fn code_lengths_are_limited_complete_and_optimal_when_unlimited(tc: TestCase) {
    let alpha = tc.draw(
        gs::integers::<usize>()
            .min_value(3)
            .max_value(mtf::MAX_ALPHA),
    );
    // A table's counts total at most a block's MTF values (under a million),
    // which keeps libbz2's `freq << 8` weights within i32.
    let raw = tc.draw(
        gs::vecs(gs::integers::<u32>())
            .min_size(alpha)
            .max_size(alpha),
    );
    let freq: Vec<i32> = if tc.draw(gs::booleans()) {
        let cap = 1_000_000 / alpha as u32 + 1;
        raw.iter().map(|&x| (x % cap) as i32).collect()
    } else {
        // Doubling chains, which need the limit, scaled to that total.
        let chain: Vec<u64> = raw.iter().map(|&x| 1 << (x % 20)).collect();
        let scale = chain.iter().sum::<u64>().div_ceil(1_000_000);
        chain.iter().map(|&c| (c / scale) as i32).collect()
    };
    // The encoder limits to 17; the limit is the function's parameter, down
    // to what the alphabet needs and up to libbz2's 20.
    let needed = usize::BITS - (alpha - 1).leading_zeros();
    let max_len = tc.draw(gs::integers::<u32>().min_value(needed).max_value(20)) as u8;
    let len = lengths_of(&[&freq], alpha, max_len)[0];
    let len = &len[..alpha];
    assert!(
        len.iter().all(|&l| (1..=max_len).contains(&l)),
        "lengths {len:?} over {max_len}"
    );
    let kraft: u64 = len.iter().map(|&l| 1u64 << (max_len - l)).sum();
    assert_eq!(kraft, 1 << max_len, "not a complete prefix code");
    let w: Vec<u64> = freq.iter().map(|&f| f.max(1) as u64).collect();
    let cost: u64 = w.iter().zip(len).map(|(&w, &l)| w * u64::from(l)).sum();
    let best = huffman_cost(&w);
    assert!(cost >= best);
    // A Huffman tree of depth d over weights >= 1 weighs at least the
    // (d + 2)th Fibonacci number, so below F(max_len + 3) none is deeper than
    // max_len and libbz2's code must be optimal.
    let (mut a, mut b) = (1u64, 1u64);
    for _ in 0..max_len + 2 {
        (a, b) = (b, a + b);
    }
    if w.iter().sum::<u64>() < a {
        assert_eq!(cost, best, "not optimal");
    }
}

/// The straight port of libbz2's `BZ2_hbMakeCodeLengths`, top-down heap and all: code lengths `len[..alpha]` for the
/// frequencies `freq`, limited to `max_len` bits by flattening the weights
/// and rebuilding until they fit.
#[allow(
    clippy::needless_range_loop,
    reason = "libbz2's loops, kept recognisable"
)]
fn reference_code_lengths(len: &mut [u8], freq: &[i32], alpha: usize, max_len: u8) {
    let mut heap = [0usize; mtf::MAX_ALPHA + 2];
    let mut weight = [0i32; mtf::MAX_ALPHA * 2];
    let mut parent = [0i32; mtf::MAX_ALPHA * 2];

    for i in 0..alpha {
        weight[i + 1] = freq[i].max(1) << 8;
    }

    loop {
        let mut n_nodes = alpha;
        let mut n_heap = 0;
        heap[0] = 0;
        weight[0] = 0;
        parent[0] = -2;

        let upheap = |heap: &mut [usize], weight: &[i32], z: usize| {
            let mut zz = z;
            let tmp = heap[zz];
            while weight[tmp] < weight[heap[zz >> 1]] {
                heap[zz] = heap[zz >> 1];
                zz >>= 1;
            }
            heap[zz] = tmp;
        };
        let downheap = |heap: &mut [usize], weight: &[i32], n_heap: usize, z: usize| {
            let mut zz = z;
            let tmp = heap[zz];
            loop {
                let mut yy = zz << 1;
                if yy > n_heap {
                    break;
                }
                if yy < n_heap && weight[heap[yy + 1]] < weight[heap[yy]] {
                    yy += 1;
                }
                if weight[tmp] < weight[heap[yy]] {
                    break;
                }
                heap[zz] = heap[yy];
                zz = yy;
            }
            heap[zz] = tmp;
        };

        for i in 1..=alpha {
            parent[i] = -1;
            n_heap += 1;
            heap[n_heap] = i;
            upheap(&mut heap, &weight, n_heap);
        }
        while n_heap > 1 {
            let n1 = heap[1];
            heap[1] = heap[n_heap];
            n_heap -= 1;
            downheap(&mut heap, &weight, n_heap, 1);
            let n2 = heap[1];
            heap[1] = heap[n_heap];
            n_heap -= 1;
            downheap(&mut heap, &weight, n_heap, 1);
            n_nodes += 1;
            parent[n1] = n_nodes as i32;
            parent[n2] = n_nodes as i32;
            let (w1, w2) = (weight[n1], weight[n2]);
            weight[n_nodes] = ((w1 & !0xff) + (w2 & !0xff)) | (1 + (w1 & 0xff).max(w2 & 0xff));
            parent[n_nodes] = -1;
            n_heap += 1;
            heap[n_heap] = n_nodes;
            upheap(&mut heap, &weight, n_heap);
        }

        // A node's parent is made after it, so one pass down from the root
        // gives every depth (libbz2 walks from each leaf to the root).
        let mut depth = [0u8; mtf::MAX_ALPHA * 2];
        for k in (1..n_nodes).rev() {
            depth[k] = depth[parent[k] as usize] + 1;
        }
        let mut too_long = false;
        for (l, &d) in len[..alpha].iter_mut().zip(&depth[1..=alpha]) {
            *l = d;
            too_long |= d > max_len;
        }
        if !too_long {
            break;
        }
        for w in &mut weight[1..=alpha] {
            *w = (1 + (*w >> 8) / 2) << 8;
        }
    }
}

/// `make_code_lengths_all` over `freqs` (at most six tables), one array per
/// table.
fn lengths_of(freqs: &[&[i32]], alpha: usize, max_len: u8) -> Vec<[u8; mtf::MAX_ALPHA]> {
    let mut w = huff::Lengths::new_boxed();
    let mut len = [[0u8; mtf::MAX_ALPHA]; 6];
    let mut freq = [[0i32; mtf::MAX_ALPHA]; 6];
    for (dst, src) in freq.iter_mut().zip(freqs) {
        dst[..alpha].copy_from_slice(src);
    }
    let ts: Vec<usize> = (0..freqs.len()).collect();
    huff::make_code_lengths_all(&mut w, &mut len, &freq, &ts, alpha, max_len);
    len[..freqs.len()].to_vec()
}

#[hegel::test(test_cases = 3000)]
fn code_lengths_match_the_straight_port_of_libbz2(tc: TestCase) {
    let alpha = tc.draw(
        gs::integers::<usize>()
            .min_value(3)
            .max_value(mtf::MAX_ALPHA),
    );
    // Small counts, so that equal weights (whose order the heap decides) are
    // everywhere; some from doubling chains, which need the length limit.
    let n_tables = tc.draw(gs::integers::<usize>().min_value(1).max_value(6));
    let mut tables: Vec<Vec<i32>> = Vec::new();
    for _ in 0..n_tables {
        // A table's counts total at most a block's MTF values, which keeps the
        // reference's `freq << 8` weights within i32 (it loops on overflow).
        let spread = tc
            .draw(gs::sampled_from(vec![2u32, 4, 20, 1000, 100_000]))
            .min(1_000_000 / alpha as u32);
        let raw = tc.draw(
            gs::vecs(gs::integers::<u32>())
                .min_size(alpha)
                .max_size(alpha),
        );
        tables.push(if tc.draw(gs::booleans()) {
            raw.iter().map(|&x| (x % spread) as i32).collect()
        } else {
            let chain: Vec<u64> = raw.iter().map(|&x| 1 << (x % 20)).collect();
            let scale = chain.iter().sum::<u64>().div_ceil(1_000_000);
            chain.iter().map(|&c| (c / scale) as i32).collect()
        });
    }
    let needed = usize::BITS - (alpha - 1).leading_zeros();
    let max_len = tc.draw(gs::integers::<u32>().min_value(needed).max_value(20)) as u8;
    let refs: Vec<&[i32]> = tables.iter().map(Vec::as_slice).collect();
    let got = lengths_of(&refs, alpha, max_len);
    for (g, f) in got.iter().zip(&tables) {
        let mut want = [0u8; mtf::MAX_ALPHA];
        reference_code_lengths(&mut want, f, alpha, max_len);
        assert_eq!(g[..alpha], want[..alpha]);
    }
}

/// A table of code lengths, as the encoder makes them: 1 to 17 bits.
fn code_length_table(tc: &TestCase) -> Vec<u8> {
    tc.draw(
        gs::vecs(gs::integers::<u8>().min_value(1).max_value(17))
            .min_size(3)
            .max_size(mtf::MAX_ALPHA),
    )
}

/// libbz2's `BZ2_hbAssignCodes`: a pass over the symbols for each length.
#[hegel::test(test_cases = 3000)]
fn canonical_codes_are_libbz2s(tc: TestCase) {
    let len = code_length_table(&tc);
    let (min, max) = (len.iter().min().unwrap(), len.iter().max().unwrap());
    let mut want = vec![0u32; len.len()];
    let mut vec = 0;
    for n in *min..=*max {
        for (c, &l) in want.iter_mut().zip(&len) {
            if l == n {
                *c = vec;
                vec += 1;
            }
        }
        vec <<= 1;
    }
    let mut got = vec![0u32; len.len()];
    huff::assign_codes(&mut got, &len);
    for ((g, w), l) in got.iter().zip(&want).zip(&len) {
        assert_eq!(*g, w << 5 | u32::from(*l));
    }
}

/// The length tables as libbz2 sends them, a step at a time.
#[hegel::test(test_cases = 3000)]
fn length_tables_are_sent_as_libbz2_sends_them(tc: TestCase) {
    let len = code_length_table(&tc);
    let mut want = Vec::new();
    {
        let mut bw = BitWriter::new(&mut want);
        bw.reserve(2000);
        let mut curr = len[0];
        bw.put(5, u64::from(curr));
        for &target in &len {
            while curr < target {
                bw.put(2, 2);
                curr += 1;
            }
            while curr > target {
                bw.put(2, 3);
                curr -= 1;
            }
            bw.put(1, 0);
        }
        bw.finish();
    }
    let mut got = Vec::new();
    {
        let mut bw = BitWriter::new(&mut got);
        bw.reserve(2000);
        huff::put_lengths(&mut bw, &len);
        bw.finish();
    }
    assert_eq!(got, want);
}
