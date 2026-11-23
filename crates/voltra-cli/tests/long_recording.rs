//! The phase 4 exit criterion, as tests: minutes of recording without A/V
//! drift and without memory growth.
//!
//! Memory is read from `voltra record --report-every`, which prints what the
//! counting global allocator sees live — and the resident size — every so many
//! seconds of recorded time. Sync is measured in the file, as in `record.rs`,
//! but at every second of it and at 29.97 fps, where a clock that accumulates
//! rounding drifts.
//!
//! `VOLTRA_LONG_SECONDS` overrides the length of the encoded recording, to
//! measure an hour by hand with the same checks.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::collections::HashMap;
use std::process::{Command, Stdio};

mod common;

/// One `report` line: key → value.
type Report = HashMap<String, u64>;

fn reports(stderr: &[u8]) -> Vec<Report> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("report "))
        .map(|line| {
            line.split_whitespace()
                .filter_map(|pair| {
                    let (key, value) = pair.split_once('=')?;
                    Some((key.to_owned(), value.parse().ok()?))
                })
                .collect()
        })
        .collect()
}

/// How much `key` grew from the first report to the last.
fn growth(reports: &[Report], key: &str) -> i128 {
    let first = reports.first().unwrap()[key];
    let last = reports.last().unwrap()[key];
    i128::from(last) - i128::from(first)
}

/// Two minutes of raw recording, streamed to nowhere: after the first ten
/// seconds, the heap neither gains a byte nor makes a single allocation.
#[test]
fn a_raw_recording_holds_its_memory_still() {
    let out = Command::new(env!("CARGO_BIN_EXE_voltra"))
        .args([
            "record",
            "-o",
            "-",
            "--size",
            "160x90",
            "--fps",
            "30000/1001",
        ])
        .args(["--seconds", "120", "--report-every", "10"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reports = reports(&out.stderr);
    assert_eq!(reports.len(), 11, "a report every 10 s of 120");
    for report in &reports {
        for key in ["heap_bytes", "heap_blocks", "allocations"] {
            assert_eq!(
                report[key], reports[0][key],
                "{key} moved after the first report: {reports:?}"
            );
        }
    }
    #[cfg(target_os = "linux")]
    assert!(
        growth(&reports, "resident_kib") <= 1_024,
        "resident memory grew: {reports:?}"
    );
}

/// Minutes of H.264 and AAC at 29.97 fps, measured at every second.
///
/// - **Audio:** each tone starts at its second plus a constant — the sample of
///   phase zero, and what AAC moves the onset by. It must not move.
/// - **Video:** each white frame is the first frame at or after its second, at
///   `ceil(s × 30000/1001)` frames. Its time must be that frame's exact time,
///   within the half millisecond Matroska's timestamps round to. A clock that
///   accumulated per-frame rounding would be off by seconds within the hour.
/// - **Memory:** the heap's blocks do not change after the first report, and
///   its bytes grow only while buffers reach the largest packet so far.
#[cfg(feature = "ffmpeg")]
#[test]
fn a_long_recording_keeps_sync_and_memory() {
    if !common::have_ffmpeg() {
        return;
    }
    let seconds: u64 = std::env::var("VOLTRA_LONG_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(180);
    let file = common::TempFile::new("long");
    let length = seconds.to_string();
    let out = common::voltra(
        &file.0,
        &["--size", "160x90", "--fps", "30000/1001"],
        &[
            "--video",
            "h264",
            "--audio",
            "aac",
            "--seconds",
            &length,
            "--report-every",
            "10",
        ],
    );

    let reports = reports(&out.stderr);
    // At 29.97 fps the last frame ends just short of the last whole second.
    assert!(reports.len() as u64 + 1 >= seconds / 10, "{reports:?}");
    for report in &reports {
        assert_eq!(
            report["heap_blocks"], reports[0]["heap_blocks"],
            "{reports:?}"
        );
    }
    let bytes = growth(&reports, "heap_bytes");
    let allocations = growth(&reports, "allocations");
    let resident = growth(&reports, "resident_kib");
    eprintln!(
        "after the first report: heap +{bytes} B in +{allocations} reallocations, resident +{resident} KiB"
    );
    assert!(bytes <= 64 * 1024 && allocations <= 32, "{reports:?}");
    #[cfg(target_os = "linux")]
    assert!(resident <= 1_024, "{reports:?}");

    let flashes = common::flash_times(&file.0);
    assert_eq!(flashes.len() as u64, seconds, "one flash per second");
    let onsets = common::tone_onsets(&file.0);

    // Second 0 is skipped: the mixer fades a new track in over its first
    // block, so the first tone starts too softly to time.
    let mut audio = Vec::new();
    let mut worst_video: f64 = 0.0;
    for (second, &flash) in flashes.iter().enumerate().skip(1) {
        let s = second as u64;
        let frame = (s * 30_000).div_ceil(1_001);
        let expected = (frame * 1_001) as f64 / 30_000.0;
        let video = flash - expected;
        assert!(
            video.abs() <= 0.000_5 + 1e-9,
            "second {second}: white frame at {flash}, expected {expected}"
        );
        worst_video = worst_video.max(video.abs());

        let onset = onsets
            .iter()
            .copied()
            .find(|&t| (t - second as f64).abs() < 0.1)
            .unwrap_or_else(|| panic!("no tone near {second} s"));
        audio.push(onset - second as f64);
    }
    let (low, high) = audio.iter().fold((f64::MAX, f64::MIN), |(low, high), &e| {
        (low.min(e), high.max(e))
    });
    eprintln!(
        "{} s at 29.97 fps: tone {:.1}..{:.1} µs after its second, white frames within {:.1} µs",
        seconds,
        low * 1e6,
        high * 1e6,
        worst_video * 1e6
    );
    assert!(high.abs() < 1.0 / 60.0, "the tones are off: {high}");
    assert!(
        high - low < 50e-6,
        "the tones moved by {} µs over the recording",
        (high - low) * 1e6
    );
}
