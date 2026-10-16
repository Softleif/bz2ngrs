//! Round trips against the `bzip2` crate (libbz2-rs-sys) at every SIMD level.

use std::io::Write;

use crate::{Decoder, Error, Position};

fn decompress(src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    Decoder::new().decompress(src, out)
}

fn decompress_at(level: fearless_simd::Level, src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    Decoder::__with_level(level).decompress(src, out)
}

pub(crate) fn levels() -> Vec<fearless_simd::Level> {
    let best = fearless_simd::Level::new();
    let v = vec![fearless_simd::Level::fallback(), best];
    #[cfg(target_arch = "x86_64")]
    let v = {
        let mut v = v;
        v.extend(best.as_sse2().map(fearless_simd::Level::Sse2));
        v.extend(best.as_sse4_2().map(fearless_simd::Level::Sse4_2));
        v.extend(best.as_avx2().map(fearless_simd::Level::Avx2));
        v
    };
    v
}

fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// libbz2's verdict: the output, or the kind of its error (`UnexpectedEof`
/// when the input ended early, `InvalidInput` for anything else).
fn reference(src: &[u8]) -> Result<Vec<u8>, std::io::ErrorKind> {
    let mut out = Vec::new();
    let mut d = bzip2::read::BzDecoder::new(src);
    std::io::Read::read_to_end(&mut d, &mut out)
        .map(|_| out)
        .map_err(|e| e.kind())
}

/// CRC-32/BZIP2 one bit at a time, so that the expected value of a test does
/// not come from the crate's own CRC.
fn bitwise_crc(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    !crc
}

fn check(data: &[u8], level: u32) {
    let c = compress(data, level);
    for lv in levels() {
        let mut out = vec![0xA5; data.len() + 7];
        let n = decompress_at(lv, &c, &mut out)
            .unwrap_or_else(|e| panic!("{lv:?} level {level}: {e:?}"));
        assert_eq!(n, data.len(), "{lv:?} level {level}");
        assert!(out[..n] == *data, "{lv:?} level {level}: wrong output");
        assert!(
            out[n..].iter().all(|&b| b == 0xA5),
            "{lv:?}: wrote past the end"
        );
        // Exact fit, and one byte short.
        let mut out = vec![0; data.len()];
        assert_eq!(decompress_at(lv, &c, &mut out), Ok(data.len()));
        if !data.is_empty() {
            let mut out = vec![0; data.len() - 1];
            assert_eq!(
                decompress_at(lv, &c, &mut out),
                Err(Error::OutputTooSmall {
                    capacity: data.len() - 1
                })
            );
        }
    }
}

fn lcg(n: usize, seed: u64, alphabet: u8) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 33) % u64::from(alphabet)) as u8
        })
        .collect()
}

#[test]
fn edge_inputs() {
    for level in [1, 6, 9] {
        check(b"", level);
        check(b"a", level);
        check(b"ab", level);
        check(b"aaaa", level);
        check(b"aaaaa", level);
        check(&[0u8; 300], level);
        check(&[7u8; 100_000], level);
        check(&b"ab".repeat(5000), level);
        check(&b"abcabcabd".repeat(3000), level);
    }
}

#[test]
fn runs_and_text() {
    // Runs of every length around the RLE1 thresholds, between literals.
    let mut d = Vec::new();
    for len in 0..300usize {
        d.extend(std::iter::repeat_n((len % 251) as u8, len));
        d.push(b'x');
    }
    check(&d, 9);
    check(&lcg(50_000, 1, 4), 9);
    check(&lcg(50_000, 2, 255), 9);
    let text: Vec<u8> = (0..20_000)
        .flat_map(|i: u32| format!("line {i} of some text\n").into_bytes())
        .collect();
    check(&text, 9);
}

#[test]
fn sizes_across_lane_counts() {
    // Blocks of about 4 Ki, 16 Ki and 64 Ki rows switch to 2, 8 and 24 lanes.
    for n in [
        100, 2047, 2048, 4095, 4096, 5000, 16_383, 16_384, 40_000, 65_535, 65_536, 200_000, 899_000,
    ] {
        check(&lcg(n, n as u64, 200), 9);
    }
}

#[test]
fn multi_block() {
    // Level 1 has 100 kB blocks.
    check(&lcg(350_000, 3, 255), 1);
    check(&lcg(350_000, 4, 3), 1);
    check(&[0u8; 2_000_000], 1);
}

#[test]
fn trailing_bytes_are_ignored() {
    let mut c = compress(b"hello hello hello", 9);
    c.extend_from_slice(b"garbage after the stream");
    let mut out = [0u8; 64];
    assert_eq!(decompress(&c, &mut out), Ok(17));
    assert_eq!(&out[..17], b"hello hello hello");
}

/// A reader that hands out at most `step` bytes per call.
struct Trickle<'a> {
    src: &'a [u8],
    step: usize,
}

impl std::io::Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.src.len());
        buf[..n].copy_from_slice(&self.src[..n]);
        self.src = &self.src[n..];
        Ok(n)
    }
}

fn read_all(src: &[u8], step: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    std::io::Read::read_to_end(
        &mut crate::DecompressReader::new(Trickle { src, step }),
        &mut out,
    )
    .map(|_| out)
}

#[test]
fn concatenated_streams() {
    let parts = [
        lcg(250_000, 5, 255),
        b"".to_vec(),
        vec![9u8; 1_000_000],
        lcg(10, 6, 3),
    ];
    let mut src = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        src.extend(compress(p, [1, 9, 2, 5][i]));
    }
    let want = parts.concat();

    let mut out = b"prefix".to_vec();
    assert_eq!(
        Decoder::new().decompress_all(&src, &mut out, usize::MAX),
        Ok(want.len())
    );
    assert!(out[6..] == want[..]);
    assert_eq!(
        Decoder::new().decompress_all(&src, &mut out, want.len() - 1),
        Err(Error::OutputTooSmall {
            capacity: want.len() - 1
        })
    );
    assert_eq!(out.len(), 6 + want.len());

    let first = compress(&parts[0], 1).len();
    let mut big = vec![0; 300_000];
    let d = Decoder::new().decompress_prefix(&src, &mut big).unwrap();
    assert_eq!((d.written, d.read), (parts[0].len(), first));

    for step in [1, 7, 4096, 1 << 20] {
        assert!(read_all(&src, step).unwrap() == want, "step {step}");
    }
}

#[test]
fn bytes_after_the_streams_are_an_error() {
    let mut src = compress(b"hello", 9);
    let len = src.len();
    src.extend_from_slice(b"BZ");
    let mut out = Vec::new();
    assert!(matches!(
        Decoder::new().decompress_all(&src, &mut out, 100),
        Err(Error::UnexpectedEof { at }) if at.bit == src.len() as u64 * 8
    ));
    assert!(out.is_empty());
    src.truncate(len);
    src.push(0);
    assert_eq!(
        Decoder::new().decompress_all(&src, &mut out, 100),
        Err(Error::NotBzip2)
    );
    let e = read_all(&src, 3).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        e.get_ref().unwrap().downcast_ref::<Error>(),
        Some(&Error::NotBzip2)
    );
    assert_eq!(
        read_all(b"", 3).unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn reader_positions_count_from_the_first_byte() {
    let data = lcg(300_000, 8, 255);
    let c = compress(&data, 1);
    for cut in [c.len() / 2, c.len() - 1] {
        let e = read_all(&c[..cut], 1000).unwrap_err();
        let inner = *e.get_ref().unwrap().downcast_ref::<Error>().unwrap();
        assert_eq!(
            inner,
            decompress(&c[..cut], &mut vec![0; data.len()]).unwrap_err()
        );
    }
}

#[test]
fn truncation_at_every_byte() {
    let data = lcg(3000, 9, 20);
    let c = compress(&data, 9);
    let mut out = vec![0; data.len()];
    for cut in 0..c.len() {
        assert!(
            matches!(
                decompress(&c[..cut], &mut out),
                Err(Error::UnexpectedEof { at }) if at.bit == cut as u64 * 8
            ),
            "cut {cut}"
        );
    }
}

#[test]
fn corruptions_match_libbz2() {
    let data = lcg(5000, 5, 30);
    let c = compress(&data, 9);
    let mut out = vec![0; data.len() + 100];
    for i in 0..c.len() {
        for bit in [0, 3, 7] {
            let mut bad = c.clone();
            bad[i] ^= 1 << bit;
            let ours = decompress(&bad, &mut out).map(|n| out[..n].to_vec());
            let theirs = reference(&bad);
            match (&ours, &theirs) {
                (Ok(a), Ok(b)) => assert!(a == b, "byte {i} bit {bit}: different output"),
                (Err(Error::UnexpectedEof { .. }), Err(std::io::ErrorKind::UnexpectedEof)) => {}
                (
                    Err(
                        Error::NotBzip2
                        | Error::Corrupt { .. }
                        | Error::BlockCrc { .. }
                        | Error::StreamCrc { .. },
                    ),
                    Err(std::io::ErrorKind::InvalidInput),
                ) => {}
                // A block that grows past `out` before its CRC is checked.
                (Err(Error::OutputTooSmall { .. }), Err(std::io::ErrorKind::InvalidInput)) => {}
                _ => panic!("byte {i} bit {bit}: ours {ours:?}, libbz2 {theirs:?}"),
            }
        }
    }
}

#[test]
fn bad_headers() {
    let mut out = [0u8; 16];
    assert_eq!(decompress(b"BZh0", &mut out), Err(Error::NotBzip2));
    assert_eq!(decompress(b"BZx9", &mut out), Err(Error::NotBzip2));
    let eof = |bit| {
        Err(Error::UnexpectedEof {
            at: Position { block: 0, bit },
        })
    };
    assert_eq!(decompress(b"BZh9", &mut out), eof(32));
    assert_eq!(decompress(b"", &mut out), eof(0));
    // An empty stream (combined CRC 0) with its last bit flipped.
    let mut c = compress(b"", 9);
    assert_eq!(c.len(), 14);
    c[13] ^= 1;
    assert_eq!(
        decompress(&c, &mut out),
        Err(Error::StreamCrc {
            stored: 1,
            computed: 0
        })
    );
}

/// Sets the randomised bit of a single-block stream's block, then fixes up
/// the block and combined CRCs to the output libbz2 produces for it.
#[test]
fn randomised_block() {
    let data = lcg(30_000, 11, 50);
    let mut c = compress(&data, 9);
    c[14] |= 0x80; // the bit right after the block CRC (bytes 10..14)
    // libbz2 delivers the block, then reports the CRC error.
    let mut d = bzip2::Decompress::new(false);
    let mut y = Vec::with_capacity(data.len() * 2);
    assert!(d.decompress_vec(&c, &mut y).is_err());
    assert_ne!(y, data);
    let crc = bitwise_crc(&y);
    c[10..14].copy_from_slice(&crc.to_be_bytes());
    // The combined CRC is the last 32 bits before the 0..7 padding bits.
    let total = c.len() * 8;
    let pad = (0..8)
        .find(|p| {
            let at = total - p - 80;
            (0..48).all(|k| {
                let bit = |i: usize| (c[i / 8] >> (7 - i % 8)) & 1;
                bit(at + k) == ((0x1772_4538_5090u64 >> (47 - k)) & 1) as u8
            })
        })
        .unwrap();
    let at = total - pad - 32;
    for k in 0..32 {
        let i = at + k;
        let v = ((crc >> (31 - k)) & 1) as u8;
        c[i / 8] = (c[i / 8] & !(1 << (7 - i % 8))) | (v << (7 - i % 8));
    }
    assert_eq!(reference(&c), Ok(y.clone()));
    for lv in levels() {
        let mut out = vec![0; y.len()];
        assert_eq!(decompress_at(lv, &c, &mut out), Ok(y.len()));
        assert!(out == y);
    }
}

#[test]
fn display_carries_the_context() {
    use crate::Corruption;
    let at = Position {
        block: 3,
        bit: 18203,
    };
    let shown = |e: Error| e.to_string();
    assert_eq!(
        shown(Error::Corrupt {
            kind: Corruption::Selector,
            at
        }),
        "corrupt bzip2 stream: selector out of range (block 3, bit 18203)"
    );
    assert_eq!(
        shown(Error::BlockCrc {
            block: 0,
            stored: 0x1234_abcd,
            computed: 0x89ab_0123
        }),
        "bzip2 block 0 CRC mismatch: stored 0x1234abcd, computed 0x89ab0123"
    );
    assert_eq!(
        shown(Error::UnexpectedEof { at }),
        "bzip2 stream ends early (block 3, bit 18203)"
    );
    assert_eq!(
        shown(Error::OutputTooSmall { capacity: 7 }),
        "bzip2 stream decodes to more than 7 bytes"
    );
    assert_eq!(
        shown(Error::NotBzip2),
        "not a bzip2 stream (no BZh1 to BZh9 header)"
    );
    assert_eq!(
        shown(Error::StreamCrc {
            stored: 1,
            computed: 0xfc89_1918
        }),
        "bzip2 stream CRC mismatch: stored 0x00000001, computed 0xfc891918"
    );
}
