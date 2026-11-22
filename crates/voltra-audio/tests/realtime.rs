//! The phase 3 exit criterion, as tests: mixing neither allocates nor blocks.
//!
//! The allocation check wraps the system allocator and counts every call made
//! **on the current thread** while a thread-local flag is armed. Tests in this
//! binary run on several threads at once; the flag keeps them from counting
//! each other. It also means only the arming thread is watched — enough while
//! `Mixer::mix` spawns no work elsewhere. If it ever does (say, `rayon` over
//! channels), this test has to grow with it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use voltra_audio::{Balance, Gain, MixSpec, Mixer, TrackControl};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static CALLS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

impl Counting {
    fn note() {
        // `try_with` because the allocator also runs while thread-locals are
        // being torn down, when `with` would panic.
        if ARMED.try_with(Cell::get).unwrap_or(false) {
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are this allocator's guarantees. The
// only addition is an atomic increment, which neither allocates nor unwinds.
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
    let before = CALLS.load(Ordering::Relaxed);
    ARMED.with(|armed| armed.set(true));
    work();
    ARMED.with(|armed| armed.set(false));
    CALLS.load(Ordering::Relaxed) - before
}

const FRAMES: usize = 1024;

fn spec() -> MixSpec {
    MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, FRAMES).unwrap()
}

fn block(layout: ChannelLayout, value: f32) -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(SampleRate::HZ_48000, layout, FRAMES, Timestamp::ZERO).unwrap();
    buffer.as_mut_slice().fill(value);
    buffer
}

#[test]
fn the_counter_sees_an_allocation() {
    // Without this, a broken counter would pass the real test vacuously.
    let calls = allocations_during(|| drop(std::hint::black_box(vec![0u8; 64])));
    assert!(calls >= 2, "expected alloc and dealloc, counted {calls}");
}

#[test]
fn mixing_never_allocates() {
    let mut mixer = Mixer::new(spec()).unwrap();
    let music = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let voice = mixer.add_track(ChannelLayout::Mono).unwrap();
    let flaky = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let absent = mixer.add_track(ChannelLayout::Stereo).unwrap();

    let stereo = block(ChannelLayout::Stereo, 0.25);
    let mono = block(ChannelLayout::Mono, 0.5);
    let mut poisoned = block(ChannelLayout::Stereo, 0.1);
    poisoned.channel_mut(1).unwrap()[100] = f32::NAN;
    let mut late = block(ChannelLayout::Stereo, 0.1);
    late.set_pts(SampleRate::HZ_48000.pts(1));
    let mut out = spec().buffer(Timestamp::ZERO).unwrap();

    let calls = allocations_during(|| {
        for index in 0..10_000u32 {
            // Exercise every path: ramps, mute fades, pan, NaN, misalignment,
            // unknown tracks, duplicates and absences.
            music.set_gain(if index % 2 == 0 {
                Gain::UNITY
            } else {
                Gain::SILENT
            });
            voice.set_balance(if index % 3 == 0 {
                Balance::LEFT
            } else {
                Balance::RIGHT
            });
            voice.set_muted(index % 5 == 0);
            let bad = if index % 2 == 0 { &poisoned } else { &late };
            let report = mixer
                .mix(
                    [
                        (music.id(), &stereo),
                        (voice.id(), &mono),
                        (flaky.id(), bad),
                        (music.id(), &stereo),
                    ],
                    &mut out,
                )
                .unwrap();
            std::hint::black_box(report);
        }
    });

    assert_eq!(calls, 0, "Mixer::mix allocated");
    assert_eq!(mixer.totals().blocks, 10_000);
    assert_eq!(flaky.faults(), 10_000);
    assert_eq!(mixer.totals().missing, 10_000, "{absent:?} never delivered");
}

#[test]
fn controls_from_another_thread_never_stall_the_mix() {
    let mut mixer = Mixer::new(spec()).unwrap();
    let a = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let b = mixer.add_track(ChannelLayout::Mono).unwrap();
    let stop = Arc::new(AtomicBool::new(false));

    let hammer = {
        let (a, b, stop): (TrackControl, TrackControl, _) =
            (a.clone(), b.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut tick = 0u32;
            while !stop.load(Ordering::Relaxed) {
                tick = tick.wrapping_add(1);
                #[allow(clippy::cast_precision_loss)]
                let position = (tick % 2001) as f32 / 1000.0 - 1.0;
                a.set_gain(Gain::new(f32::from(u8::try_from(tick % 11).unwrap())).unwrap());
                b.set_balance(Balance::new(position).unwrap());
                b.set_muted(tick % 7 == 0);
            }
        })
    };

    let stereo = block(ChannelLayout::Stereo, 0.5);
    let mono = block(ChannelLayout::Mono, 0.5);
    let mut out = spec().buffer(Timestamp::ZERO).unwrap();
    // Two inputs at 0.5, each at most Gain::MAX: nothing can legitimately
    // exceed this, and anything torn or uninitialised almost surely would.
    let ceiling = 2.0 * 0.5 * Gain::MAX.linear();
    for _ in 0..10_000 {
        mixer
            .mix([(a.id(), &stereo), (b.id(), &mono)], &mut out)
            .unwrap();
        assert!(
            out.as_slice()
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() <= ceiling)
        );
    }
    stop.store(true, Ordering::Relaxed);
    hammer.join().unwrap();
}
