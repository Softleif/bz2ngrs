//! The scale probe: big streams, once, outside the generators. Per-block and
//! per-byte costs, buffer sizes and anything quadratic only show here.

mod common;

use std::sync::OnceLock;

use common::inputs::mixed;
use common::oracle::Kind;
use common::oracle::{all_decoders, check_exact, compress, compress_pieces, count_blocks};

/// About 30 MB in 1 MB regions of different statistics: noise, small
/// alphabets with runs, nearly constant data.
fn mixed_30mb() -> Vec<u8> {
    const ALPHABETS: [usize; 8] = [256, 4, 16, 2, 1, 90, 256, 8];
    (0..30)
        .flat_map(|i| mixed(i as u64, 1 << 20, ALPHABETS[i % ALPHABETS.len()]))
        .collect()
}

#[test]
fn thirty_megabytes_of_mixed_data_at_level_9() {
    let data = mixed_30mb();
    let stream = compress(&data, 9);
    // 30 MB of data holds at least 33 blocks of 899981 bytes: the premise of a
    // multi-block scale probe.
    assert!(
        count_blocks(&stream) >= 30,
        "{} blocks",
        count_blocks(&stream)
    );
    for d in all_decoders() {
        check_exact(d, &stream, &data);
    }
}

const ZEROS: usize = 50_000_000;

fn zeros_stream() -> &'static [u8] {
    static STREAM: OnceLock<Vec<u8>> = OnceLock::new();
    STREAM.get_or_init(|| compress(&vec![0; ZEROS], 9))
}

#[test]
fn fifty_megabytes_of_zeros_into_an_exact_out() {
    let stream = zeros_stream();
    for d in all_decoders() {
        check_exact(d, stream, &vec![0; ZEROS]);
    }
}

#[test]
fn fifty_megabytes_of_zeros_into_an_out_one_byte_short() {
    let stream = zeros_stream();
    for d in all_decoders() {
        let mut out = vec![0xA5; ZEROS - 1];
        assert_eq!(
            d.run_kind(stream, &mut out),
            Err(Kind::OutputTooSmall),
            "{}",
            d.name()
        );
    }
}

#[test]
fn hundreds_of_tiny_blocks() {
    for level in [1, 9] {
        let pieces: Vec<Vec<u8>> = (0..500usize)
            .map(|i| mixed(i as u64, 1 + (i * 7) % 200, 1 + i % 256))
            .collect();
        let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
        let stream = compress_pieces(&refs, level);
        assert!(
            count_blocks(&stream) >= 400,
            "level {level}: {} blocks",
            count_blocks(&stream)
        );
        for d in all_decoders() {
            check_exact(d, &stream, &pieces.concat());
        }
    }
}
