//! A capture and a render loop at their own rates, for real, on two threads:
//! what is shown, dropped and repeated when the rates match and when they do
//! not.
//!
//! The scheduler of a CI machine is not real-time, so the bounds on how many
//! frames each loop manages are wide. What is exact is the accounting: every
//! captured frame was either shown or dropped, apart from at most one still
//! waiting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::time::{Duration, Instant};

use voltra_capture::{CaptureSource, CaptureStats, CaptureStatus, TestPatternCapture};
use voltra_core::{Fps, FrameSize, Source, TickContext, VideoSource};

/// Tick `source` at `fps` for `seconds`, paced by absolute deadlines as a
/// render loop is, then stop the capture and return what crossed.
fn render_loop(source: &mut CaptureSource, fps: Fps, seconds: u64) -> CaptureStats {
    let start = Instant::now();
    let ticks = seconds * u64::from(fps.num()) / u64::from(fps.den());
    for n in 0..ticks {
        source.tick(&TickContext::new(n, fps)).unwrap();
        if let Some(frame) = source.frame() {
            std::hint::black_box(frame.as_bytes()[0]);
        }
        let deadline = start + Duration::from_nanos(fps.pts(n + 1).as_nanos());
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    source.stop();
    assert_eq!(source.status(), CaptureStatus::Stopped);
    let stats = source.stats();
    assert_eq!(
        stats.shown + stats.dropped + stats.pending(),
        stats.captured,
        "every captured frame accounted for: {stats:?}"
    );
    assert!(stats.pending() <= 1, "{stats:?}");
    stats
}

fn size() -> FrameSize {
    FrameSize::new(320, 180).unwrap()
}

#[test]
fn at_matching_rates_almost_every_frame_is_shown() {
    let mut source = TestPatternCapture::start(size(), Fps::FPS_60).unwrap();
    let stats = render_loop(&mut source, Fps::FPS_60, 2);
    eprintln!("60 fps into 60 fps for 2 s: {stats:?}");
    assert!((100..=130).contains(&stats.captured), "{stats:?}");
    // Two free-running 60 fps clocks drift in and out of phase: a frame lands
    // just after a tick and the next just before the one after, so some are
    // dropped and as many ticks repeat. Most are still shown.
    assert!(
        stats.shown as f64 >= 0.75 * stats.captured as f64,
        "{stats:?}"
    );
}

#[test]
fn a_render_loop_at_half_the_rate_drops_about_half() {
    let mut source = TestPatternCapture::start(size(), Fps::FPS_60).unwrap();
    let stats = render_loop(&mut source, Fps::FPS_30, 2);
    eprintln!("60 fps into 30 fps for 2 s: {stats:?}");
    let dropped = stats.dropped as f64 / stats.captured as f64;
    assert!((0.35..=0.65).contains(&dropped), "{stats:?}");
}

#[test]
fn the_source_shows_the_capture_s_frames_at_its_size() {
    let mut source = TestPatternCapture::start(size(), Fps::FPS_60).unwrap();
    assert_eq!(source.kind(), "test_pattern_capture");
    assert!(source.frame().is_none() && source.size().is_none());
    let deadline = Instant::now() + Duration::from_secs(2);
    while source.frame().is_none() && Instant::now() < deadline {
        source.tick(&TickContext::new(0, Fps::FPS_60)).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(source.size(), Some(size()));
    assert_eq!(source.status(), CaptureStatus::Running);
    source.stop();
    source.stop();
    assert_eq!(source.status(), CaptureStatus::Stopped);
    // The last frame stays on show after the capture stopped.
    assert!(source.frame().is_some());
}
