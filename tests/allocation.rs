//! `Decoder` and `Encoder` allocate on first use only, and move between
//! threads. Their own test binary, because it installs a counting global
//! allocator.

#![allow(unsafe_code, reason = "a counting GlobalAlloc has no safe form")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use bz2ngrs::{Compression, Decoder, Encoder};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to `System` unchanged; the counter is the only
// addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        // SAFETY: the caller's contract is `System`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        // SAFETY: as in `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        // SAFETY: as in `dealloc`, and the caller's contract for `new_size`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations_during<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = ALLOCS.load(Relaxed);
    let r = f();
    (ALLOCS.load(Relaxed) - before, r)
}

fn assert_send<T: Send>() {}

// One test function: the counter is global, so tests running in parallel
// threads would count each other's allocations.
#[test]
fn allocates_on_first_use_only() {
    decoder_allocates_on_first_decode_only();
    encoder_allocates_on_first_compress_only();
}

fn encoder_allocates_on_first_compress_only() {
    assert_send::<Encoder>();

    // Not periodic, so that the whole sorter (its recursion included) runs.
    let mut x = 1u32;
    let data: Vec<u8> = (0..200_000)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            b"ACGT"[(x >> 29) as usize & 3]
        })
        .collect();
    let (n, mut encoder) = allocations_during(|| Encoder::new(Compression::BEST));
    assert_eq!(n, 0, "Encoder::new allocated");

    let mut out = Vec::with_capacity(1 << 20);
    let (n, ()) = allocations_during(|| encoder.compress(&data, &mut out));
    assert!(
        n > 0,
        "the first compress allocated nothing, so this test counts nothing"
    );
    let first = out.clone();
    // Once its memory has grown to an input, the encoder allocates nothing
    // for it or a shorter one; the caller's `out` has room.
    for input in [&data[..], &data[..1000], &[][..]] {
        out.clear();
        let (n, ()) = allocations_during(|| encoder.compress(input, &mut out));
        assert_eq!(n, 0, "a later compress of {} bytes allocated", input.len());
    }
    out.clear();
    encoder.compress(&data, &mut out);
    assert!(out == first, "a reused encoder wrote another stream");
}

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
