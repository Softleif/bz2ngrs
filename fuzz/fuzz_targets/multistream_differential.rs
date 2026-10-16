#![no_main]

//! Arbitrary bytes as concatenated bzip2 streams, decoded by
//! `Decoder::decompress_all` and by a `DecompressReader` fed in pieces of a
//! drawn size, at every SIMD level, against the `bzip2` crate's
//! `MultiBzDecoder`. If the oracle decodes all of it within the cap, both
//! return the same bytes; if it errors, both error, with the same class of
//! error (input that ends early, or bad input). The reader's error is the
//! one `decompress_all` returns, and the bytes it returned before the error
//! agree with the oracle's as far as both go.

use std::io::{self, Read};

use bz2ngrs::{Decoder, DecompressReader, Error};
use bzip2::read::MultiBzDecoder;
use fearless_simd::Level;
use libfuzzer_sys::fuzz_target;

/// Hands out at most `step` bytes per read.
struct Trickle<'a> {
    src: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.src.len());
        buf[..n].copy_from_slice(&self.src[..n]);
        self.src = &self.src[n..];
        Ok(n)
    }
}

fn levels() -> Vec<Level> {
    let best = Level::new();
    #[allow(unused_mut, reason = "only x86 has levels below the best one")]
    let mut v = vec![Level::fallback(), best];
    #[cfg(target_arch = "x86_64")]
    {
        v.extend(best.as_sse2().map(Level::Sse2));
        v.extend(best.as_sse4_2().map(Level::Sse4_2));
        v.extend(best.as_avx2().map(Level::Avx2));
    }
    v
}

fuzz_target!(|input: (u16, &[u8])| {
    let (step, data) = input;
    let step = 1 + usize::from(step);
    let cap = 4 * data.len() + (1 << 20);

    let mut want = Vec::new();
    let oracle = MultiBzDecoder::new(data)
        .take(cap as u64 + 1)
        .read_to_end(&mut want);
    if oracle.is_ok() && want.len() > cap {
        return;
    }
    for level in levels() {
        let all = Decoder::__with_level(level).decompress_all_to_vec(data, cap);
        let mut got = Vec::new();
        let read = DecompressReader::__with_level(Trickle { src: data, step }, level)
            .read_to_end(&mut got);
        match (&oracle, &all) {
            (Ok(_), Ok(v)) => assert!(*v == want, "{level:?}: decompress_all bytes"),
            (Err(e), Err(ours)) => assert_eq!(
                e.kind() == io::ErrorKind::UnexpectedEof,
                matches!(ours, Error::UnexpectedEof { .. }),
                "{level:?}: oracle {e}, ours {ours}"
            ),
            _ => panic!("{level:?}: oracle {oracle:?}, decompress_all {all:?}"),
        }
        match (&all, &read) {
            (Ok(_), Ok(_)) => assert!(got == want, "{level:?}: reader bytes"),
            (Err(ours), Err(e)) => {
                assert_eq!(
                    e.get_ref().and_then(|e| e.downcast_ref::<Error>()),
                    Some(ours)
                );
                // The oracle drops what it decoded in the read that fails.
                assert!(
                    want.starts_with(&got) || got.starts_with(&want),
                    "{level:?}: reader bytes before the error"
                );
            }
            _ => panic!("{level:?}: decompress_all {all:?}, reader {read:?}"),
        }
    }
});
