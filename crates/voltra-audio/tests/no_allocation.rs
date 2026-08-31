//! The exit criterion of phase 3, enforced rather than asserted.
//!
//! CLAUDE.md §4.3 forbids allocation in the audio callback. That is easy to
//! write in a document and easy to break in a refactor, so this test counts
//! allocations with a global allocator and requires the count to be exactly
//! zero across the mix call.
//!
//! # Why this test can be trusted
//!
//! A counting harness that counts nothing passes every time. So the first test
//! here is a **negative control**: it allocates on purpose inside the measured
//! window and requires the counter to notice. If that ever stops failing to be
//! zero, every other test in this file has become worthless and this one says
//! so.
//!
//! # Why the counter is per thread
//!
//! The allocator is process-wide but `cargo test` runs tests in parallel, so a
//! process-wide counter measures every other test as well as the one under
//! test. The first version of this file did exactly that and reported the mixer
//! allocating one to seven times per run — all of it other threads. Counting in
//! a thread-local fixes it properly; requiring `--test-threads=1` would have
//! been a trap for whoever next runs `cargo test --workspace`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use voltra_audio::{ChannelLayout, Gain, MixInput, Mixer, SampleRate};
use voltra_core::{AudioBuffer, Timestamp};

thread_local! {
    /// Allocations made by *this* thread while it was watching.
    ///
    /// Const-initialised so that reading it never itself allocates, which would
    /// be an amusing way to make the measurement infinite.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    /// Whether this thread is currently counting.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Record one allocation, if this thread is watching.
///
/// `try_with` rather than `with`: during thread teardown the thread-local is
/// already destroyed, and a deallocation at that point must not panic.
fn note_allocation() {
    let _ = WATCHING.try_with(|watching| {
        if watching.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

/// A pass-through allocator that counts while watching.
struct Counting;

// SAFETY: `GlobalAlloc` requires that every method behave exactly as the
// underlying allocator does, and that memory handed to `dealloc` came from a
// matching `alloc` with the same layout. This type delegates every operation to
// `System` unchanged and adds only an atomic increment, so:
//
// 1. Every pointer returned is one `System` returned, valid for the requested
//    layout and suitably aligned.
// 2. Every pointer passed to `dealloc`/`realloc` is forwarded to `System` with
//    the same layout it was allocated with, because this type never rewrites
//    layouts and never allocates from anywhere else.
// 3. The counter is a const-initialised thread-local `Cell` on separate
//    storage, so it cannot alias or disturb allocator state, and re-entrancy is
//    impossible because a const-initialised thread-local never allocates on
//    first access.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` is forwarded unchanged from our caller, which the
        // `GlobalAlloc` contract already requires to be valid and non-zero.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this same `layout`, per
        // the `GlobalAlloc` contract our caller upholds.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: as `dealloc`, plus `new_size` forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `body` with this thread's allocations counted, and report how many.
///
/// Everything is per thread, so tests run in parallel without measuring each
/// other. The count is taken as a delta rather than reset, so nesting would
/// still report sensibly.
fn allocations_during<R>(body: impl FnOnce() -> R) -> (R, usize) {
    let before = ALLOCATIONS.with(Cell::get);
    WATCHING.with(|watching| watching.set(true));
    let result = body();
    WATCHING.with(|watching| watching.set(false));
    let after = ALLOCATIONS.with(Cell::get);
    (result, after - before)
}

const RATE: SampleRate = SampleRate::HZ_48000;
const FRAMES: usize = 1024;

fn buffer(value: f32) -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(RATE, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap();
    buffer.as_mut_slice().fill(value);
    buffer
}

/// The negative control. If this ever passes with a zero count, the harness has
/// stopped measuring and every other test here is meaningless.
#[test]
fn the_counter_notices_a_deliberate_allocation() {
    let (value, count) = allocations_during(|| {
        let noise: Vec<f32> = vec![0.0; 16];
        noise.len()
    });
    assert_eq!(value, 16);
    assert!(
        count > 0,
        "the allocation counter saw nothing; this harness is not measuring anything"
    );
}

/// The exit criterion of phase 3.
#[test]
fn mixing_allocates_nothing() {
    let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, 8).unwrap();

    // Control plane: setting up may allocate, and does.
    let mut ids = Vec::with_capacity(8);
    let mut faders = Vec::with_capacity(8);
    for _ in 0..8 {
        let (id, fader) = mixer.add_track().unwrap();
        ids.push(id);
        faders.push(fader);
    }

    let source = buffer(0.1);
    let inputs: Vec<MixInput<'_>> = ids.iter().map(|id| MixInput::new(*id, &source)).collect();
    let mut out = buffer(0.0);

    // Settle every ramp first, so the steady path is measured too.
    mixer.mix(&inputs, &mut out).unwrap();

    let ((), count) = allocations_during(|| {
        for _ in 0..64 {
            mixer.mix(&inputs, &mut out).unwrap();
        }
    });

    assert_eq!(count, 0, "mixing allocated {count} times");
}

/// The ramp is the branch with the most arithmetic in it, so it gets its own
/// measurement rather than being covered by luck.
#[test]
fn mixing_allocates_nothing_while_ramping() {
    let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, 4).unwrap();
    let (id, fader) = mixer.add_track().unwrap();

    let source = buffer(0.5);
    let inputs = [MixInput::new(id, &source)];
    let mut out = buffer(0.0);

    let ((), count) = allocations_during(|| {
        for step in 0..32 {
            // Move the fader every block, so no block finds a settled gain.
            fader.set_gain(if step % 2 == 0 {
                Gain::SILENT
            } else {
                Gain::UNITY
            });
            mixer.mix(&inputs, &mut out).unwrap();
        }
    });

    assert_eq!(count, 0, "ramping allocated {count} times");
}

/// Muted tracks, unknown identifiers and rejected inputs all take different
/// branches through `mix`, and none of them may allocate either — including the
/// error paths, which is where a `format!` is easiest to leave behind.
#[test]
fn the_awkward_paths_allocate_nothing() {
    let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, 4).unwrap();
    let (present, fader) = mixer.add_track().unwrap();
    let (_absent, _) = mixer.add_track().unwrap();
    fader.set_muted(true);

    let source = buffer(0.3);
    let wrong_rate = AudioBuffer::new(
        SampleRate::HZ_44100,
        ChannelLayout::Stereo,
        FRAMES,
        Timestamp::ZERO,
    )
    .unwrap();

    let inputs = [
        MixInput::new(present, &source),
        MixInput::new(voltra_audio::TrackId::from_raw(4242), &source),
        MixInput::new(voltra_audio::TrackId::from_raw(1), &wrong_rate),
    ];
    let mut out = buffer(0.0);

    // Settle the mute ramp.
    mixer.mix(&inputs, &mut out).unwrap();

    let ((), count) = allocations_during(|| {
        for _ in 0..32 {
            mixer.mix(&inputs, &mut out).unwrap();
        }
    });

    assert_eq!(count, 0, "the awkward paths allocated {count} times");
}

/// The interface moves faders from its own thread while audio runs. Neither
/// side may allocate, and neither may block — this exercises both at once.
#[test]
fn control_from_another_thread_allocates_nothing_in_the_mixer() {
    let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, 4).unwrap();
    let (id, fader) = mixer.add_track().unwrap();

    let source = buffer(0.4);
    let inputs = [MixInput::new(id, &source)];
    let mut out = buffer(0.0);
    mixer.mix(&inputs, &mut out).unwrap();

    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stopper = std::sync::Arc::clone(&stop);
    let far = fader.clone();
    let interface = std::thread::spawn(move || {
        let mut tick = 0u32;
        while !stopper.load(Ordering::Relaxed) {
            far.set_gain(Gain::from_db(
                -(f32::from(u16::try_from(tick % 40).unwrap())),
            ));
            far.set_muted(tick % 7 == 0);
            tick = tick.wrapping_add(1);
        }
    });

    let ((), count) = allocations_during(|| {
        for _ in 0..256 {
            mixer.mix(&inputs, &mut out).unwrap();
        }
    });

    stop.store(true, Ordering::Relaxed);
    interface.join().unwrap();

    assert_eq!(
        count, 0,
        "mixing under concurrent control allocated {count} times"
    );
}

/// Nothing in this crate may hold a lock: the audio thread cannot wait on
/// anything. There is no runtime assertion for that, so this checks the source
/// itself — crude, but it fails loudly the day somebody reaches for a `Mutex`.
#[test]
fn the_crate_contains_no_locks() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();

    for entry in std::fs::read_dir(&root).expect("the source directory") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a source file");
        for forbidden in ["Mutex", "RwLock", "Condvar"] {
            // Skip the prose: only code should be searched, and every mention in
            // this crate's comments is about not using them.
            let in_code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(forbidden));
            if in_code {
                offenders.push(format!("{} uses {forbidden}", path.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the audio path must not block: {offenders:?}"
    );
}
