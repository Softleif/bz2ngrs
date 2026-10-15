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
    // The runs in the data shrink in the first run-length stage, so 30 MB fill
    // fewer than 34 blocks of 899 981 bytes; the premise is dozens of them.
    assert!(
        count_blocks(&stream) >= 20,
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

/// Every encoder writes libbz2's stream for `data` at `level`, in at least
/// `blocks` blocks.
fn check_encoders_at_scale(data: &[u8], level: u32, blocks: usize) {
    use common::enc_oracle::{Encoder, all_encoders};
    let stream = Encoder::New.run(data, level);
    assert!(
        common::oracle::count_blocks(&stream) >= blocks,
        "{} blocks",
        common::oracle::count_blocks(&stream)
    );
    common::enc_props::check_all(&all_encoders(), data, level);
}

/// 3.8 MB of noise with runs at level 1: 38 blocks, so the combined CRC's
/// rotation laps 32 and every buffer is reused 37 times.
#[test]
fn encoder_writes_libbz2s_stream_for_three_megabytes_at_level_1() {
    check_encoders_at_scale(&mixed(7, 3_800_000, 256), 1, 33);
}

/// 3 MB at level 9 in regions of noise, a 4-symbol and a 16-symbol alphabet
/// with runs: blocks of the full 899 981 bytes, the size CRAM writers use.
#[test]
fn encoder_writes_libbz2s_stream_for_three_megabytes_at_level_9() {
    let data: Vec<u8> = [256, 4, 16]
        .into_iter()
        .enumerate()
        .flat_map(|(i, alphabet)| mixed(40 + i as u64, 1 << 20, alphabet))
        .collect();
    check_encoders_at_scale(&data, 9, 3);
}

/// 34 blocks that are each a 27-byte pattern 3703 times (27 divides the
/// 99 981 bytes of a level 1 block): 34 exactly periodic blocks.
#[test]
fn encoder_writes_libbz2s_stream_for_34_periodic_blocks() {
    let raw: Vec<u8> = (0..27u8).map(|i| i.wrapping_mul(37)).collect();
    let pattern = common::enc_props::run_free(&raw);
    let data: Vec<u8> = pattern
        .into_iter()
        .cycle()
        .take(34 * 99_981 + 1000)
        .collect();
    check_encoders_at_scale(&data, 1, 34);
}

/// 5.1 MB of one byte at level 1: units of 255 bytes, 19 997 of them per block.
#[test]
fn encoder_writes_libbz2s_stream_for_five_megabytes_of_one_byte() {
    check_encoders_at_scale(&vec![0x61; 5_100_000], 1, 2);
}
