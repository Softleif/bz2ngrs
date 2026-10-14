//! `Decoder` allocates on first use only, and moves between threads. Its own
//! test binary, because it installs a counting global allocator.

#![allow(unsafe_code, reason = "a counting GlobalAlloc has no safe form")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use bz2ngrs::Decoder;

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
