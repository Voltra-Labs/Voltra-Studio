//! The `record` subcommand: the scene and a sound, together, in one Matroska
//! file.
//!
//! ```text
//! voltra record -o demo.mkv --seconds 10
//! voltra record -o - | ffplay -
//! ```
//!
//! Video is the demonstration scene, as `voltra render` composites it, in raw
//! I420. Audio is a mono tone through a track of the mixer into a 48 kHz stereo
//! mix, as float PCM. No encoder yet: this fixes what no encoder will change —
//! the container, the timestamps and the interleaving (plan 016).
//!
//! **Sync marks.** At every whole second the frame that starts there is white,
//! and the tone sounds for 50 ms from that second's exact sample. Everything
//! else is silence. A tool that has never heard of Voltra can then measure the
//! audio/video sync in the file, which is what the tests do with `FFmpeg`.
//!
//! **The audio is the clock** (CLAUDE.md §5). Block `k` is stamped
//! `rate.pts(k · 1024)` and frame `n` `fps.pts(n)`, both from their absolute
//! index; whichever comes first is written next.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use voltra_audio::{MixSpec, Mixer};
use voltra_core::{
    AudioBuffer, ChannelLayout, ColorRange, ColorSpace, ColorSpec, Fps, FrameSize, PixelFormat,
    SampleRate, ScaleFilter, TickContext, Timestamp, VideoFrame, rgb_to_yuv,
};
use voltra_encode::{Bitstream, EncodedPacket, EncoderInput, VideoEncoder, VideoSettings, h264};
use voltra_output::{MatroskaWriter, TrackSpec};
use voltra_render::{Compositor, CpuCompositor};

use crate::demo::DemoScene;
use crate::render::{parse_fps, parse_size};

const RATE: SampleRate = SampleRate::HZ_48000;
const BLOCK: usize = 1024;
/// How long each second's tone lasts.
const MARK: std::time::Duration = std::time::Duration::from_millis(50);

/// Arguments of `voltra record`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Where to write the Matroska file. `-` is standard output, which works
    /// because the file is written as a stream; only the declared duration is
    /// lost, since declaring it means going back.
    #[arg(short, long, value_name = "PATH")]
    output: PathBuf,

    /// Canvas size, as `WIDTHxHEIGHT`.
    #[arg(long, default_value = "640x360", value_parser = parse_size)]
    size: FrameSize,

    /// Frame rate, as `30` or as an exact ratio like `30000/1001`.
    #[arg(long, default_value = "30", value_parser = parse_fps)]
    fps: Fps,

    /// How many seconds to record.
    #[arg(short = 't', long, default_value_t = 10)]
    seconds: u64,

    /// Frequency of the tone that marks each second, in hertz.
    #[arg(long, default_value_t = 1_000.0)]
    tone: f64,

    /// How to store the video: `raw` I420, or `h264` through x264, which needs
    /// a build with `--features ffmpeg`.
    #[arg(long, value_enum, default_value_t = VideoCodec::Raw)]
    video: VideoCodec,

    /// H.264 bitrate in kilobits per second, constant. OBS's default.
    #[arg(long, default_value_t = 6_000)]
    bitrate: u32,
}

/// How `voltra record` stores video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum VideoCodec {
    /// Uncompressed I420 (`V_UNCOMPRESSED`).
    Raw,
    /// H.264 through x264 (`V_MPEG4/ISO/AVC`).
    H264,
}

/// Open the encoder the arguments ask for, if any.
fn open_encoder(args: &Args) -> Result<Option<Box<dyn VideoEncoder>>> {
    match args.video {
        VideoCodec::Raw => Ok(None),
        VideoCodec::H264 => {
            #[cfg(feature = "ffmpeg")]
            {
                let mut settings = VideoSettings::new(args.size, args.fps);
                settings.bitrate_kbps = args.bitrate;
                let encoder =
                    voltra_encode::FfmpegVideoEncoder::h264(settings).context("opening x264")?;
                Ok(Some(Box::new(encoder)))
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                let _ = VideoSettings::new;
                anyhow::bail!("this build has no H.264 encoder; rebuild with `--features ffmpeg`");
            }
        }
    }
}

/// The video track for the file: raw, or the encoder's with its `avcC`.
fn video_track(args: &Args, encoder: Option<&dyn VideoEncoder>) -> Result<TrackSpec> {
    let Some(encoder) = encoder else {
        return Ok(TrackSpec::RawVideo {
            size: args.size,
            fps: args.fps,
        });
    };
    // Matroska writes CodecPrivate up front. x264 knows its headers on opening;
    // an encoder that only learns them from its first packet needs the writer
    // to wait, which no encoder here requires yet.
    let headers = encoder
        .headers()
        .context("the encoder gave no headers before its first packet")?;
    let decoder_config = h264::decoder_config(headers).context("reading the encoder's headers")?;
    Ok(TrackSpec::H264 {
        size: args.size,
        fps: args.fps,
        decoder_config,
    })
}

/// Record and write the file.
///
/// # Errors
///
/// When the destination cannot be opened or written, or when compositing,
/// converting or mixing fails.
pub fn run(args: &Args) -> Result<()> {
    let mut encoder = open_encoder(args)?;
    let tracks = [
        video_track(args, encoder.as_deref())?,
        TrackSpec::PcmAudio {
            rate: RATE,
            layout: ChannelLayout::Stereo,
        },
    ];
    let started = Instant::now();
    let summary = if args.output.as_os_str() == "-" {
        let sink = std::io::stdout().lock();
        let writer = MatroskaWriter::new(sink, &tracks).context("writing the header")?;
        let (writer, summary) = record(writer, args, encoder.as_deref_mut())?;
        drop(writer.finish().context("flushing the recording")?);
        summary
    } else {
        let file = std::fs::File::create(&args.output)
            .with_context(|| format!("creating {}", args.output.display()))?;
        let sink = std::io::BufWriter::with_capacity(64 * 1024, file);
        let writer = MatroskaWriter::new(sink, &tracks).context("writing the header")?;
        let (writer, summary) = record(writer, args, encoder.as_deref_mut())?;
        writer
            .finish_with_duration()
            .context("finishing the recording")?;
        summary
    };
    let wall = started.elapsed();
    eprintln!(
        "recorded {} s: {} frames at {}, {} audio blocks at {RATE}, in {:.2} s",
        args.seconds,
        summary.frames,
        args.fps,
        summary.blocks,
        wall.as_secs_f64()
    );
    Ok(())
}

/// What a recording wrote.
#[derive(Debug, Clone, Copy)]
struct Summary {
    frames: u64,
    blocks: u64,
}

/// Interleave video and audio into `writer`, earliest first, and return it.
fn record<W: Write>(
    writer: MatroskaWriter<W>,
    args: &Args,
    encoder: Option<&mut (dyn VideoEncoder + 'static)>,
) -> Result<(MatroskaWriter<W>, Summary)> {
    match encoder {
        None => record_raw(writer, args),
        Some(encoder) => record_encoded(writer, args, encoder),
    }
}

/// Raw video: every frame is ready the moment it is made, so whichever of the
/// next frame and the next audio block comes first is written next.
fn record_raw<W: Write>(
    mut writer: MatroskaWriter<W>,
    args: &Args,
) -> Result<(MatroskaWriter<W>, Summary)> {
    let mut video = Video::new(args)?;
    let mut audio = Audio::new(args.tone)?;

    let frames = args.seconds * u64::from(args.fps.num()) / u64::from(args.fps.den());
    let blocks = (args.seconds * u64::from(RATE.hz())).div_ceil(BLOCK as u64);
    let (mut n, mut k) = (0u64, 0u64);
    while n < frames || k < blocks {
        let video_next = (n < frames).then(|| args.fps.pts(n));
        let audio_next = (k < blocks).then(|| RATE.pts(k * BLOCK as u64));
        // Video goes first on a tie: the frame at a second and the block that
        // contains that second's sample start at the same instant.
        let video_first = match (video_next, audio_next) {
            (Some(v), Some(a)) => v <= a,
            (Some(_), None) => true,
            _ => false,
        };
        if video_first {
            let frame = video.frame(n, args.fps)?;
            writer
                .write_video(0, frame, args.fps.pts(n))
                .context("writing a frame")?;
            n += 1;
        } else {
            let block = audio.block(k)?;
            writer.write_audio(1, block).context("writing audio")?;
            k += 1;
        }
    }
    Ok((
        writer,
        Summary {
            frames: n,
            blocks: k,
        },
    ))
}

/// Encoded video: packets come out of the encoder late, so the video leads.
/// Before each packet at `P`, every audio block at or before `P` is written;
/// then the packet. The rest of the audio follows the encoder's flush.
fn record_encoded<W: Write>(
    mut writer: MatroskaWriter<W>,
    args: &Args,
    encoder: &mut (dyn VideoEncoder + 'static),
) -> Result<(MatroskaWriter<W>, Summary)> {
    let mut video = Video::new(args)?;
    let mut audio = Audio::new(args.tone)?;
    let frames = args.seconds * u64::from(args.fps.num()) / u64::from(args.fps.den());
    let blocks = (args.seconds * u64::from(RATE.hz())).div_ceil(BLOCK as u64);
    let length_prefixed = encoder.info().bitstream == Bitstream::LengthPrefixed;
    let mut payload = Vec::new();
    let mut k = 0u64;
    let mut packets = 0u64;

    let mut sink = |packet: EncodedPacket<'_>| -> voltra_encode::Result<()> {
        let refuse = |error: anyhow::Error| voltra_encode::Error::Sink(format!("{error:#}"));
        while k < blocks && RATE.pts(k * BLOCK as u64) <= packet.pts {
            let block = audio.block(k).map_err(refuse)?;
            writer
                .write_audio(1, block)
                .map_err(|error| refuse(error.into()))?;
            k += 1;
        }
        let data = if length_prefixed {
            packet.data
        } else {
            h264::to_length_prefixed(packet.data, &mut payload);
            &payload
        };
        writer
            .write_block(0, packet.pts, packet.keyframe, data)
            .map_err(|error| refuse(error.into()))?;
        packets += 1;
        Ok(())
    };
    for n in 0..frames {
        let frame = video.frame(n, args.fps)?;
        encoder
            .encode(EncoderInput::Frame(frame), &mut sink)
            .context("encoding a frame")?;
    }
    encoder.flush(&mut sink).context("flushing the encoder")?;
    while k < blocks {
        let block = audio.block(k)?;
        writer.write_audio(1, block).context("writing audio")?;
        k += 1;
    }
    Ok((
        writer,
        Summary {
            frames: packets,
            blocks: k,
        },
    ))
}

/// The scene, composited and converted, with a white frame at each second.
struct Video {
    demo: DemoScene,
    compositor: CpuCompositor,
    frame: VideoFrame,
    spec: ColorSpec,
    context: TickContext,
}

impl Video {
    fn new(args: &Args) -> Result<Self> {
        let mut compositor = CpuCompositor::new(args.size).context("building the compositor")?;
        compositor.set_background([0x10, 0x18, 0x20, 0xFF]);
        Ok(Self {
            demo: DemoScene::new(args.size, ScaleFilter::Bilinear)
                .context("building the demo scene")?,
            compositor,
            frame: VideoFrame::new(PixelFormat::I420, args.size, Timestamp::ZERO)
                .context("allocating the frame")?,
            spec: ColorSpec::new(ColorSpace::Bt709, ColorRange::Limited),
            context: TickContext::new(0, args.fps),
        })
    }

    /// Frame `n`: the first frame at or after a whole second is a sync mark.
    fn frame(&mut self, n: u64, fps: Fps) -> Result<&VideoFrame> {
        let pts = fps.pts(n);
        self.demo.tick(&self.context).context("ticking the scene")?;
        let canvas = self
            .compositor
            .composite(self.demo.scene(), &self.demo, pts)
            .context("compositing")?;
        self.frame.set_pts(pts);
        rgb_to_yuv(canvas, &mut self.frame, self.spec).context("converting to I420")?;
        if is_mark_frame(n, fps) {
            // Limited-range white, neutral chroma.
            for (plane, value) in [(0, 235u8), (1, 128), (2, 128)] {
                if let Some(bytes) = self.frame.plane_mut(plane) {
                    bytes.fill(value);
                }
            }
        }
        self.context = self.context.next();
        Ok(&self.frame)
    }
}

/// Whether frame `n` is the first at or after a whole second: some second
/// `s` lies in `(pts(n − 1), pts(n)]`, which is when the whole seconds elapsed
/// — rounded down — differ between the two frames.
fn is_mark_frame(n: u64, fps: Fps) -> bool {
    let second = |n: u64| fps.pts(n).as_nanos() / 1_000_000_000;
    n == 0 || second(n) != second(n - 1)
}

/// A mono tone that sounds for [`MARK`] at each second, through the mixer.
struct Audio {
    mixer: Mixer,
    track: voltra_audio::TrackId,
    tone: AudioBuffer,
    mix: AudioBuffer,
    hz: f64,
}

impl Audio {
    fn new(hz: f64) -> Result<Self> {
        let spec = MixSpec::new(RATE, ChannelLayout::Stereo, BLOCK).context("the mix")?;
        let mut mixer = Mixer::new(spec).context("building the mixer")?;
        let track = mixer
            .add_track(ChannelLayout::Mono)
            .context("adding the tone")?
            .id();
        Ok(Self {
            mixer,
            track,
            tone: AudioBuffer::new(RATE, ChannelLayout::Mono, BLOCK, Timestamp::ZERO)
                .context("allocating the tone")?,
            mix: spec.buffer(Timestamp::ZERO).context("allocating the mix")?,
            hz,
        })
    }

    /// Mix block `k`.
    fn block(&mut self, k: u64) -> Result<&AudioBuffer> {
        let first = k * BLOCK as u64;
        let pts = RATE.pts(first);
        let rate = u64::from(RATE.hz());
        let mark = RATE.frames_in(MARK);
        if let Some(samples) = self.tone.channel_mut(0) {
            for (offset, sample) in samples.iter_mut().enumerate() {
                let index = first + offset as u64;
                *sample = if index % rate < mark {
                    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                    let value =
                        (std::f64::consts::TAU * self.hz * index as f64 / rate as f64).sin() as f32;
                    0.5 * value
                } else {
                    0.0
                };
            }
        }
        self.tone.set_pts(pts);
        self.mix.set_pts(pts);
        self.mixer
            .mix([(self.track, &self.tone)], &mut self.mix)
            .context("mixing")?;
        Ok(&self.mix)
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::Fps;

    use super::is_mark_frame;

    #[test]
    fn the_mark_is_the_first_frame_of_each_second() {
        let marks: Vec<u64> = (0..100)
            .filter(|&n| is_mark_frame(n, Fps::FPS_30))
            .collect();
        assert_eq!(marks, [0, 30, 60, 90]);
        let ntsc = Fps::new(30_000, 1_001).unwrap();
        let marks: Vec<u64> = (0..100).filter(|&n| is_mark_frame(n, ntsc)).collect();
        // 29.97 fps: frame 30 starts at 1.001 s, the first at or after 1 s.
        assert_eq!(marks, [0, 30, 60, 90]);
    }
}
