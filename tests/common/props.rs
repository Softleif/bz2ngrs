//! The property bodies, one function per property, shared by the two surfaces
//! (`decompress` and `__decompress_at`) so that they cannot drift apart.
#![allow(
    clippy::cast_possible_truncation,
    reason = "block indices and 32-bit CRC fields of test-sized streams"
)]

use bz2ngrs::{Corruption, Error, Position};
use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};

use super::forge::{Spec, block_crc, forge, naive_block};
use super::inputs;
use super::oracle::{
    Decoder, Reference, block_starts, check_against_reference, check_exact, compress,
    compress_pieces, read_bits, reference, stream_crc_bit, write_bits,
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

/// Property 2: A larger `out` still gives `Ok(len)` and the same prefix, and the bytes
/// past `len` are untouched (a documented promise of `bz2ngrs::decompress`).
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
                assert!(
                    out[n..].iter().all(|&b| b == 0xA5),
                    "{}: wrote past the returned length",
                    d.name()
                );
            }
            Err(e) => panic!("{}: rejected with {e:?}", d.name()),
        }
    }
}

/// Property 3: An `out` shorter than the data by `k >= 1` gives
/// `Err(OutputTooSmall { capacity })` with `capacity` the length of `out`.
pub fn short_out(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    tc.assume(!v.data.is_empty());
    let k = tc.draw(gs::integers::<usize>().min_value(1).max_value(v.data.len()));
    for d in decoders {
        let capacity = v.data.len() - k;
        let mut out = vec![0xA5; capacity];
        assert_eq!(
            d.run(&v.stream, &mut out),
            Err(Error::OutputTooSmall { capacity }),
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

/// Property 5: A proper prefix of a valid stream is `UnexpectedEof` at the
/// prefix's end, in the block the cut falls in (and that is what libbz2 says
/// about it, which the first assertion checks). The block is worked out from
/// the bit offsets of the block magics and the end-of-stream marker: block `i`
/// begins at the `i`th magic, and the end-of-stream marker is block `n`.
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
    let end = stream_crc_bit(&v.stream);
    assert!(end.is_some(), "premise: the end-of-stream marker is found");
    let mut marks = block_starts(&v.stream);
    marks.extend(end.map(|crc| crc - 48));
    let cut = prefix.len() * 8;
    let block = marks.iter().rposition(|&m| m <= cut).unwrap_or(0) as u32;
    for d in decoders {
        let mut out = vec![0xA5; v.data.len()];
        assert_eq!(
            d.run(prefix, &mut out),
            Err(Error::UnexpectedEof {
                at: Position {
                    block,
                    bit: cut as u64
                }
            }),
            "{}",
            d.name()
        );
    }
}

/// A stream of one block per non-empty piece, and where its blocks begin.
struct Blocks {
    pieces: Vec<Vec<u8>>,
    stream: Vec<u8>,
    starts: Vec<usize>,
}

fn multi_block(tc: &TestCase) -> Blocks {
    let pieces: Vec<Vec<u8>> = tc
        .draw(inputs::pieces())
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    tc.assume(!pieces.is_empty());
    let level = tc.draw(inputs::level());
    let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    let stream = compress_pieces(&refs, level);
    let starts = block_starts(&stream);
    assert_eq!(
        starts.len(),
        pieces.len(),
        "premise: the encoder ends a block after each piece"
    );
    Blocks {
        pieces,
        stream,
        starts,
    }
}

/// A nonzero change to a CRC: one flipped bit, or anything.
fn crc_change(tc: &TestCase) -> u32 {
    if tc.draw(gs::booleans()) {
        1 << below(tc, 32)
    } else {
        tc.draw(gs::integers::<u32>().min_value(1))
    }
}

/// Property 5: Overwriting the stored CRC of block `k` gives `BlockCrc` for
/// block `k`, with the value written as `stored` and the CRC of the piece
/// that block holds, worked out here, as `computed`.
pub fn corrupt_block_crc(tc: &TestCase, decoders: &[Decoder]) {
    let b = multi_block(tc);
    let k = below(tc, b.pieces.len());
    let crc_bit = b.starts[k] + 48;
    let original = read_bits(&b.stream, crc_bit, 32) as u32;
    let computed = block_crc(&b.pieces[k]);
    assert_eq!(original, computed, "premise: the encoder stores the CRC");
    let stored = original ^ crc_change(tc);
    let mut stream = b.stream.clone();
    write_bits(&mut stream, crc_bit, 32, u64::from(stored));
    let total: usize = b.pieces.iter().map(Vec::len).sum();
    assert!(
        matches!(
            reference(&stream, total),
            Reference::Err(std::io::ErrorKind::InvalidInput)
        ),
        "premise: libbz2 rejects the corrupted CRC"
    );
    for d in decoders {
        let mut out = vec![0xA5; total];
        assert_eq!(
            d.run(&stream, &mut out),
            Err(Error::BlockCrc {
                block: k as u32,
                stored,
                computed
            }),
            "{}",
            d.name()
        );
    }
}

/// Property 5: Overwriting the combined stream CRC gives `StreamCrc` with
/// the value written as `stored`, and as `computed` the one the encoder wrote,
/// which equals the blocks' CRCs combined here.
pub fn corrupt_stream_crc(tc: &TestCase, decoders: &[Decoder]) {
    let b = multi_block(tc);
    let crc_bit = stream_crc_bit(&b.stream);
    assert!(
        crc_bit.is_some(),
        "premise: the end-of-stream marker is found"
    );
    let crc_bit = crc_bit.unwrap_or_default();
    let original = read_bits(&b.stream, crc_bit, 32) as u32;
    let combined = b
        .pieces
        .iter()
        .fold(0u32, |c, p| c.rotate_left(1) ^ block_crc(p));
    assert_eq!(original, combined, "premise: the encoder stores the CRC");
    let stored = original ^ crc_change(tc);
    let mut stream = b.stream.clone();
    write_bits(&mut stream, crc_bit, 32, u64::from(stored));
    let total: usize = b.pieces.iter().map(Vec::len).sum();
    assert!(
        matches!(
            reference(&stream, total),
            Reference::Err(std::io::ErrorKind::InvalidInput)
        ),
        "premise: libbz2 rejects the corrupted CRC"
    );
    for d in decoders {
        let mut out = vec![0xA5; total];
        assert_eq!(
            d.run(&stream, &mut out),
            Err(Error::StreamCrc {
                stored,
                computed: original
            }),
            "{}",
            d.name()
        );
    }
}

/// Property 5: A stream whose first four bytes are not `BZh1` to `BZh9` is
/// `NotBzip2` as soon as the first wrong byte is in, wherever the input is
/// cut after it.
pub fn not_bzip2(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let at = below(tc, 4);
    let mut stream = v.stream.clone();
    stream[at] = tc.draw(gs::integers::<u8>());
    tc.assume(stream[at] != v.stream[at]);
    tc.assume(!(at == 3 && (b'1'..=b'9').contains(&stream[at])));
    stream.truncate(at + 1 + below(tc, stream.len() - at));
    assert!(
        matches!(
            reference(&stream, v.data.len()),
            Reference::Err(std::io::ErrorKind::InvalidInput)
        ),
        "premise: libbz2 rejects the header"
    );
    for d in decoders {
        let mut out = vec![0xA5; v.data.len()];
        assert_eq!(
            d.run(&stream, &mut out),
            Err(Error::NotBzip2),
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

fn describe(r: &Reference) -> String {
    match r {
        Reference::Ok(bytes) => format!("{} bytes", bytes.len()),
        other => format!("{other:?}"),
    }
}

/// Property 7: Accept, differential on forged streams (valid framing and CRCs around an
/// arbitrary BWT column): the naive reference decoder, libbz2 and every decoder agree
/// on the bytes, or all reject.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn forged_agrees(tc: &TestCase, decoders: &[Decoder], spec: impl PrintableGenerator<Spec>) {
    let f = forge(&tc.draw(spec));
    tc.event(if f.expected.is_some() {
        "naive reference: valid"
    } else {
        "naive reference: invalid"
    });
    let libbz2 = reference(&f.stream, f.cap);
    match (&f.expected, &libbz2) {
        (Some(want), Reference::Ok(got)) if want == got => {}
        (None, Reference::Err(_)) => {}
        _ => panic!(
            "the naive reference ({}) and libbz2 ({}) disagree",
            f.expected
                .as_ref()
                .map_or("invalid".into(), |b| format!("{} bytes", b.len())),
            describe(&libbz2)
        ),
    }
    for d in decoders {
        match &f.expected {
            Some(want) => check_exact(*d, &f.stream, want),
            None => {
                check_against_reference(*d, &f.stream, f.cap);
            }
        }
    }
}

/// Property 7: Reject, forged streams whose last block is invalid in the block
/// header (`origPtr` not below the row count, more rows than the digit allows)
/// are `Err(Corrupt)` of the given kind, in that block, as libbz2's data
/// error. The earlier blocks are valid (the property discards the cases where
/// they are not), so the block index is the one the spec put the defect in.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn forged_is_corrupt(
    tc: &TestCase,
    decoders: &[Decoder],
    spec: impl PrintableGenerator<Spec>,
    want: Corruption,
) {
    let spec = tc.draw(spec);
    let f = forge(&spec);
    let bad_block = spec.blocks.len() as u32 - 1;
    // A forged block is not always valid (four equal bytes at its very end
    // are not), so the prefix is checked with the naive reference.
    tc.assume(
        spec.blocks[..spec.blocks.len() - 1]
            .iter()
            .all(|r| naive_block(&r.column(), r.orig_ptr as usize, r.randomised).1),
    );
    let libbz2 = reference(&f.stream, f.cap);
    assert!(
        f.expected.is_none() && matches!(libbz2, Reference::Err(_)),
        "premise: the naive reference and libbz2 reject ({})",
        describe(&libbz2)
    );
    for d in decoders {
        let mut out = vec![0xA5; f.cap];
        match d.run(&f.stream, &mut out) {
            Err(Error::Corrupt { kind, at }) => {
                assert_eq!(kind, want, "{}", d.name());
                assert_eq!(at.block, bad_block, "{}", d.name());
            }
            other => panic!("{}: expected Corrupt, got {other:?}", d.name()),
        }
    }
}
