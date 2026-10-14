//! `cargo run --release --example crc_speed`: GB/s of every CRC path of
//! `src/crc.rs` and of the `crc-fast` crate (dev-dependency, for comparison).

#[path = "../src/crc.rs"]
#[allow(dead_code)]
mod crc;

use std::hint::black_box;
use std::time::Instant;

fn gbs(len: usize, mut f: impl FnMut() -> u32) -> f64 {
    let iters = (1usize << 30) / len;
    let t = Instant::now();
    let mut x = 0;
    for _ in 0..iters {
        x ^= f();
    }
    black_box(x);
    (iters * len) as f64 / t.elapsed().as_secs_f64() / 1e9
}

#[allow(clippy::cast_possible_truncation)]
fn fast(buf: &[u8]) -> u32 {
    crc_fast::checksum(crc_fast::CrcAlgorithm::Crc32Bzip2, buf) as u32
}

fn main() {
    let big: Vec<u8> = (0..1u32 << 20)
        .map(|i| (i.wrapping_mul(2654435761) >> 13).to_le_bytes()[0])
        .collect();
    assert_eq!(
        !crc::update(!0, &big[..65536]),
        fast(&big[..65536]),
        "crc-fast disagrees"
    );
    for len in [1 << 10, 20 << 10, 64 << 10, 120 << 10, 1 << 20] {
        let buf = &big[..len];
        println!("--- {len} bytes");
        for (name, f) in crc::paths() {
            println!(
                "{name:10} {:7.2} GB/s",
                gbs(len, || f(black_box(!0), black_box(buf)))
            );
        }
        println!(
            "{:10} {:7.2} GB/s",
            "crc-fast",
            gbs(len, || fast(black_box(buf)))
        );
    }
}
