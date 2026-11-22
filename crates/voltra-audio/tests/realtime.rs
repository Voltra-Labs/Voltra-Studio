//! The phase 3 exit criterion, as tests: mixing neither allocates nor blocks.
//!
//! The allocation check wraps the system allocator and counts every call made
//! **on the current thread** while a thread-local flag is armed. Tests in this
//! binary run on several threads at once, so both the flag and the count are
//! thread-local: with a shared count, two armed tests would read each other's
//! allocations into their own totals. It also means only the arming thread is watched — enough while
//! `Mixer::mix` spawns no work elsewhere. If it ever does (say, `rayon` over
//! channels), this test has to grow with it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::atomic::AtomicU64;
use voltra_audio::{
    Balance, Gain, InputSpec, Meter, MixSpec, Mixer, PeakMode, PullOutcome, Resampler,
    TrackControl, audio_input,
};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

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
    let mut music_meter = mixer.take_track_meter(music.id()).unwrap();
    let mut voice_meter = mixer.take_track_meter(voice.id()).unwrap();

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
            std::hint::black_box((music_meter.take(), voice_meter.take()));
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

    // A third thread reads both tracks' meters as fast as it can.
    let mut meters = (
        mixer.take_track_meter(a.id()).unwrap(),
        mixer.take_track_meter(b.id()).unwrap(),
    );
    let reader = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut frames = 0u64;
            while !stop.load(Ordering::Relaxed) {
                frames += meters.0.take().input.frames();
                std::hint::black_box(meters.1.take());
            }
            frames + meters.0.take().input.frames()
        })
    };

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
    // Every mixed block's duration reached the reader exactly once.
    assert_eq!(reader.join().unwrap(), 10_000 * FRAMES as u64);
}

#[test]
fn metering_never_allocates() {
    for mode in [PeakMode::Sample, PeakMode::True] {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, mode);
        let stereo = block(ChannelLayout::Stereo, 0.25);
        let mono = block(ChannelLayout::Mono, 0.5);
        // Fewer blocks than the mixer test: true peak is slow in debug builds,
        // and one allocation per block would show in the first.
        let calls = allocations_during(|| {
            for _ in 0..1_000 {
                meter.process(&stereo);
                meter.process(&mono);
                std::hint::black_box(reader.take());
            }
        });
        assert_eq!(calls, 0, "metering allocated in {mode:?}");
    }
}

#[test]
fn resampling_never_allocates() {
    for (from, to) in [
        (SampleRate::HZ_44100, SampleRate::HZ_48000),
        (SampleRate::HZ_48000, SampleRate::HZ_44100),
    ] {
        let mut resampler = Resampler::new(from, to, ChannelLayout::Stereo, FRAMES).unwrap();
        let mut input =
            AudioBuffer::new(from, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap();
        input.as_mut_slice().fill(0.25);
        let mut output = AudioBuffer::new(
            to,
            ChannelLayout::Stereo,
            resampler.max_output_frames(FRAMES),
            Timestamp::ZERO,
        )
        .unwrap();
        let mut consumed = 0u64;
        let calls = allocations_during(|| {
            // Includes the shrink-and-grow of the output between blocks of
            // 1114 and 1115 samples, which must stay within its capacity.
            for _ in 0..200 {
                input.set_pts(from.pts(consumed));
                resampler.process(&input, &mut output).unwrap();
                consumed += FRAMES as u64;
            }
        });
        assert_eq!(calls, 0, "resampling {from} to {to} allocated");
        assert_eq!(resampler.discontinuities(), 0);
    }
}

/// Ten seconds of a 1 kHz tone from a 44.1 kHz source, in blocks of varying
/// size with timestamps jittered by up to 5 ms, pushed from one thread and
/// mixed at 48 kHz in another with 85 ms of latency. It must come out whole and
/// clean, and neither thread may allocate.
///
/// Time is simulated so the test is deterministic: the mix thread publishes
/// how far the clock has reached (its tick plus the latency), the source only
/// delivers blocks that would have been captured by then, and the mix waits for
/// it to catch up before each tick.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, two threads; splitting it would hide the timing between them"
)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "sample indices and counts far below the limits of f64 and usize"
)]
fn a_jittery_source_reaches_the_mix_whole_and_clean() {
    const SOURCE: SampleRate = SampleRate::HZ_44100;
    const MIX: SampleRate = SampleRate::HZ_48000;
    const TONE: f64 = 1_000.0;
    const SECONDS: u64 = 10;
    // An hour into the monotonic clock, exact at both rates.
    const EPOCH_S: u64 = 3_600;
    let latency_ns: u64 = 4 * MIX.pts(1024).as_nanos();

    let spec = MixSpec::new(MIX, ChannelLayout::Mono, FRAMES).unwrap();
    let (mut writer, mut reader) =
        audio_input(InputSpec::new(SOURCE, ChannelLayout::Mono, 1_323, spec)).unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let delivered = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let source = {
        let (now, delivered, done) = (Arc::clone(&now), Arc::clone(&delivered), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut blocks: Vec<AudioBuffer> = [441usize, 882, 1_323, 600, 1_000, 777]
                .iter()
                .map(|&len| {
                    AudioBuffer::new(SOURCE, ChannelLayout::Mono, len, Timestamp::ZERO).unwrap()
                })
                .collect();
            let total = u64::from(SOURCE.hz()) * SECONDS;
            let mut next = 0u64;
            let mut lcg = 0x2545_f491_u64;
            let calls = allocations_during(|| {
                let mut which = 0;
                while next < total {
                    let block = &mut blocks[which % 6];
                    let len = block.frames() as u64;
                    let first = u64::from(SOURCE.hz()) * EPOCH_S + next;
                    let end_ns = SOURCE.pts(first + len).as_nanos();
                    // Deliver only what would have been captured by now.
                    while end_ns > now.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                    for (k, sample) in block.channel_mut(0).unwrap().iter_mut().enumerate() {
                        let n = (first + k as u64) as f64;
                        #[allow(clippy::cast_possible_truncation)]
                        let value =
                            (std::f64::consts::TAU * TONE * n / f64::from(SOURCE.hz())).sin();
                        *sample = value as f32;
                    }
                    // Jitter of up to ±5 ms on the claimed time — except the
                    // first block, whose time anchors the whole stream: an
                    // offset there would shift everything after it, which is
                    // correct behaviour but not what this test measures.
                    lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let jitter = if next == 0 {
                        5_000_000
                    } else {
                        (lcg >> 33) % 10_000_001
                    };
                    let claimed = (SOURCE.pts(first).as_nanos() + jitter).saturating_sub(5_000_000);
                    block.set_pts(Timestamp::from_nanos(claimed));
                    writer.push(block).unwrap();
                    delivered.store(end_ns, Ordering::Release);
                    next += len;
                    which += 1;
                }
            });
            done.store(true, Ordering::Release);
            (calls, writer.stats())
        })
    };

    let mut mixer = Mixer::new(spec).unwrap();
    let track = mixer.add_track(ChannelLayout::Mono).unwrap();
    let mut block = spec.buffer(Timestamp::ZERO).unwrap();
    let mut out = spec.buffer(Timestamp::ZERO).unwrap();
    let ticks = (u64::from(MIX.hz()) * SECONDS) / FRAMES as u64 - 8;
    let mut stream = Vec::with_capacity(ticks as usize * FRAMES);
    let mut outcomes = Vec::with_capacity(ticks as usize);
    let calls = allocations_during(|| {
        for tick in 0..ticks {
            let first = u64::from(MIX.hz()) * EPOCH_S + tick * FRAMES as u64;
            let start = MIX.pts(first);
            let horizon = MIX.pts(first + FRAMES as u64).as_nanos() + latency_ns;
            now.store(horizon, Ordering::Release);
            while delivered.load(Ordering::Acquire) + latency_ns / 2 < horizon
                && !done.load(Ordering::Acquire)
            {
                std::hint::spin_loop();
            }
            out.set_pts(start);
            let outcome = reader.pull(start, &mut block).unwrap();
            outcomes.push(outcome);
            if outcome == PullOutcome::Empty {
                mixer.mix(std::iter::empty(), &mut out).unwrap();
            } else {
                mixer.mix([(track.id(), &block)], &mut out).unwrap();
            }
            stream.extend_from_slice(out.channel(0).unwrap());
        }
    });
    now.store(u64::MAX, Ordering::Release);
    let (source_calls, source_stats) = source.join().unwrap();

    assert_eq!(calls, 0, "the mix thread allocated");
    assert_eq!(source_calls, 0, "the source thread allocated");

    // Past the first ticks (the resampler's look-ahead and the track's fade-in),
    // every tick is whole and the stream matches the tone at 48 kHz.
    let stats = reader.stats();
    assert!(
        outcomes[2..].iter().all(|&o| o == PullOutcome::Full),
        "{stats:?}"
    );
    assert_eq!(
        (
            source_stats.gaps,
            source_stats.overlap_samples,
            source_stats.discontinuities
        ),
        (0, 0, 0)
    );
    assert!(
        source_stats.max_jitter_ns > 4_000_000,
        "the jitter was real"
    );

    let skip = 4 * FRAMES;
    let mut error = 0.0f64;
    let mut power = 0.0f64;
    for (k, &sample) in stream.iter().enumerate().skip(skip) {
        let n = (u64::from(MIX.hz()) * EPOCH_S + k as u64) as f64;
        let ideal = (std::f64::consts::TAU * TONE * n / f64::from(MIX.hz())).sin();
        error += (f64::from(sample) - ideal).powi(2);
        power += ideal * ideal;
    }
    let error_db = 10.0 * (error / power).log10();
    assert!(error_db < -100.0, "error {error_db:.1} dB");
    eprintln!(
        "pipeline: error {error_db:.1} dB, min headroom {} samples",
        stats.min_headroom
    );
}
