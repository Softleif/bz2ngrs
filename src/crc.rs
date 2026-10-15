//! CRC-32/BZIP2: polynomial 0x04C11DB7, MSB-first (not reflected), init and
//! final XOR 0xFFFFFFFF, applied by the caller as libbz2 does.
//!
//! A port of bz2-ng's `crc32.c` and its instruction-set kernels (MIT): slice-by-16
//! everywhere, ARMv8 `crc32` instructions and PMULL folding on aarch64, PCLMULQDQ
//! folding on x86-64 (bz2-ng's VPCLMULQDQ kernel is left out: no machine here
//! can run it, and bz2-ng measures its SSE4 tier within 1–3% of AVX2). This module is the documented exception to
//! "SIMD only via fearless_simd", which has no carry-less multiply. The kernel is
//! picked once at run time and cached.

use std::sync::OnceLock;

type Kernel = fn(u32, &[u8]) -> u32;

/// Feeds `data` into a running CRC (without the init/final XOR).
pub(crate) fn update(crc: u32, data: &[u8]) -> u32 {
    static BEST: OnceLock<Kernel> = OnceLock::new();
    BEST.get_or_init(|| paths().last().map_or(slice16 as Kernel, |p| p.1))(crc, data)
}

/// Every kernel this CPU can run, slowest first. The accelerated ones are
/// unsafe to call on a CPU without their features, so each safe closure here
/// is built only inside the branch that detected them.
#[allow(dead_code, reason = "also used by tests and examples/crc_speed.rs")]
#[allow(
    unsafe_code,
    reason = "calls #[target_feature] kernels once each, right after detecting their features"
)]
pub(crate) fn paths() -> Vec<(&'static str, Kernel)> {
    let mut v: Vec<(&'static str, Kernel)> = vec![("bytewise", bytewise), ("slice16", slice16)];
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("crc") {
            v.push(("armv8-crc", |c, d| {
                // SAFETY: this closure exists only where `crc` was detected.
                unsafe { arm::crc_kernel(c, d) }
            }));
            if std::arch::is_aarch64_feature_detected!("aes") {
                v.push(("pmull", |c, d| {
                    // SAFETY: this closure exists only where `crc` and `aes` were detected;
                    // NEON is baseline.
                    unsafe { arm::pmull_kernel(c, d) }
                }));
            }
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("sse4.1")
        {
            v.push(("pclmul", |c, d| {
                // SAFETY: this closure exists only where pclmulqdq and sse4.1 were detected.
                unsafe { x86::pclmul_kernel(c, d) }
            }));
        }
    }
    v
}

/// Feeds `n` copies of `b` into a running CRC: the encoder's RLE1 runs,
/// whose bytes are no longer in one slice.
pub(crate) fn repeat(crc: u32, b: u8, n: usize) -> u32 {
    (0..n).fold(crc, |c, _| step(c, b))
}

/// The register after one byte.
#[inline(always)]
fn step(crc: u32, b: u8) -> u32 {
    (crc << 8) ^ TABLE[0][usize::from((crc >> 24) as u8 ^ b)]
}

/// The reference: one byte at a time.
fn bytewise(crc: u32, data: &[u8]) -> u32 {
    data.iter().fold(crc, |c, &b| step(c, b))
}

/// Sixteen bytes per step: `TABLE[k][b]` is the effect of byte `b` followed by
/// `k` zero bytes.
fn slice16(mut crc: u32, data: &[u8]) -> u32 {
    let (chunks, tail) = data.as_chunks::<16>();
    for c in chunks {
        let w = |i: usize| u32::from_be_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
        let mut r = 0;
        for (j, x) in [crc ^ w(0), w(4), w(8), w(12)].into_iter().enumerate() {
            let t = 12 - 4 * j;
            r ^= TABLE[t + 3][(x >> 24) as usize]
                ^ TABLE[t + 2][((x >> 16) & 0xff) as usize]
                ^ TABLE[t + 1][((x >> 8) & 0xff) as usize]
                ^ TABLE[t][(x & 0xff) as usize];
        }
        crc = r;
    }
    bytewise(crc, tail)
}

static TABLE: [[u32; 256]; 16] = {
    let mut t = [[0u32; 256]; 16];
    let mut i = 0u32;
    while i < 256 {
        let mut c = i << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            k += 1;
        }
        t[0][i as usize] = c;
        i += 1;
    }
    let mut k = 1;
    while k < 16 {
        let mut i = 0;
        while i < 256 {
            let p = t[k - 1][i];
            t[k][i] = (p << 8) ^ t[0][(p >> 24) as usize];
            i += 1;
        }
        k += 1;
    }
    t
};

// x^D mod P (P = 0x104C11DB7) for the folding distances D of the carry-less
// kernels. A 128-bit accumulator A folds forward by D bits as
// A * x^D = A_hi * (x^(D+64) mod P) + A_lo * (x^D mod P).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod k {
    #[cfg(target_arch = "aarch64")]
    pub(super) const K_1088: u64 = 0x10bd_4d7c;
    #[cfg(target_arch = "aarch64")]
    pub(super) const K_1024: u64 = 0x567f_ddeb;
    pub(super) const K_576: u64 = 0x8833_794c;
    pub(super) const K_512: u64 = 0xe622_8b11;
    pub(super) const K_448: u64 = 0x64bf_7a9b;
    pub(super) const K_384: u64 = 0x8c38_28a8;
    pub(super) const K_320: u64 = 0x5697_00e5;
    pub(super) const K_256: u64 = 0x75be_46b7;
    pub(super) const K_192: u64 = 0xc5b9_cd4c;
    pub(super) const K_128: u64 = 0xe8a4_5605;
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::k::*;
    use core::arch::aarch64::*;

    /// `x` with the bits of every byte reversed and the byte order kept.
    #[inline(always)]
    fn rev_bits_in_bytes(x: u64) -> u64 {
        x.reverse_bits().swap_bytes()
    }

    /// The instructions compute the reflected CRC with the same polynomial, so
    /// the register is bit-reversed on entry and exit and the bits of every
    /// input byte are reversed.
    #[target_feature(enable = "crc")]
    pub(super) fn crc_kernel(crc: u32, data: &[u8]) -> u32 {
        let mut r = crc.reverse_bits();
        let (words, bytes) = data.as_chunks::<8>();
        for w in words {
            r = __crc32d(r, rev_bits_in_bytes(u64::from_le_bytes(*w)));
        }
        for &b in bytes {
            r = __crc32b(r, b.reverse_bits());
        }
        r.reverse_bits()
    }

    #[inline]
    #[target_feature(enable = "neon")]
    fn consts(k_d: u64, k_d64: u64) -> poly64x2_t {
        vreinterpretq_p64_u64(vcombine_u64(vcreate_u64(k_d), vcreate_u64(k_d64)))
    }

    /// The 16 bytes in reverse order: bit i is the coefficient of x^i.
    #[inline]
    #[target_feature(enable = "neon,aes")]
    fn load_rev(p: &[u8; 16], rev: uint8x16_t) -> uint8x16_t {
        vqtbl1q_u8(vreinterpretq_u8_p128(u128::from_le_bytes(*p)), rev)
    }

    /// acc * x^D + d (mod P), each product at most 96 bits.
    #[inline]
    #[target_feature(enable = "neon,aes")]
    fn fold(acc: uint8x16_t, k: poly64x2_t, d: uint8x16_t) -> uint8x16_t {
        let a = vreinterpretq_p64_u8(acc);
        let lo = vreinterpretq_u8_p128(vmull_p64(vgetq_lane_p64::<0>(a), vgetq_lane_p64::<0>(k)));
        let hi = vreinterpretq_u8_p128(vmull_high_p64(a, k));
        veorq_u8(veorq_u8(lo, hi), d)
    }

    /// Eight accumulators folded by 1024 bits keep enough multiplies in flight
    /// on Apple cores and Neoverse V2. The residual and the tail go through
    /// the `crc32` instructions, as do inputs too short to fold.
    #[target_feature(enable = "neon,aes,crc")]
    pub(super) fn pmull_kernel(crc: u32, data: &[u8]) -> u32 {
        if data.len() < PMULL_MIN {
            return crc_kernel(crc, data);
        }
        // The `tbl` index 15, 14, ..., 0.
        let rev = vreinterpretq_u8_u64(vcombine_u64(
            vcreate_u64(0x0809_0a0b_0c0d_0e0f),
            vcreate_u64(0x0001_0203_0405_0607),
        ));
        let (k1024, k512, k128) = (
            consts(K_1024, K_1088),
            consts(K_512, K_576),
            consts(K_128, K_192),
        );
        let (blocks, tail) = data.as_chunks::<128>();
        let ld = |b: &[u8; 128]| -> [uint8x16_t; 8] {
            let (s, _) = b.as_chunks::<16>();
            core::array::from_fn(|i| load_rev(&s[i], rev))
        };
        let mut a = ld(&blocks[0]);
        // The register enters as an XOR into the first 32 message bits.
        a[0] = veorq_u8(
            a[0],
            vreinterpretq_u8_u32(vsetq_lane_u32::<3>(crc, vdupq_n_u32(0))),
        );
        for b in &blocks[1..] {
            let d = ld(b);
            for i in 0..8 {
                a[i] = fold(a[i], k1024, d[i]);
            }
        }
        for i in 0..4 {
            a[i] = fold(a[i], k512, a[i + 4]);
        }
        let mut tail = tail;
        if let Some((b, rest)) = tail.split_first_chunk::<64>() {
            let (s, _) = b.as_chunks::<16>();
            for i in 0..4 {
                a[i] = fold(a[i], k512, load_rev(&s[i], rev));
            }
            tail = rest;
        }
        let mut r = fold(a[0], consts(K_384, K_448), vdupq_n_u8(0));
        r = fold(a[1], consts(K_256, K_320), r);
        r = fold(a[2], k128, veorq_u8(r, a[3]));
        let (words, tail) = tail.as_chunks::<16>();
        for w in words {
            r = fold(r, k128, load_rev(w, rev));
        }
        // The register after the prefix is r * x^32 mod P: the register of r's
        // 16 bytes from a zero register, via the bit-reversed `crc32` kernel.
        let r64 = vreinterpretq_u64_u8(r);
        let reg = __crc32d(
            __crc32d(0, vgetq_lane_u64::<1>(r64).reverse_bits()),
            vgetq_lane_u64::<0>(r64).reverse_bits(),
        );
        crc_kernel(reg.reverse_bits(), tail)
    }

    /// Below this the `crc32` instructions are as fast as folding.
    const PMULL_MIN: usize = 256;
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::k::*;
    use super::slice16;
    use core::arch::x86_64::*;

    /// The two little-endian halves of 16 bytes.
    #[inline(always)]
    fn halves(p: &[u8; 16]) -> (u64, u64) {
        let (h, _) = p.as_chunks::<8>();
        (u64::from_le_bytes(h[0]), u64::from_le_bytes(h[1]))
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    fn kc(hi: u64, lo: u64) -> __m128i {
        _mm_set_epi64x(hi.cast_signed(), lo.cast_signed())
    }

    #[inline]
    #[target_feature(enable = "sse4.1")]
    fn rev16(v: __m128i) -> __m128i {
        _mm_shuffle_epi8(
            v,
            _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15),
        )
    }

    /// The 16 bytes at `p` in reverse order.
    #[inline]
    #[target_feature(enable = "sse4.1")]
    fn load_rev(p: &[u8; 16]) -> __m128i {
        let (lo, hi) = halves(p);
        rev16(_mm_set_epi64x(hi.cast_signed(), lo.cast_signed()))
    }

    /// acc * x^D mod P: `k` holds x^(D+64) mod P in the high qword, x^D mod P in the low.
    #[inline]
    #[target_feature(enable = "pclmulqdq")]
    fn fold(acc: __m128i, k: __m128i) -> __m128i {
        _mm_xor_si128(
            _mm_clmulepi64_si128::<0x11>(acc, k),
            _mm_clmulepi64_si128::<0x00>(acc, k),
        )
    }

    /// The register after the prefix whose 128-bit residual is `r`, then `tail`.
    #[inline]
    #[target_feature(enable = "sse4.1")]
    fn finish(r: __m128i, tail: &[u8]) -> u32 {
        let v = rev16(r);
        let (lo, hi) = (_mm_cvtsi128_si64(v), _mm_extract_epi64::<1>(v));
        let mut bytes = [0u8; 16];
        let (h, _) = bytes.as_chunks_mut::<8>();
        (h[0], h[1]) = (lo.to_le_bytes(), hi.to_le_bytes());
        slice16(slice16(0, &bytes), tail)
    }

    #[target_feature(enable = "pclmulqdq,sse4.1")]
    pub(super) fn pclmul_kernel(crc: u32, data: &[u8]) -> u32 {
        if data.len() < 64 {
            return slice16(crc, data);
        }
        let (k512, k128) = (kc(K_576, K_512), kc(K_192, K_128));
        let (blocks, tail) = data.as_chunks::<64>();
        let ld = |b: &[u8; 64]| -> [__m128i; 4] {
            let (s, _) = b.as_chunks::<16>();
            core::array::from_fn(|i| load_rev(&s[i]))
        };
        let mut a = ld(&blocks[0]);
        a[0] = _mm_xor_si128(a[0], _mm_set_epi32(crc.cast_signed(), 0, 0, 0));
        for b in &blocks[1..] {
            let d = ld(b);
            for i in 0..4 {
                a[i] = _mm_xor_si128(fold(a[i], k512), d[i]);
            }
        }
        let mut r = _mm_xor_si128(fold(a[0], kc(K_448, K_384)), fold(a[1], kc(K_320, K_256)));
        r = _mm_xor_si128(r, _mm_xor_si128(fold(a[2], k128), a[3]));
        let (words, tail) = tail.as_chunks::<16>();
        for w in words {
            r = _mm_xor_si128(fold(r, k128), load_rev(w));
        }
        finish(r, tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    /// CRC-32/BZIP2 one bit at a time, straight from the definition, so that
    /// it shares nothing with the lookup table the kernels are built on.
    fn bitwise(mut crc: u32, data: &[u8]) -> u32 {
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
        crc
    }

    /// A megabyte at an odd offset: the long-buffer loops of the folding
    /// kernels, which the 3000-byte properties stop short of.
    #[test]
    fn a_megabyte_at_an_odd_offset() {
        let buf: Vec<u8> = (0..(1u32 << 20) + 32)
            .map(|i| (i.wrapping_mul(2654435761) >> 11).to_le_bytes()[0])
            .collect();
        let d = &buf[5..5 + (1 << 20) + 13];
        let want = bitwise(!0, d);
        for (name, f) in paths() {
            assert_eq!(f(!0, d), want, "{name}");
        }
        assert_eq!(update(!0, d), want);
    }

    #[test]
    fn check_value() {
        // CRC-32/BZIP2 check value from the CRC catalogue.
        assert_eq!(!update(!0, b"123456789"), 0xFC89_1918);
        for (name, f) in paths() {
            assert_eq!(!f(!0, b"123456789"), 0xFC89_1918, "{name}");
        }
    }

    /// Every path on every length 0..=300 at every alignment, against the bit-by-bit definition.
    #[test]
    fn all_lengths_and_alignments() {
        let buf: Vec<u8> = (0..316u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13).to_le_bytes()[0])
            .collect();
        for (name, f) in paths() {
            for off in 0..16 {
                for len in 0..=300 {
                    let d = &buf[off..off + len];
                    for init in [0, !0, 0x1234_5678] {
                        assert_eq!(f(init, d), bitwise(init, d), "{name} off={off} len={len}");
                    }
                }
            }
        }
    }

    fn any_bytes() -> impl hegel::generators::PrintableGenerator<Vec<u8>> {
        gs::binary().max_size(3000)
    }

    #[hegel::test]
    fn matches_reference(tc: TestCase) {
        let data = tc.draw(any_bytes());
        let init = tc.draw(gs::integers::<u32>());
        let want = bitwise(init, &data);
        for (name, f) in paths() {
            assert_eq!(f(init, &data), want, "{name}");
        }
        assert_eq!(update(init, &data), want);
    }

    #[hegel::test]
    fn chaining(tc: TestCase) {
        let a = tc.draw(any_bytes());
        let b = tc.draw(any_bytes());
        let init = tc.draw(gs::integers::<u32>());
        let whole = [a.as_slice(), b.as_slice()].concat();
        for (name, f) in paths() {
            assert_eq!(f(f(init, &a), &b), f(init, &whole), "{name}");
        }
    }

    /// Throughput of each path, in GB/s: `cargo test --release -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn speed() {
        for len in [4 << 10, 64 << 10, 1 << 20] {
            let buf = vec![0xA5u8; len];
            for (name, f) in paths() {
                let iters = (1usize << 30) / len;
                let t = std::time::Instant::now();
                let mut c = 0;
                for _ in 0..iters {
                    c = f(std::hint::black_box(c), std::hint::black_box(&buf));
                }
                let gbs = (iters * len) as f64 / t.elapsed().as_secs_f64() / 1e9;
                println!("{name:10} {len:8} B {gbs:7.2} GB/s (crc {c:08x})");
            }
        }
    }
}
