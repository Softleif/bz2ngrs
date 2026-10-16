//! The properties of `bz2ngrs::CompressWriter`, at `new` and at every SIMD
//! level: whatever the split of the input into `write` calls and whatever the
//! inner writer does, the stream is `Encoder::compress`'s over the whole
//! input (itself tested against libbz2 1.0.8 in `encoder.rs`), and libbz2
//! decodes it back to the input.

#![allow(clippy::unwrap_used, reason = "a failed property is a panic")]

mod common;

use std::io::{self, Write};

use bz2ngrs::CompressWriter;
use common::enc_oracle::{Encoder, all_encoders, check_decodes, compression};
use common::{enc_props, inputs};
use hegel::TestCase;
use hegel::generators::{self as gs};

fn writer<W: Write>(e: Encoder, inner: W, level: u32) -> CompressWriter<W> {
    match e {
        Encoder::New => CompressWriter::new(inner, compression(level)),
        Encoder::At(simd) => CompressWriter::__with_level(inner, compression(level), simd),
    }
}

/// Where to cut `len` bytes of input into `write` calls: single bytes at the
/// start, random positions, and positions at and around the block size of
/// `level` (and its multiples), so that splits fall on both sides of the
/// instant a block fills. Positions repeat or are 0, which are empty writes.
#[hegel::composite]
fn cuts(tc: &TestCase, len: usize, level: u32) -> Vec<usize> {
    let mut at: Vec<usize> = Vec::new();
    if tc.draw(gs::booleans()) {
        at.extend(0..len.min(64));
    }
    let max_cuts = tc.draw(gs::integers::<usize>().max_value(12));
    for _ in 0..max_cuts {
        let p = if tc.draw(gs::booleans()) {
            tc.draw(gs::integers::<u32>()) as usize % (len + 1)
        } else {
            let k = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
            let delta = tc.draw(gs::integers::<isize>().min_value(-24).max_value(24));
            (100_000 * level as usize * k - 19).saturating_add_signed(delta)
        };
        at.push(p.min(len));
    }
    at.sort_unstable();
    at
}

/// `data` cut at `at` (sorted): the pieces, empty ones included.
fn pieces<'a>(data: &'a [u8], at: &[usize]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut from = 0;
    for &p in at.iter().chain([&data.len()]) {
        out.push(&data[from..p]);
        from = p;
    }
    out
}

/// A writer that fails, accepts partially or accepts everything, as scripted.
#[derive(Default)]
struct Flaky {
    out: Vec<u8>,
    script: Vec<u8>,
    next: usize,
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let fate = self.script.get(self.next).copied().unwrap_or(255);
        self.next += 1;
        let take = |n: usize| buf.len().min(n);
        let n = match fate % 6 {
            0 => return Err(io::ErrorKind::WouldBlock.into()),
            1 => return Err(io::ErrorKind::Interrupted.into()),
            2 => return Err(io::Error::other("scripted")),
            3 => take(1 + usize::from(fate / 6)),
            4 => 0,
            _ => buf.len(),
        };
        self.out.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn is_prefix(got: &[u8], whole: &[u8]) -> bool {
    whole.starts_with(got)
}

/// Writes `data` in `at`'s pieces, flushing after the pieces `flushes` marks,
/// and checks that what has reached `inner` after a flush is a prefix of
/// `want`.
fn write_pieces(
    e: Encoder,
    data: &[u8],
    at: &[usize],
    flushes: &[bool],
    level: u32,
    want: &[u8],
) -> Vec<u8> {
    let mut w = writer(e, Vec::new(), level);
    for (i, piece) in pieces(data, at).into_iter().enumerate() {
        assert_eq!(w.write(piece).unwrap(), piece.len(), "{}", e.name());
        if flushes.get(i).copied().unwrap_or(false) {
            w.flush().unwrap();
            assert!(
                is_prefix(w.get_ref(), want),
                "{}: output after a flush is not a prefix of the stream",
                e.name()
            );
        }
    }
    w.finish().unwrap()
}

/// Property W1: any split of the input, with flushes between the writes,
/// gives `Encoder::compress`'s stream, which libbz2 decodes to the input.
fn split_matches_encoder(tc: &TestCase, data: Vec<u8>, level: u32) {
    let at = tc.draw(cuts(data.len(), level));
    let flushes = tc.draw(gs::vecs(gs::booleans()).max_size(16));
    for (i, e) in all_encoders().into_iter().enumerate() {
        let want = e.run(&data, level);
        let got = write_pieces(e, &data, &at, &flushes, level, &want);
        assert!(
            got == want,
            "{}: split {at:?} of {} bytes at level {level} differs from Encoder::compress",
            e.name(),
            data.len()
        );
        if i == 0 {
            check_decodes(&e.name(), &got, &data);
        }
    }
}

fn split_of(tc: &TestCase, data: impl gs::PrintableGenerator<Vec<u8>>) {
    let data = tc.draw(data);
    let level = tc.draw(inputs::level());
    split_matches_encoder(tc, data, level);
}

#[hegel::test]
fn split_arbitrary_bytes(tc: TestCase) {
    split_of(&tc, inputs::any_bytes());
}

#[hegel::test]
fn split_runs(tc: TestCase) {
    split_of(&tc, inputs::runs());
}

#[hegel::test]
fn split_periodic(tc: TestCase) {
    split_of(&tc, inputs::periodic());
}

#[hegel::test]
fn split_tiny_alphabet(tc: TestCase) {
    split_of(&tc, inputs::tiny_alphabet());
}

#[hegel::test(
    test_cases = common::pinned::cases(40),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn split_around_block_boundary(tc: TestCase) {
    let case = tc.draw(inputs::block_boundary());
    split_matches_encoder(&tc, case.data, case.level);
}

#[hegel::test(
    test_cases = common::pinned::cases(30),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn split_full_block_at_end(tc: TestCase) {
    let case = tc.draw(enc_props::full_at_end());
    split_matches_encoder(&tc, case.data, case.level);
}

/// The input ends in the byte that fills the first block, or within a few
/// bytes of it: one unit per byte, so the block is full exactly when the last
/// unit arrives. The writer cannot know it is the last until `finish`.
#[hegel::test(
    test_cases = common::pinned::cases(40),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn split_block_fills_on_the_last_byte(tc: TestCase) {
    let level = tc.draw(inputs::level());
    let seed = tc.draw(gs::integers::<u64>());
    let alphabet = tc.draw(gs::integers::<usize>().min_value(2).max_value(256));
    let delta = tc.draw(gs::integers::<isize>().min_value(-2).max_value(2));
    let mut data = inputs::mixed(
        seed,
        (100_000 * level as usize - 19).saturating_add_signed(delta),
        alphabet,
    );
    for i in 1..data.len() {
        if data[i] == data[i - 1] {
            data[i] = data[i].wrapping_add(1);
        }
    }
    data.push(data.last().map_or(0, |b| b.wrapping_add(1)));
    split_matches_encoder(&tc, data, level);
}

#[hegel::test(
    test_cases = common::pinned::cases(15),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn split_many_blocks(tc: TestCase) {
    let level = tc.draw(inputs::level());
    let data = tc.draw(enc_props::big_input(level));
    split_matches_encoder(&tc, data, level);
}

/// Property W2: whatever `inner` refuses, accepts in part or interrupts, a
/// retry of each failed call leaves a stream that is `Encoder::compress`'s,
/// and what `inner` holds is at every point a prefix of it.
#[hegel::test(
    test_cases = common::pinned::cases(300),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn failing_inner_writer_loses_nothing(tc: TestCase) {
    let level = tc.draw(inputs::level());
    let data = tc.draw(enc_props::big_input(level));
    let at = tc.draw(cuts(data.len(), level));
    let script = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(400));
    let finish_after_flush = tc.draw(gs::booleans());
    for e in all_encoders() {
        let want = e.run(&data, level);
        let mut flaky = Flaky {
            script: script.clone(),
            ..Flaky::default()
        };
        let mut w = writer(e, &mut flaky, level);
        for piece in pieces(&data, &at) {
            while w.write(piece).is_err() {}
        }
        // A healed writer must finish, whatever is still pending.
        if finish_after_flush {
            w.get_mut().script.clear();
        }
        let finished = w.finish().is_ok();
        assert!(
            is_prefix(&flaky.out, &want),
            "{}: bytes lost, duplicated or reordered after an inner error",
            e.name()
        );
        assert!(
            finished || !finish_after_flush,
            "{}: healed writer",
            e.name()
        );
        if finished {
            assert!(
                flaky.out == want,
                "{}: stream differs after retries",
                e.name()
            );
        }
    }
}

/// Property W3: `reset` ends the stream in the old writer and starts another
/// in the new one; each is `Encoder::compress`'s for its input, however the
/// writer was used before.
#[hegel::test(
    test_cases = common::pinned::cases(60),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn reset_gives_independent_streams(tc: TestCase) {
    let level = tc.draw(inputs::level());
    let a = tc.draw(enc_props::big_input(level));
    let b = tc.draw(enc_props::big_input(level));
    let at_a = tc.draw(cuts(a.len(), level));
    let at_b = tc.draw(cuts(b.len(), level));
    for e in all_encoders() {
        let mut w = writer(e, Vec::new(), level);
        for piece in pieces(&a, &at_a) {
            w.write_all(piece).unwrap();
        }
        let first = w.reset(Vec::new()).unwrap();
        for piece in pieces(&b, &at_b) {
            w.write_all(piece).unwrap();
        }
        let second = w.reset(Vec::new()).unwrap();
        let third = w.finish().unwrap();
        assert!(first == e.run(&a, level), "{}: first stream", e.name());
        assert!(second == e.run(&b, level), "{}: second stream", e.name());
        assert!(
            third == e.run(&[], level),
            "{}: empty third stream",
            e.name()
        );
        check_decodes(&e.name(), &first, &a);
        check_decodes(&e.name(), &second, &b);
    }
}

/// Property W4: a writer dropped without `finish` has written a complete
/// stream, the same one.
#[hegel::test(
    test_cases = common::pinned::cases(100),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn drop_ends_the_stream(tc: TestCase) {
    let level = tc.draw(inputs::level());
    let data = tc.draw(enc_props::big_input(level));
    let at = tc.draw(cuts(data.len(), level));
    for e in all_encoders() {
        let mut out = Vec::new();
        {
            let mut w = writer(e, &mut out, level);
            for piece in pieces(&data, &at) {
                w.write_all(piece).unwrap();
            }
        }
        assert!(out == e.run(&data, level), "{}: dropped writer", e.name());
        check_decodes(&e.name(), &out, &data);
    }
}

#[test]
fn accessors_and_debug() {
    let mut w = CompressWriter::new(Vec::new(), bz2ngrs::Compression::FAST);
    assert!(w.get_ref().is_empty());
    w.get_mut().push(7);
    w.write_all(b"abc").unwrap();
    assert!(format!("{w:?}").contains("CompressWriter"));
    let out = w.finish().unwrap();
    assert_eq!(out[0], 7);
    assert_eq!(&out[1..5], b"BZh1");
}
