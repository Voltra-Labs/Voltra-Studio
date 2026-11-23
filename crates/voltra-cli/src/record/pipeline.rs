//! Encoders, tracks and the loop: frames and blocks made in time order,
//! encoded or not, interleaved, written.

use std::io::Write;

use anyhow::{Context, Result};
use voltra_core::{AudioBuffer, ChannelLayout, SampleSpec, Timestamp, VideoFrame};
use voltra_encode::{
    AudioEncoder, AudioSettings, Bitstream, EncodedPacket, EncoderInput, VideoEncoder,
    VideoSettings, h264,
};
use voltra_output::{Interleaver, MatroskaWriter, TrackSpec};

use super::args::{Args, AudioCodec, VideoCodec};
use super::scene::{Audio, BLOCK, RATE, Video};

/// The recording's time zero. One second in, so an encoder's priming — the
/// samples its first packet starts before the input — has room before it as an
/// unsigned timestamp; the file's own zero is set to this instant, and what
/// comes before it is written with negative timestamps.
const ORIGIN: Timestamp = Timestamp::from_millis(1_000);

/// The encoders the arguments ask for. `None` stores that stream raw.
pub(super) struct Encoders {
    video: Option<Box<dyn VideoEncoder>>,
    audio: Option<Box<dyn AudioEncoder>>,
}

impl Encoders {
    pub(super) fn open(args: &Args) -> Result<Self> {
        let video = match args.video {
            VideoCodec::Raw => None,
            VideoCodec::H264 => Some(open_video(args)?),
        };
        let audio = match args.audio {
            AudioCodec::Pcm => None,
            AudioCodec::Aac => Some(open_audio(args)?),
        };
        Ok(Self { video, audio })
    }

    /// The file's tracks: video 0, audio 1, raw or with their encoder's
    /// configuration.
    pub(super) fn tracks(&self, args: &Args) -> Result<[TrackSpec; 2]> {
        let video = match &self.video {
            None => TrackSpec::RawVideo {
                size: args.size,
                fps: args.fps,
            },
            Some(encoder) => {
                // Matroska writes CodecPrivate up front; x264 knows its headers
                // on opening.
                let headers = encoder
                    .headers()
                    .context("the video encoder gave no headers before its first packet")?;
                TrackSpec::H264 {
                    size: args.size,
                    fps: args.fps,
                    decoder_config: h264::decoder_config(headers)
                        .context("reading the video encoder's headers")?,
                }
            }
        };
        let audio = match &self.audio {
            None => TrackSpec::PcmAudio {
                rate: RATE,
                layout: ChannelLayout::Stereo,
            },
            Some(encoder) => TrackSpec::Aac {
                rate: RATE,
                layout: ChannelLayout::Stereo,
                decoder_config: encoder
                    .headers()
                    .context("the audio encoder gave no configuration")?
                    .to_vec(),
                delay: encoder.info().delay,
            },
        };
        Ok([video, audio])
    }
}

#[cfg(feature = "ffmpeg")]
fn open_video(args: &Args) -> Result<Box<dyn VideoEncoder>> {
    let mut settings = VideoSettings::new(args.size, args.fps);
    settings.bitrate_kbps = args.bitrate;
    Ok(Box::new(
        voltra_encode::FfmpegVideoEncoder::h264(settings).context("opening x264")?,
    ))
}

#[cfg(feature = "ffmpeg")]
fn open_audio(args: &Args) -> Result<Box<dyn AudioEncoder>> {
    let mut settings = AudioSettings::new(RATE, ChannelLayout::Stereo);
    settings.bitrate_kbps = args.audio_bitrate;
    Ok(Box::new(
        voltra_encode::FfmpegAudioEncoder::aac(settings).context("opening AAC")?,
    ))
}

#[cfg(not(feature = "ffmpeg"))]
fn open_video(_: &Args) -> Result<Box<dyn VideoEncoder>> {
    let _ = VideoSettings::new;
    anyhow::bail!("this build has no H.264 encoder; rebuild with `--features ffmpeg`")
}

#[cfg(not(feature = "ffmpeg"))]
fn open_audio(_: &Args) -> Result<Box<dyn AudioEncoder>> {
    let _ = AudioSettings::new;
    anyhow::bail!("this build has no AAC encoder; rebuild with `--features ffmpeg`")
}

/// What a recording wrote.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Summary {
    pub(super) video_packets: u64,
    pub(super) audio_packets: u64,
    pub(super) most_queued: usize,
}

/// Make every frame and block in time order, encode what has an encoder, and
/// write it all through the interleaver: encoders hand packets over late, each
/// by its own delay, and the file must still be in time order.
pub(super) fn record<W: Write>(
    mut writer: MatroskaWriter<W>,
    args: &Args,
    encoders: &mut Encoders,
) -> Result<(MatroskaWriter<W>, Summary)> {
    writer
        .set_origin(ORIGIN)
        .context("setting the file's origin")?;
    let mut video = Video::new(args.size, args.fps)?;
    let mut audio = Audio::new(args.tone)?;
    let mut interleaver = Interleaver::new(2);
    let mut summary = Summary::default();
    // Reused across packets: raw payloads and Annex B conversions.
    let mut video_bytes = Vec::new();
    let mut audio_bytes = Vec::new();

    let frames = args.seconds * u64::from(args.fps.num()) / u64::from(args.fps.den());
    let blocks = (args.seconds * u64::from(RATE.hz())).div_ceil(BLOCK as u64);
    let frame_at = |n: u64| later(ORIGIN, args.fps.pts(n));
    let block_at = |k: u64| RATE.pts(u64::from(RATE.hz()) + k * BLOCK as u64);

    let (mut n, mut k) = (0u64, 0u64);
    while n < frames || k < blocks {
        // Video first on a tie: the frame at a second and the block holding
        // that second's sample start at the same instant.
        if n < frames && (k >= blocks || frame_at(n) <= block_at(k)) {
            let frame = video.frame(n, args.fps, frame_at(n))?;
            match encoders.video.as_deref_mut() {
                None => {
                    raw_frame(frame, &mut video_bytes);
                    interleaver.push(0, frame.pts(), true, &video_bytes)?;
                }
                Some(encoder) => {
                    let length_prefixed = encoder.info().bitstream == Bitstream::LengthPrefixed;
                    let mut sink = |packet: EncodedPacket<'_>| -> voltra_encode::Result<()> {
                        let data = if length_prefixed {
                            packet.data
                        } else {
                            h264::to_length_prefixed(packet.data, &mut video_bytes);
                            &video_bytes
                        };
                        interleaver
                            .push(0, packet.pts, packet.keyframe, data)
                            .map_err(refused)
                    };
                    encoder
                        .encode(EncoderInput::Frame(frame), &mut sink)
                        .context("encoding a frame")?;
                }
            }
            n += 1;
        } else {
            let block = audio.block(k, block_at(k))?;
            match encoders.audio.as_deref_mut() {
                None => {
                    raw_block(block, &mut audio_bytes)?;
                    interleaver.push(1, block.pts(), true, &audio_bytes)?;
                }
                Some(encoder) => {
                    let mut sink = |packet: EncodedPacket<'_>| -> voltra_encode::Result<()> {
                        interleaver
                            .push(1, packet.pts, packet.keyframe, packet.data)
                            .map_err(refused)
                    };
                    encoder.encode(block, &mut sink).context("encoding audio")?;
                }
            }
            k += 1;
        }
        drain(&mut interleaver, &mut writer, &mut summary)?;
    }

    if let Some(encoder) = encoders.video.as_deref_mut() {
        let length_prefixed = encoder.info().bitstream == Bitstream::LengthPrefixed;
        let mut sink = |packet: EncodedPacket<'_>| -> voltra_encode::Result<()> {
            let data = if length_prefixed {
                packet.data
            } else {
                h264::to_length_prefixed(packet.data, &mut video_bytes);
                &video_bytes
            };
            interleaver
                .push(0, packet.pts, packet.keyframe, data)
                .map_err(refused)
        };
        encoder
            .flush(&mut sink)
            .context("flushing the video encoder")?;
    }
    interleaver.finish(0);
    if let Some(encoder) = encoders.audio.as_deref_mut() {
        let mut sink = |packet: EncodedPacket<'_>| -> voltra_encode::Result<()> {
            interleaver
                .push(1, packet.pts, packet.keyframe, packet.data)
                .map_err(refused)
        };
        encoder
            .flush(&mut sink)
            .context("flushing the audio encoder")?;
    }
    interleaver.finish(1);
    drain(&mut interleaver, &mut writer, &mut summary)?;
    summary.most_queued = interleaver.peak();
    Ok((writer, summary))
}

/// Write whatever the interleaver can release.
fn drain<W: Write>(
    interleaver: &mut Interleaver,
    writer: &mut MatroskaWriter<W>,
    summary: &mut Summary,
) -> Result<()> {
    interleaver
        .drain(|track, pts, keyframe, data| {
            if track == 0 {
                summary.video_packets += 1;
            } else {
                summary.audio_packets += 1;
            }
            writer.write_block(track, pts, keyframe, data)
        })
        .context("writing a packet")
}

/// A raw I420 frame's planes, rows end to end, without stride padding.
fn raw_frame(frame: &VideoFrame, out: &mut Vec<u8>) {
    out.clear();
    for plane in 0..frame.plane_count() {
        if let Some(rows) = frame.rows(plane) {
            for row in rows {
                out.extend_from_slice(row);
            }
        }
    }
}

/// A block as interleaved little-endian float, the PCM track's payload.
fn raw_block(block: &AudioBuffer, out: &mut Vec<u8>) -> Result<()> {
    let spec = SampleSpec::F32_INTERLEAVED;
    out.resize(spec.byte_len(block.frames(), block.channel_count()), 0);
    block.write_to(out, spec).context("converting audio")
}

fn later(origin: Timestamp, offset: Timestamp) -> Timestamp {
    Timestamp::from_nanos(origin.as_nanos().saturating_add(offset.as_nanos()))
}

/// A writer's refusal, passed back through an encoder's sink.
#[allow(clippy::needless_pass_by_value, reason = "the shape `map_err` calls")]
fn refused(error: voltra_core::Error) -> voltra_encode::Error {
    voltra_encode::Error::Sink(error.to_string())
}
