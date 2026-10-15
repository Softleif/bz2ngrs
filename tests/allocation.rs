//! `Decoder` and `Encoder` allocate on first use only, and move between
//! threads. Their own test binary, because it installs a counting global
//! allocator.

#![allow(unsafe_code, reason = "a counting GlobalAlloc has no safe form")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;

use bz2ngrs::{Compression, Decoder, Encoder};

struct Counting;

thread_local! {
    // Per thread, so that the test harness's other threads, which may
    // allocate at any time, are not counted.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

// SAFETY: forwards every call to `System` unchanged; the counter is the only
// addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract is `System`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: as in `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: as in `dealloc`, and the caller's contract for `new_size`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations_during<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = allocs();
    let r = f();
    (allocs() - before, r)
}

fn assert_send<T: Send>() {}

#[test]
fn encoder_allocates_on_first_compress_only() {
    assert_send::<Encoder>();

    // Not periodic, so that the whole sorter (its recursion included) runs.
    let mut x = 1u32;
    let acgt = |len: usize, x: &mut u32| -> Vec<u8> {
        (0..len)
            .map(|_| {
                *x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                b"ACGT"[(*x >> 29) as usize & 3]
            })
            .collect()
    };
    let data = acgt(200_000, &mut x);
    // A pattern of 7 bytes: its blocks are exactly periodic.
    let periodic: Vec<u8> = b"ACGTTGA".iter().copied().cycle().take(150_000).collect();
    // Several blocks at level 1.
    let several = acgt(350_000, &mut x);

    for (compression, inputs) in [
        (
            Compression::BEST,
            vec![
                data.clone(),
                periodic.clone(),
                data[..1000].to_vec(),
                Vec::new(),
            ],
        ),
        (
            Compression::new(1).unwrap_or_default(),
            vec![
                several,
                periodic,
                data[..1000].to_vec(),
                data[..1].to_vec(),
                Vec::new(),
            ],
        ),
    ] {
        // What a new encoder writes for each input: a reused one must write
        // the same, whatever it compressed before.
        let want: Vec<Vec<u8>> = inputs
            .iter()
            .map(|d| Encoder::new(compression).compress_to_vec(d))
            .collect();

        let (n, mut encoder) = allocations_during(|| Encoder::new(compression));
        assert_eq!(n, 0, "Encoder::new allocated");
        let mut out = Vec::with_capacity(1 << 21);
        let (n, ()) = allocations_during(|| encoder.compress(&inputs[0], &mut out));
        assert!(
            n > 0,
            "the first compress allocated nothing, so this test counts nothing"
        );
        assert!(out == want[0], "the first compress wrote another stream");
        // Once its memory has grown to the first input, the encoder allocates
        // nothing for it, for the others (each no larger in blocks) or for
        // any of them again, in any order; the caller's `out` has room.
        for i in (0..inputs.len()).chain((0..inputs.len()).rev()) {
            out.clear();
            let (n, ()) = allocations_during(|| encoder.compress(&inputs[i], &mut out));
            assert_eq!(
                n,
                0,
                "a later compress of {} bytes allocated",
                inputs[i].len()
            );
            assert!(
                out == want[i],
                "a reused encoder wrote another stream for {} bytes",
                inputs[i].len()
            );
        }
    }
}

#[test]
fn decoder_allocates_on_first_decode_only() {
    assert_send::<Decoder>();

    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
    enc.write_all(&b"ACGT".repeat(1000)).expect("write to Vec");
    let stream = enc.finish().expect("finish into Vec");
    let mut out = vec![0u8; 4000];

    let (n, mut decoder) = allocations_during(Decoder::new);
    assert_eq!(n, 0, "Decoder::new allocated");
    let (n, default) = allocations_during(Decoder::default);
    assert_eq!(n, 0, "Decoder::default allocated");
    drop(default);

    let (n, r) = allocations_during(|| decoder.decompress(&stream, &mut out));
    assert_eq!(r, Ok(4000));
    assert!(
        n > 0,
        "the first decode allocated nothing, so this test counts nothing"
    );
    let (n, r) = allocations_during(|| decoder.decompress(&stream, &mut out));
    assert_eq!(r, Ok(4000));
    assert_eq!(n, 0, "the second decode allocated");

    // A decode that fails allocates nothing either.
    let (n, r) = allocations_during(|| decoder.decompress(b"not bzip2", &mut out));
    assert!(r.is_err());
    assert_eq!(n, 0, "a failed decode allocated");
}
