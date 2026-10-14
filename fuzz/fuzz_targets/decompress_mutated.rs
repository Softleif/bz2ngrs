#![no_main]

//! A valid stream (built from arbitrary data) with a few structured edits,
//! checked against libbz2 as in `decompress_differential`. Reaches the block
//! parser, Huffman tables, selectors and CRC checks past the header that raw
//! bytes rarely get through.

use arbitrary::Arbitrary;
use bz2ngrs_fuzz::oracle::{
    all_decoders, check_against_reference, compress, first_block_crc_bit, flip_bit, stream_crc_bit,
};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum Edit {
    FlipBit(u32),
    SetByte(u32, u8),
    Truncate(u32),
    Insert(u32, Vec<u8>),
    Delete(u32, u8),
    BlockSizeDigit(u8),
    BlockCrc(u8),
    StreamCrc(u8),
}

#[derive(Debug, Arbitrary)]
struct Input {
    level: u8,
    repeat: u16,
    data: Vec<u8>,
    edits: Vec<Edit>,
}

fn apply(stream: &mut Vec<u8>, edit: &Edit) {
    let len = stream.len();
    match edit {
        Edit::FlipBit(at) if len > 0 => flip_bit(stream, *at as usize % (len * 8)),
        Edit::SetByte(at, value) if len > 0 => stream[*at as usize % len] = *value,
        Edit::Truncate(at) => stream.truncate(*at as usize % (len + 1)),
        Edit::Insert(at, bytes) => {
            let at = *at as usize % (len + 1);
            stream.splice(at..at, bytes.iter().copied());
        }
        Edit::Delete(at, count) if len > 0 => {
            let at = *at as usize % len;
            stream.drain(at..len.min(at + usize::from(*count)));
        }
        Edit::BlockSizeDigit(value) if len > 3 => stream[3] = *value,
        Edit::BlockCrc(bit) => {
            if let Some(start) = first_block_crc_bit(stream) {
                flip_bit(stream, start + usize::from(*bit) % 32);
            }
        }
        Edit::StreamCrc(bit) => {
            if let Some(start) = stream_crc_bit(stream) {
                flip_bit(stream, start + usize::from(*bit) % 32);
            }
        }
        _ => {}
    }
}

fuzz_target!(|input: Input| {
    let level = u32::from(input.level % 9) + 1;
    let times = 1 + usize::from(input.repeat) % 64;
    let data: Vec<u8> = input
        .data
        .iter()
        .copied()
        .cycle()
        .take((input.data.len() * times).min(1 << 20))
        .collect();
    let mut stream = compress(&data, level);
    for edit in input.edits.iter().take(8) {
        apply(&mut stream, edit);
    }
    let cap = 4 * data.len() + (1 << 20);
    for decoder in all_decoders() {
        check_against_reference(decoder, &stream, cap);
    }
});
