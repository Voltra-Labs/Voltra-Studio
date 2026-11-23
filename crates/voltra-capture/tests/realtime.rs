//! Handing a frame over allocates nothing: neither the capture thread writing
//! and publishing it, nor the render thread taking it.
//!
//! The check wraps the system allocator and counts every call made **on the
//! current thread** while a thread-local flag is armed, as in voltra-audio's
//! test of the same name: tests run on several threads at once, and a shared
//! count would read the others' allocations into its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use voltra_capture::frame_channel;
use voltra_core::{FrameSize, PixelFormat, Timestamp};

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

impl Counting {
    fn note() {
        // `try_with` because the allocator also runs while thread-locals are
        // being torn down, when `with` would panic.
        if ARMED.try_with(Cell::get).unwrap_or(false) {
            let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
        }
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are this allocator's guarantees. The
// only addition is a thread-local counter increment, which neither allocates
// nor unwinds.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded verbatim; the caller upholds `alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::note();
        // SAFETY: `ptr` came from `System` through this allocator with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::note();
        // SAFETY: `ptr` came from `System` through this allocator with `layout`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocator calls made on this thread while running `work`.
fn allocations_during(work: impl FnOnce()) -> usize {
    let before = CALLS.with(Cell::get);
    ARMED.with(|armed| armed.set(true));
    work();
    ARMED.with(|armed| armed.set(false));
    CALLS.with(Cell::get) - before
}

#[test]
fn the_counter_sees_an_allocation() {
    // Without this, a broken counter would pass the real test vacuously.
    let calls = allocations_during(|| drop(std::hint::black_box(vec![0u8; 64])));
    assert!(calls >= 2, "expected alloc and dealloc, counted {calls}");
}

#[test]
fn writing_publishing_and_taking_a_frame_never_allocates() {
    let size = FrameSize::new(1920, 1080).unwrap();
    let format = PixelFormat::Bgra8;
    let (mut writer, mut reader) = frame_channel(format, size).unwrap();
    let calls = allocations_during(|| {
        for n in 0..100u64 {
            let frame = writer.frame_mut(format, size).unwrap();
            frame.as_bytes_mut()[0] = u8::try_from(n % 256).unwrap();
            writer.publish(Timestamp::from_millis(n));
            if n % 3 != 0 {
                reader.update();
            }
            let _ = std::hint::black_box(reader.frame());
            let _ = std::hint::black_box(reader.stats());
        }
    });
    assert_eq!(calls, 0, "the handover allocated");
    let stats = reader.stats();
    assert_eq!(stats.captured, 100);
    assert!(stats.dropped > 0 && stats.repeated == 0, "{stats:?}");
}
