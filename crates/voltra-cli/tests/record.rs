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

/// The presentation time of every decoded frame of stream `kind` (`v`, `a`).
fn frame_times(file: &Path, kind: &str) -> Vec<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            kind,
            "-show_entries",
            "frame=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split(',').next()?.parse().ok())
        .collect()
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

/// Where each second's tone starts relative to its white frame, for seconds
/// 1 to 5 of a six-second recording, measured on what `FFmpeg` decodes. Fails
/// the test if any is off by half a frame or more, or the offset grows.
fn sync_offsets(file: &Path) -> Vec<f64> {
    // Real timestamps, not indices: a stream shifted as a whole must show up.
    // One luma byte per decoded frame, frames passed through as they are —
    // FFmpeg would otherwise duplicate frames to fill a timeline that starts
    // with the audio's priming — paired with each frame's own time.
    let times = frame_times(file, "v");
    let luma = decode(
        file,
        &[
            "-map",
            "0:v",
            "-fps_mode",
            "passthrough",
            "-vf",
            "scale=1:1,format=gray",
            "-f",
            "rawvideo",
        ],
    );
    assert_eq!(luma.len(), times.len(), "one luma byte per decoded frame");
    let flashes: Vec<f64> = luma
        .iter()
        .zip(&times)
        .filter(|&(&value, _)| value > 200)
        .map(|(_, &time)| time)
        .collect();
    assert_eq!(flashes.len(), 6, "one flash per second: {flashes:?}");

    // Mono float samples from the first decoded one, which is at the audio's
    // first frame time: each tone starts after at least 10 ms of silence.
    let audio_start = frame_times(file, "a")[0];
    let pcm = decode(file, &["-map", "0:a", "-ac", "1", "-f", "f32le"]);
    let samples: Vec<f32> = pcm
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let mut onsets = Vec::new();
    let mut quiet = usize::MAX;
    for (index, &sample) in samples.iter().enumerate() {
        if sample.abs() > 0.02 {
            if quiet > 480 {
                onsets.push(audio_start + index as f64 / 48_000.0);
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
    offsets
}

#[test]
fn audio_and_video_are_in_sync_in_the_file() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("sync");
    record(&file.0, &["--seconds", "6"]);
    let offsets = sync_offsets(&file.0);
    eprintln!("sync offsets (audio − video): {offsets:?}");
}

/// The same measurement on a compressed recording: H.264 keeps the white frames
/// white enough to find. And the bitrate asked for is a ceiling the video stays
/// under — x264 with its VBV capped at the bitrate and no filler spends less
/// when the scene is simple, which is what a recording wants.
#[cfg(feature = "ffmpeg")]
#[test]
fn an_h264_recording_is_in_sync_and_within_its_bitrate() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("sync-h264");
    record(
        &file.0,
        &["--seconds", "6", "--video", "h264", "--bitrate", "1000"],
    );
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-show_entries"])
        .arg("stream=codec_name,profile,nb_read_frames")
        .args(["-of", "compact"])
        .arg(&file.0)
        .output()
        .unwrap();
    let probe = String::from_utf8(probe.stdout).unwrap();
    assert!(
        probe.contains("codec_name=h264|profile=High|nb_read_frames=180"),
        "{probe}"
    );

    let offsets = sync_offsets(&file.0);
    eprintln!("h264 sync offsets (audio − video): {offsets:?}");

    // 282 blocks of 1024 stereo float samples are 2_310_144 bytes of audio;
    // the rest is video and a few kilobytes of container.
    let total = std::fs::metadata(&file.0).unwrap().len();
    let video_bits = (total - 2_310_144) * 8;
    let ceiling = 1_000_000 * 6;
    eprintln!(
        "h264 video: {} kbit/s against a 1000 kbit/s ceiling",
        video_bits / 6_000
    );
    assert!(video_bits <= ceiling * 11 / 10, "{video_bits} bits in 6 s");
}

/// H.264 and AAC: two encoders, each delivering late by its own delay, through
/// the interleaver. AAC's first packet is its priming, a frame before the
/// audio; the track's `CodecDelay` makes the decoder discard it. If either
/// half were wrong, the tones would land 21 ms off their flashes.
#[cfg(feature = "ffmpeg")]
#[test]
fn an_h264_and_aac_recording_is_in_sync() {
    if !have_ffmpeg() {
        return;
    }
    let file = TempFile::new("sync-aac");
    record(
        &file.0,
        &["--seconds", "6", "--video", "h264", "--audio", "aac"],
    );
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,profile,initial_padding",
        ])
        .args(["-of", "compact"])
        .arg(&file.0)
        .output()
        .unwrap();
    let probe = String::from_utf8(probe.stdout).unwrap();
    assert!(
        probe.contains("codec_name=aac|profile=LC|initial_padding=1024"),
        "{probe}"
    );
    let offsets = sync_offsets(&file.0);
    eprintln!("h264 + aac sync offsets (audio − video): {offsets:?}");
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
