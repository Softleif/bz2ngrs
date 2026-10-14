//! The property bodies, one function per property, shared by the two surfaces
//! (`Decoder::new()` and a decoder at each level) so that they cannot drift apart.
#![allow(
    clippy::cast_possible_truncation,
    reason = "block indices and 32-bit CRC fields of test-sized streams"
)]

use bz2ngrs::{Corruption, Error, Position};
use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};

use super::forge::{self, Broken, Rule, Spec, block_crc, forge, naive_block};
use super::inputs;
use super::oracle::{
    Decoder, Reference, block_starts, check_against_reference, check_exact, check_outcome,
    compress, compress_pieces, read_bits, reference, stream_crc_bit, write_bits,
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
/// past `len` are untouched (a documented promise of `Decoder::decompress`).
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn larger_out(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    // Slack of a few bytes (the run fills overshoot by up to 288) or a lot.
    let extra = 1 + if tc.draw(gs::booleans()) {
        below(tc, 300)
    } else {
        below(tc, 70_000)
    };
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
    let k = if tc.draw(gs::booleans()) {
        1 + below(tc, v.data.len().min(8))
    } else {
        tc.draw(gs::integers::<usize>().min_value(1).max_value(v.data.len()))
    };
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

fn corruption(rule: Rule) -> Corruption {
    match rule {
        Rule::OrigPtr => Corruption::OrigPtr,
        Rule::BlockTooLong => Corruption::BlockTooLong,
        Rule::BlockMagic => Corruption::BlockMagic,
        Rule::NoSymbols => Corruption::NoSymbols,
        Rule::GroupCount => Corruption::GroupCount,
        Rule::SelectorCount => Corruption::SelectorCount,
        Rule::Selector => Corruption::Selector,
        Rule::CodeLength => Corruption::CodeLength,
        Rule::HuffmanCode => Corruption::HuffmanCode,
        Rule::NoEndOfBlock => Corruption::NoEndOfBlock,
        Rule::RunTooLong => Corruption::RunTooLong,
    }
}

/// Property 7: Reject, forged streams whose last block breaks one format rule
/// (`origPtr` not below the row count, more rows than the digit allows, a
/// defect in the block header or symbols) are `Err(Corrupt)` of the matching
/// kind, in that block, as libbz2's data error. The earlier blocks are valid
/// (the property discards the cases where they are not), so the block index
/// is the one the spec put the defect in. The bit is at or just after the
/// offending field: no earlier than where the field starts, and no later than
/// 21 bits (the most libbz2 reads of a Huffman code) past where it ends.
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn forged_is_corrupt(
    tc: &TestCase,
    decoders: &[Decoder],
    broken: impl PrintableGenerator<Broken>,
) {
    let Broken { spec, rule } = tc.draw(broken);
    let want = corruption(rule);
    let f = forge(&spec);
    tc.assume(f.applied);
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
        f.expected.is_none() && matches!(libbz2, Reference::Err(std::io::ErrorKind::InvalidInput)),
        "premise: the naive reference and libbz2 reject ({})",
        describe(&libbz2)
    );
    let (lo, hi) = f
        .field
        .map_or((f.last_block_bit, u64::MAX), |(a, b)| (a, b + 21));
    let hi = hi.min(f.stream.len() as u64 * 8);
    for d in decoders {
        let mut out = vec![0xA5; f.cap];
        match d.run(&f.stream, &mut out) {
            Err(Error::Corrupt { kind, at }) => {
                assert_eq!(kind, want, "{}", d.name());
                assert_eq!(at.block, bad_block, "{}", d.name());
                assert!(
                    (lo..=hi).contains(&at.bit),
                    "{}: bit {} outside {lo}..={hi}",
                    d.name(),
                    at.bit
                );
            }
            other => panic!("{}: expected Corrupt({want:?}), got {other:?}", d.name()),
        }
    }
}

/// Property 7: Accept, a block with more selectors than libbz2 keeps (it
/// reads up to 32767 and ignores those past 18002) decodes like the same block
/// without them.
pub fn forged_excess_selectors(tc: &TestCase, decoders: &[Decoder]) {
    let f = forge(&tc.draw(forge::excess_selectors()));
    tc.assume(f.expected.is_some());
    let want = f.expected.unwrap_or_default();
    assert!(
        matches!(reference(&f.stream, f.cap), Reference::Ok(ref got) if *got == want),
        "premise: libbz2 decodes the block"
    );
    for d in decoders {
        check_exact(*d, &f.stream, &want);
    }
}

/// Property 7: Reject, a defective stream cut off at a random point, around
/// the defect or anywhere from the defective block on: whether the cut falls
/// before the decoder can tell (`UnexpectedEof`) or after (the defect's own
/// error) is libbz2's call, by its error class.
pub fn forged_defect_cut_agrees(tc: &TestCase, decoders: &[Decoder]) {
    let b = tc.draw(forge::any_defective());
    let f = forge(&b.spec);
    let total = f.stream.len();
    let first = (f.last_block_bit / 8) as usize;
    let (lo, hi) = match f.field {
        Some((a, e)) if tc.draw(gs::booleans()) => ((a / 8) as usize, (e / 8) as usize + 4),
        _ => (first, total),
    };
    let hi = hi.min(total);
    let cut = lo.min(hi) + below(tc, hi.saturating_sub(lo) + 1);
    for d in decoders {
        tc.event(check_against_reference(*d, &f.stream[..cut], f.cap));
    }
}

/// One call in a sequence for [`reuse_independent`].
#[derive(Debug, Clone)]
struct Step {
    what: &'static str,
    stream: Vec<u8>,
    cap: usize,
}

#[hegel::composite]
fn step(tc: &TestCase) -> Step {
    match tc.draw(gs::integers::<u8>().max_value(5)) {
        0 => {
            let v = tc.draw(inputs::valid_stream());
            Step {
                what: "valid, exact cap",
                cap: v.data.len(),
                stream: v.stream,
            }
        }
        1 => {
            let v = tc.draw(inputs::valid_stream());
            Step {
                what: "valid, any cap",
                cap: tc.draw(inputs::output_cap(v.data.len())),
                stream: v.stream,
            }
        }
        2 => {
            let m = tc.draw(inputs::mutated_stream());
            Step {
                what: "mutated",
                cap: tc.draw(inputs::output_cap(m.original.data.len())),
                stream: m.stream,
            }
        }
        3 => {
            let v = tc.draw(inputs::valid_stream());
            let cut = below(tc, v.stream.len());
            Step {
                what: "truncated",
                cap: v.data.len(),
                stream: v.stream[..cut].to_vec(),
            }
        }
        4 => {
            // A block that fills most of the scratch, to follow a small one
            // and to be followed by one.
            let seed = tc.draw(gs::integers::<u64>());
            let len = 150_000 + below(tc, 750_000);
            let data = inputs::mixed(seed, len, 1 + below(tc, 256));
            Step {
                what: "big block",
                cap: data.len(),
                stream: compress(&data, 9),
            }
        }
        _ => Step {
            what: "hostile bytes",
            stream: tc.draw(inputs::hostile_bzh()),
            cap: HOSTILE_CAP,
        },
    }
}

/// Property 8: Reuse independence. One long-lived decoder decodes a sequence
/// of streams (valid, mutated, truncated, hostile; big and small; with `out`
/// too small or not). Each result equals what a decoder made for that stream
/// gives (the documented contract: results never depend on earlier calls),
/// and agrees with the oracle (which keeps both from being wrong in the same
/// way).
pub fn reuse_independent(tc: &TestCase, decoders: &[Decoder]) {
    let steps = tc.draw(gs::vecs(step()).min_size(2).max_size(6));
    for s in &steps {
        tc.event(s.what);
    }
    let wants: Vec<Reference> = steps.iter().map(|s| reference(&s.stream, s.cap)).collect();
    for d in decoders {
        for (s, want) in steps.iter().zip(&wants) {
            let mut out = vec![0xA5; s.cap];
            let got = d.run(&s.stream, &mut out);
            let mut fresh_out = vec![0xA5; s.cap];
            let fresh = d.run_fresh(&s.stream, &mut fresh_out);
            assert_eq!(got, fresh, "{} ({}): reused vs fresh", d.name(), s.what);
            if let Ok(n) = got {
                assert!(
                    out[..n] == fresh_out[..n],
                    "{} ({}): bytes differ from a fresh decoder's",
                    d.name(),
                    s.what
                );
            }
            check_outcome(&d.name(), &s.stream, want, &got, &out, s.cap);
        }
    }
}

/// Property 9: `decompress_to_vec` with a size at or above the stream's
/// length is the oracle's bytes, and its length is the stream's length (the
/// vector is cut to what was written).
#[allow(clippy::panic, reason = "a failed property is a panic")]
pub fn to_vec_equals_oracle(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    let extra = if tc.draw(gs::booleans()) {
        0
    } else {
        below(tc, 70_000)
    };
    let size = v.data.len() + extra;
    assert!(
        matches!(reference(&v.stream, size), Reference::Ok(ref got) if *got == v.data),
        "premise: libbz2 decodes a valid stream to its data"
    );
    for d in decoders {
        let got = d.run_to_vec(&v.stream, size);
        assert_eq!(got.as_deref(), Ok(&v.data[..]), "{}: size {size}", d.name());
        // "The `Vec` keeps capacity `size` even when shorter."
        let capacity = got.map(|g| g.capacity());
        assert!(
            capacity.is_ok_and(|c| c >= size),
            "{}: {capacity:?}",
            d.name()
        );
    }
}

/// Property 9: `decompress_to_vec` with a size below the stream's length is
/// `OutputTooSmall` with `capacity: size`.
pub fn to_vec_too_small(tc: &TestCase, decoders: &[Decoder]) {
    let v = tc.draw(inputs::valid_stream());
    tc.assume(!v.data.is_empty());
    let k = if tc.draw(gs::booleans()) {
        1 + below(tc, v.data.len().min(8))
    } else {
        tc.draw(gs::integers::<usize>().min_value(1).max_value(v.data.len()))
    };
    let size = v.data.len() - k;
    for d in decoders {
        assert_eq!(
            d.run_to_vec(&v.stream, size),
            Err(Error::OutputTooSmall { capacity: size }),
            "{}",
            d.name()
        );
    }
}

/// Property 9: `decompress_to_vec` on mutated streams follows the same
/// differential rule as `decompress`.
pub fn to_vec_mutated_agrees(tc: &TestCase, decoders: &[Decoder]) {
    let m = tc.draw(inputs::mutated_stream());
    let size = tc.draw(inputs::output_cap(m.original.data.len()));
    let want = reference(&m.stream, size);
    for d in decoders {
        let got = d.run_to_vec(&m.stream, size);
        let n = got.as_ref().map(Vec::len).map_err(|e| *e);
        tc.event(check_outcome(
            &d.name(),
            &m.stream,
            &want,
            &n,
            got.as_deref().unwrap_or(&[]),
            size,
        ));
    }
}
