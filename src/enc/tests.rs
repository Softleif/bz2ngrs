//! The encoder's stages against references that share none of their code:
//! the sorter against a naive rotation sort and against divsufsort, the least
//! rotation against Booth's algorithm, origPtr against the rows of a naive
//! sort, MTF against libbz2's formulation, RLE1 against libbz2's byte loop,
//! and the code lengths against Huffman optimality.

use hegel::TestCase;
use hegel::generators as gs;

use super::{huff, mtf, rle, rotate, sort};

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

/// The sorter's results for the Lyndon word `t`: BWT, histogram, row of `q`.
fn run_bwt(t: &[u8], q: usize) -> (Vec<u8>, [i32; 256], usize) {
    let mut l = t.to_vec();
    let mut freq = [0; 256];
    let mut work = Vec::new();
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
    let (l, freq, row) = run_bwt(&t, q);
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
    let (l, freq, row) = run_bwt(&t, q);
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
    let (l, _, row) = run_bwt(&t, n - 1);
    let mut sa = vec![0; n];
    divsufsort::sort_in_place(&t, &mut sa);
    let want: Vec<u8> = sa.iter().map(|&s| t[(s as usize + n - 1) % n]).collect();
    assert!(l == want, "BWT differs from divsufsort's (n = {n})");
    assert_eq!(row, sa.iter().position(|&s| s as usize == n - 1).unwrap());
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
    let mut cand = vec![0; t.len()];
    let (r, periodic) = rotate::least_rotation(&doubled(&t), t.len(), &mut cand);
    assert!(
        rotation(&t, r) == rotation(&t, booth(&t)),
        "not the least rotation"
    );
    assert_eq!(periodic, root_len(&t) < t.len(), "periodicity");
}

#[hegel::test(test_cases = 3000)]
fn sort_block_gives_bwt_and_last_tied_row(tc: TestCase) {
    let t = tc.draw(small_text());
    tc.assume(!t.is_empty());
    let n = t.len();
    let mut tt = doubled(&t);
    let mut freq = [0; 256];
    let mut work = Vec::new();
    let (orig, r) = rotate::sort_block(&mut tt, n, &mut freq, &mut work);
    let rows = naive_rows(&t);
    let want: Vec<u8> = rows.iter().map(|&s| t[(s + n - 1) % n]).collect();
    assert_eq!(tt[r..r + n], want[..], "BWT");
    // Rows whose rotation is the block itself; any is a valid origPtr, and
    // the encoder writes the last.
    let tied = (0..n).filter(|&i| rotation(&t, rows[i]) == t).max();
    assert_eq!(Some(orig), tied, "origPtr");
    assert_eq!(freq, histogram(&t));
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
    let mut mtfv = vec![0; l.len() + 1];
    let mut freq = [0; mtf::MAX_ALPHA];
    let n = mtf::mtf(&l, &in_use, &mut mtfv, &mut freq);
    let (want, want_freq) = naive_mtf(&l);
    assert_eq!(mtfv[..n], want[..], "values");
    assert_eq!(freq[..want_freq.len()], want_freq[..], "counts");
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

/// The same blocks through `rle::fill`, fed in the given chunks.
fn bulk_rle(data: &[u8], chunks: &[usize], nblock_max: usize) -> Vec<Block> {
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
            let used = rle::fill(&mut st, chunk, &mut blk, nblock_max);
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
        let byte = tc.draw(gs::integers::<u8>().max_value(3));
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
    let nblock_max = 1 + tc.draw(gs::integers::<usize>()) % 3000;
    let chunks = tc.draw(gs::vecs(gs::integers::<usize>().max_value(5000)).max_size(8));
    assert_eq!(
        bulk_rle(&data, &chunks, nblock_max),
        naive_rle(&data, nblock_max)
    );
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
    let mut len = [0u8; mtf::MAX_ALPHA];
    huff::make_code_lengths(&mut len, &freq, alpha, 17);
    let len = &len[..alpha];
    assert!(
        len.iter().all(|&l| (1..=17).contains(&l)),
        "lengths {len:?}"
    );
    let kraft: u64 = len.iter().map(|&l| 1u64 << (17 - l)).sum();
    assert_eq!(kraft, 1 << 17, "not a complete prefix code");
    let w: Vec<u64> = freq.iter().map(|&f| f.max(1) as u64).collect();
    let cost: u64 = w.iter().zip(len).map(|(&w, &l)| w * u64::from(l)).sum();
    let best = huffman_cost(&w);
    assert!(cost >= best);
    // A Huffman tree of depth d over weights >= 1 weighs at least the
    // (d + 2)th Fibonacci number, so below F(20) none is deeper than 17 and
    // libbz2's code must be optimal.
    if w.iter().sum::<u64>() < 6765 {
        assert_eq!(cost, best, "not optimal");
    }
}
