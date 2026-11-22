//! What the Matroska writer produces, read back by `FFmpeg`.
//!
//! `FFmpeg` is the independent oracle: what it reads is what players read. These
//! tests skip, and say so, when `ffprobe` or `ffmpeg` are not installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::process::Command;

use voltra_core::{
    AudioBuffer, ChannelLayout, Fps, FrameSize, PixelFormat, SampleRate, Timestamp, VideoFrame,
};
use voltra_output::{MatroskaWriter, TrackSpec};

fn have(tool: &str) -> bool {
    let found = Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("skipping: {tool} is not installed");
    }
    found
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("voltra-{}-{name}.mkv", std::process::id()))
}

const SIZE: (u32, u32) = (64, 48);
const RATE: SampleRate = SampleRate::HZ_48000;

/// Write `seconds` of interleaved video and audio, each frame a different grey.
fn record(path: &PathBuf, seconds: u64) {
    let size = FrameSize::new(SIZE.0, SIZE.1).unwrap();
    let fps = Fps::FPS_30;
    let tracks = [
        TrackSpec::RawVideo { size, fps },
        TrackSpec::PcmAudio {
            rate: RATE,
            layout: ChannelLayout::Stereo,
        },
    ];
    let sink = BufWriter::new(File::create(path).unwrap());
    let mut writer = MatroskaWriter::new(sink, &tracks).unwrap();
    let mut frame = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).unwrap();
    let mut block = AudioBuffer::new(RATE, ChannelLayout::Stereo, 1024, Timestamp::ZERO).unwrap();
    block.as_mut_slice().fill(0.25);

    let frames = 30 * seconds;
    let blocks = (48_000 * seconds).div_ceil(1024);
    let (mut n, mut k) = (0u64, 0u64);
    while n < frames || k < blocks {
        let video_next = (n < frames).then(|| fps.pts(n));
        let audio_next = (k < blocks).then(|| RATE.pts(k * 1024));
        if video_next.is_some() && (audio_next.is_none() || video_next <= audio_next) {
            #[allow(clippy::cast_possible_truncation)]
            frame.plane_mut(0).unwrap().fill((n * 7 % 200) as u8 + 16);
            writer.write_video(0, &frame, fps.pts(n)).unwrap();
            n += 1;
        } else {
            block.set_pts(RATE.pts(k * 1024));
            writer.write_audio(1, &block).unwrap();
            k += 1;
        }
    }
    writer.finish_with_duration().unwrap();
}

fn probe(path: &PathBuf, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            entries,
            "-of",
            "compact",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn ffmpeg_reads_and_decodes_what_is_written() {
    if !have("ffprobe") || !have("ffmpeg") {
        return;
    }
    let path = scratch("whole");
    record(&path, 1);

    let streams = probe(
        &path,
        "stream=codec_name,pix_fmt,width,height,nb_read_frames,sample_rate,channels",
    );
    assert!(
        streams.contains("codec_name=rawvideo|width=64|height=48|pix_fmt=yuv420p")
            && streams.contains("nb_read_frames=30"),
        "{streams}"
    );
    assert!(
        streams.contains("codec_name=pcm_f32le|sample_rate=48000|channels=2"),
        "{streams}"
    );
    let format = probe(&path, "format=duration");
    // 47 blocks of 1024 at 48 kHz: 1.002667 s, the longest track.
    assert!(format.contains("duration=1.00"), "{format}");

    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(decode.status.success());
    assert!(
        decode.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&decode.stderr)
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_cut_short_still_plays_up_to_the_cut() {
    if !have("ffprobe") {
        return;
    }
    let path = scratch("cut");
    record(&path, 3);
    let bytes = std::fs::read(&path).unwrap();
    // Cut at 60 %: mid-block, as a crash would.
    let cut = scratch("cut-short");
    std::fs::write(&cut, &bytes[..bytes.len() * 6 / 10]).unwrap();

    let streams = probe(&cut, "stream=codec_name,nb_read_frames");
    let video: u64 = streams
        .lines()
        .find(|line| line.contains("rawvideo"))
        .and_then(|line| line.split("nb_read_frames=").nth(1))
        .and_then(|count| count.trim().parse().ok())
        .unwrap();
    // 90 frames in all; most of the first 60 % survives.
    assert!(
        (45..90).contains(&video),
        "{video} frames read from the cut file"
    );
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&cut).ok();
}
