//! End-to-end tests of `voltra record`, judged by `FFmpeg`.
//!
//! The recording carries sync marks: a white frame and the start of a tone at
//! every whole second. Decoding both with `FFmpeg` — a tool that knows nothing
//! about Voltra — and comparing where they land measures the audio/video sync
//! of the file itself. Skipped, and said so, when `FFmpeg` is not installed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::path::{Path, PathBuf};
use std::process::Command;

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("voltra-{}-{name}.mkv", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn have_ffmpeg() -> bool {
    let found = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|out| out.status.success())
    });
    if !found {
        eprintln!("skipping: ffmpeg and ffprobe are needed to judge the recording");
    }
    found
}

fn record(output: &Path, extra: &[&str]) -> std::process::Output {
    let out = Command::new(env!("CARGO_BIN_EXE_voltra"))
        .args(["record", "-o"])
        .arg(output)
        .args(["--size", "320x180", "--fps", "30"])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Decode one stream of `file` to raw bytes with `FFmpeg`.
fn decode(file: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(args)
        .arg("-")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn the_recording_is_what_ffmpeg_expects() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("record");
    record(&file.0, &["--seconds", "4"]);
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-show_entries"])
        .arg("stream=codec_name,width,height,nb_read_frames,sample_rate,channels:format=duration")
        .args(["-of", "compact"])
        .arg(&file.0)
        .output()
        .unwrap();
    let probe = String::from_utf8(probe.stdout).unwrap();
    assert!(
        probe.contains("codec_name=rawvideo|width=320|height=180|nb_read_frames=120"),
        "{probe}"
    );
    assert!(
        probe.contains("codec_name=pcm_f32le|sample_rate=48000|channels=2"),
        "{probe}"
    );
    assert!(probe.contains("duration=4.01"), "{probe}");
}

#[test]
fn audio_and_video_are_in_sync_in_the_file() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("sync");
    record(&file.0, &["--seconds", "6"]);

    // One luma byte per frame: the white sync frames stand out.
    let luma = decode(
        &file.0,
        &[
            "-map",
            "0:v",
            "-vf",
            "scale=1:1,format=gray",
            "-f",
            "rawvideo",
        ],
    );
    let flashes: Vec<f64> = luma
        .iter()
        .enumerate()
        .filter(|&(_, &value)| value > 200)
        .map(|(n, _)| n as f64 / 30.0)
        .collect();
    assert_eq!(flashes.len(), 6, "one flash per second: {flashes:?}");

    // Mono float samples: each tone starts after at least 10 ms of silence.
    let pcm = decode(&file.0, &["-map", "0:a", "-ac", "1", "-f", "f32le"]);
    let samples: Vec<f32> = pcm
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let mut onsets = Vec::new();
    let mut quiet = usize::MAX;
    for (index, &sample) in samples.iter().enumerate() {
        if sample.abs() > 0.02 {
            if quiet > 480 {
                onsets.push(index as f64 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
    }

    // Second 0 is skipped: the mixer fades a new track in over its first block,
    // so the first tone starts too softly to time. Every other second must
    // match within half a frame, and the offset must not grow. (The tone is a
    // sine from phase zero, so its first sample is exactly 0 and the onset
    // reads one sample, 20.8 µs, late.)
    let mut offsets = Vec::new();
    for (second, &flash) in flashes.iter().enumerate().skip(1) {
        let onset = onsets
            .iter()
            .copied()
            .find(|&t| (t - second as f64).abs() < 0.1)
            .unwrap_or_else(|| panic!("no tone near {second} s: {onsets:?}"));
        let offset = onset - flash;
        assert!(
            offset.abs() < 1.0 / 60.0,
            "second {second}: audio {onset} vs video {flash}"
        );
        offsets.push(offset);
    }
    let drift = offsets.last().unwrap() - offsets.first().unwrap();
    assert!(
        drift.abs() < 1e-4,
        "the offset grew by {drift} s over 4 s: {offsets:?}"
    );
    eprintln!("sync offsets (audio − video): {offsets:?}");
}

#[test]
fn a_recording_streams_to_standard_output() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("stdout");
    let out = record(Path::new("-"), &["--seconds", "2"]);
    std::fs::write(&file.0, &out.stdout).unwrap();
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "compact",
        ])
        .arg(&file.0)
        .output()
        .unwrap();
    let probe = String::from_utf8(probe.stdout).unwrap();
    assert!(probe.contains("nb_read_frames=60"), "{probe}");
}
