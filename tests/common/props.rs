//! The property bodies, one function per property, shared by the two surfaces
//! (`decompress` and `__decompress_at`) so that they cannot drift apart.

use bz2ngrs::Error;
use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};

use super::inputs;
use super::oracle::{
    Decoder, Reference, check_against_reference, check_exact, compress, first_block_crc_bit,
    flip_bit, reference, stream_crc_bit,
};

/// Output capacity for the hostile-input properties.
const HOSTILE_CAP: usize = 1 << 20;

/// A count in `0..bound` from the whole of `u32`.
fn below(tc: &TestCase, bound: usize) -> usize {
    tc.draw(gs::integers::<u32>()) as usize % bound
}

/// Property 1: Accept: data of a given shape, compressed at any level, decodes into an
/// `out` of exactly its size, byte for byte.
pub fn roundtrip(tc: &TestCase, decoders: &[Decoder], data: impl PrintableGenerator<Vec<u8>>) {
    let data = tc.draw(data);
    let level = tc.draw(inputs::level());
    let stream = compress(&data, level);
    for d in decoders {
        check_exact(*d, &stream, &data);
    }
}

/// Property 1: Accept, around block boundaries: the data is sized from the level.
pub fn roundtrip_boundary(tc: &TestCase, decoders: &[Decoder]) {
    let case = tc.draw(inputs::block_boundary());
    let stream = compress(&case.data, case.level);
    for d in decoders {
        check_exact(*d, &stream, &case.data);
    }
}

/// Property 1: Accept, streams made of many ragged blocks.
pub fn roundtrip_pieces(tc: &TestCase, decoders: &[Decoder]) {
    let pieces = tc.draw(inputs::pieces());
    let level = tc.draw(inputs::level());
    let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    let stream = super::oracle::compress_pieces(&refs, level);
    let data = pieces.concat();
    for d in decoders {
        check_exact(*d, &stream, &data);
    }
}

/// Property 2: A larger `out` still gives `Ok(len)` and the same prefix. What lies past
/// `len` is not tested: see the documentation of `bz2ngrs::decompress`, which
/// (as of writing) only says the contents of `out` are unspecified on error.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn larger_out(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let extra = 1 + below(tc, 70_000);
    for d in decoders {
        let mut out = vec![0xA5; v.data.len() + extra];
        match d.run(&v.stream, &mut out) {
            Ok(n) => {
                assert_eq!(n, v.data.len(), "{}: length", d.name());
                assert!(out[..n] == v.data[..], "{}: prefix differs", d.name());
            }
            Err(e) => panic!("{}: rejected with {e:?}", d.name()),
        }
    }
}

/// Property 3: An `out` shorter than the data by `k >= 1` gives `Err(OutputTooSmall)`.
pub fn short_out(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    tc.assume(!v.data.is_empty());
    let k = tc.draw(gs::integers::<usize>().min_value(1).max_value(v.data.len()));
    for d in decoders {
        let mut out = vec![0xA5; v.data.len() - k];
        assert_eq!(
            d.run(&v.stream, &mut out),
            Err(Error::OutputTooSmall),
            "{}",
            d.name()
        );
    }
}

/// Property 4: Bytes after the end-of-stream marker (arbitrary ones, another stream, a
/// header) change nothing.
pub fn trailing_bytes(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let trailing = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => tc.draw(gs::binary().max_size(3000)),
        1 => tc.draw(inputs::valid_stream()).stream,
        _ => {
            let mut t = tc.draw(inputs::hostile_bzh());
            t.truncate(4);
            t
        }
    };
    let mut stream = v.stream.clone();
    stream.extend_from_slice(&trailing);
    for d in decoders {
        check_exact(*d, &stream, &v.data);
    }
}

/// Property 5: Reject, differential: mutated streams.
pub fn mutated_agrees(tc: &TestCase, decoders: &[Decoder]) {
    let m = tc.draw(inputs::mutated_stream());
    let cap = tc.draw(inputs::output_cap(m.original.data.len()));
    let kinds = m.mutations.iter().map(|x| format!("{x:?}"));
    for kind in kinds.map(|k| k.split([' ', '(']).next().map(str::to_owned)) {
        tc.event(kind.unwrap_or_default());
    }
    for d in decoders {
        tc.event(check_against_reference(*d, &m.stream, cap));
    }
}

/// Property 5: A proper prefix of a valid stream is `Err(UnexpectedEof)` (and that is
/// what libbz2 says about it, which the second assertion checks).
pub fn truncated(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let prefix = &v.stream[..below(tc, v.stream.len())];
    assert!(
        matches!(
            reference(prefix, v.data.len()),
            Reference::Err(std::io::ErrorKind::UnexpectedEof)
        ),
        "premise: libbz2 reports a truncated stream as UnexpectedEof"
    );
    for d in decoders {
        let mut out = vec![0xA5; v.data.len()];
        assert_eq!(
            d.run(prefix, &mut out),
            Err(Error::UnexpectedEof),
            "{}",
            d.name()
        );
    }
}

/// Property 5: A flipped bit in the first block's stored CRC is `Err(CrcMismatch)`.
pub fn corrupt_block_crc(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let bit = below(tc, 32);
    let mut stream = v.stream.clone();
    let start = first_block_crc_bit(&stream);
    tc.assume(start.is_some());
    flip_bit(&mut stream, start.unwrap_or_default() + bit);
    crc_mismatch(decoders, &stream, v.data.len());
}

/// Property 5: A flipped bit in the combined stream CRC is `Err(CrcMismatch)`.
pub fn corrupt_stream_crc(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let bit = below(tc, 32);
    let mut stream = v.stream.clone();
    let start = stream_crc_bit(&stream);
    assert!(
        start.is_some(),
        "premise: the end-of-stream marker is found"
    );
    flip_bit(&mut stream, start.unwrap_or_default() + bit);
    crc_mismatch(decoders, &stream, v.data.len());
}

fn crc_mismatch(decoders: &[Decoder], stream: &[u8], out_len: usize) {
    assert!(
        matches!(
            reference(stream, out_len),
            Reference::Err(std::io::ErrorKind::InvalidInput)
        ),
        "premise: libbz2 rejects the corrupted CRC"
    );
    for d in decoders {
        let mut out = vec![0xA5; out_len];
        assert_eq!(
            d.run(stream, &mut out),
            Err(Error::CrcMismatch),
            "{}",
            d.name()
        );
    }
}

/// Property 6: Hostile bytes: same differential rule as for mutated streams.
pub fn hostile(tc: &TestCase, decoders: &[Decoder], bytes: impl PrintableGenerator<Vec<u8>>) {
    let bytes = tc.draw(bytes);
    for d in decoders {
        tc.event(check_against_reference(*d, &bytes, HOSTILE_CAP));
    }
}

/// Extra: whatever state a call leaves behind (scratch buffers), a failed
/// decode does not make the next, valid one fail or differ.
pub fn failure_leaves_no_state(tc: &TestCase, decoders: &[Decoder]) {
    let m = tc.draw(inputs::mutated_stream());
    let v = tc.draw(inputs::valid_stream());
    for d in decoders {
        let mut out = vec![0xA5; m.original.data.len()];
        let _ = d.run(&m.stream, &mut out);
        check_exact(*d, &v.stream, &v.data);
    }
}
