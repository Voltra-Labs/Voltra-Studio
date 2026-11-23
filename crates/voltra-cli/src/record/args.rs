//! The arguments of `voltra record`.

use std::path::PathBuf;

use voltra_core::{Fps, FrameSize};

use crate::render::{parse_fps, parse_size};

/// Arguments of `voltra record`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Where to write the Matroska file. `-` is standard output, which works
    /// because the file is written as a stream; only the declared duration is
    /// lost, since declaring it means going back.
    #[arg(short, long, value_name = "PATH")]
    pub(super) output: PathBuf,

    /// Canvas size, as `WIDTHxHEIGHT`.
    #[arg(long, default_value = "640x360", value_parser = parse_size)]
    pub(super) size: FrameSize,

    /// Frame rate, as `30` or as an exact ratio like `30000/1001`.
    #[arg(long, default_value = "30", value_parser = parse_fps)]
    pub(super) fps: Fps,

    /// How many seconds to record.
    #[arg(short = 't', long, default_value_t = 10)]
    pub(super) seconds: u64,

    /// Frequency of the tone that marks each second, in hertz.
    #[arg(long, default_value_t = 1_000.0)]
    pub(super) tone: f64,

    /// How to store the video: `raw` I420, or `h264` through x264, which needs
    /// a build with `--features ffmpeg`.
    #[arg(long, value_enum, default_value_t = VideoCodec::Raw)]
    pub(super) video: VideoCodec,

    /// H.264 bitrate ceiling in kilobits per second. OBS's default.
    #[arg(long, default_value_t = 6_000)]
    pub(super) bitrate: u32,

    /// How to store the audio: `pcm` float, or `aac`, which needs a build with
    /// `--features ffmpeg`.
    #[arg(long, value_enum, default_value_t = AudioCodec::Pcm)]
    pub(super) audio: AudioCodec,

    /// AAC bitrate in kilobits per second. OBS's default.
    #[arg(long, default_value_t = 128)]
    pub(super) audio_bitrate: u32,

    /// Report what the process holds — live heap, resident memory, queued
    /// packets — every this many seconds of recorded time, on standard error.
    /// 0 for no reports.
    #[arg(long, value_name = "SECONDS", default_value_t = 0)]
    pub(super) report_every: u64,
}

/// How `voltra record` stores video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum VideoCodec {
    /// Uncompressed I420 (`V_UNCOMPRESSED`).
    Raw,
    /// H.264 through x264 (`V_MPEG4/ISO/AVC`).
    H264,
}

/// How `voltra record` stores audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AudioCodec {
    /// 32-bit float PCM (`A_PCM/FLOAT/IEEE`).
    Pcm,
    /// AAC-LC (`A_AAC`).
    Aac,
}
