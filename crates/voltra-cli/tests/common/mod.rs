//! What the recording tests share: temporary files, `FFmpeg` as the judge,
//! and finding the sync marks — white frames and tone onsets — in a file.

#![allow(
    dead_code,
    reason = "each test binary includes this module and uses part of it"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TempFile(pub PathBuf);

impl TempFile {
    pub fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("voltra-{}-{name}.mkv", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn have_ffmpeg() -> bool {
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

/// Run `voltra record` into `output` with the test size and rate, plus
/// `extra`, and require it to succeed.
pub fn record(output: &Path, extra: &[&str]) -> std::process::Output {
    voltra(output, &["--size", "320x180", "--fps", "30"], extra)
}

/// Run `voltra record -o output`, then `base` and `extra`.
pub fn voltra(output: &Path, base: &[&str], extra: &[&str]) -> std::process::Output {
    let out = Command::new(env!("CARGO_BIN_EXE_voltra"))
        .args(["record", "-o"])
        .arg(output)
        .args(base)
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
pub fn frame_times(file: &Path, kind: &str) -> Vec<f64> {
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
pub fn decode(file: &Path, args: &[&str]) -> Vec<u8> {
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

/// The presentation time of every white frame, as `FFmpeg` decodes them.
///
/// Real timestamps, not indices: a stream shifted as a whole must show up. One
/// luma byte per decoded frame, frames passed through as they are — `FFmpeg`
/// would otherwise duplicate frames to fill a timeline that starts with the
/// audio's priming — paired with each frame's own time.
pub fn flash_times(file: &Path) -> Vec<f64> {
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
    luma.iter()
        .zip(&times)
        .filter(|&(&value, _)| value > 200)
        .map(|(_, &time)| time)
        .collect()
}

/// The time each tone starts, after at least 10 ms of silence, in mono float
/// samples counted from the first decoded one, which is at the audio's first
/// frame time. Read as `FFmpeg` decodes, not collected: an hour of audio is
/// 690 MB of samples.
pub fn tone_onsets(file: &Path) -> Vec<f64> {
    use std::io::Read;

    let audio_start = frame_times(file, "a")[0];
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-map", "0:a", "-ac", "1", "-f", "f32le", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut pcm = std::io::BufReader::with_capacity(1 << 16, child.stdout.take().unwrap());
    let mut onsets = Vec::new();
    let mut quiet = usize::MAX;
    let mut bytes = [0u8; 4];
    let mut index = 0usize;
    while pcm.read_exact(&mut bytes).is_ok() {
        let sample = f32::from_le_bytes(bytes);
        if sample.abs() > 0.02 {
            if quiet > 480 {
                onsets.push(audio_start + index as f64 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
        index += 1;
    }
    assert!(
        child.wait().unwrap().success(),
        "ffmpeg failed decoding audio"
    );
    onsets
}
