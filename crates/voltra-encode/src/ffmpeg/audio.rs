//! AAC through `libavcodec`'s own encoder, behind the safe
//! [`FfmpegAudioEncoder`].

use std::ffi::c_int;
use std::ptr;

use ffmpeg_sys_next as ff;
use voltra_core::{AudioBuffer, SampleRate, Timestamp};

use super::common::{
    Context, Frame, Packet, check, extradata, open_context, send_and_drain, to_int,
};
use crate::{AudioCodec, AudioEncoder, AudioEncoderInfo, AudioSettings, Error, PacketSink, Result};

/// An audio encoder backed by `libavcodec`.
///
/// # Examples
///
/// ```no_run
/// use voltra_core::{ChannelLayout, SampleRate};
/// use voltra_encode::{AudioEncoder, AudioSettings, FfmpegAudioEncoder};
///
/// let encoder = FfmpegAudioEncoder::aac(AudioSettings::new(SampleRate::HZ_48000, ChannelLayout::Stereo))?;
/// assert_eq!(encoder.info().frame_size, 1024);
/// assert_eq!(encoder.info().delay, 1024, "AAC primes one frame");
/// # Ok::<(), voltra_encode::Error>(())
/// ```
#[derive(Debug)]
pub struct FfmpegAudioEncoder {
    // Field order is drop order: packet and frame before the context.
    packet: Packet,
    frame: Frame,
    context: Context,
    info: AudioEncoderInfo,
    headers: Option<Vec<u8>>,
    /// Samples already in the frame being filled.
    filled: usize,
    /// Absolute index of the frame being filled's first sample, once known.
    next: Option<u64>,
    flushed: bool,
}

// SAFETY: as for the video encoder: the pointers are owned exclusively by this
// value, and the context is used by one thread at a time through `&mut self`.
unsafe impl Send for FfmpegAudioEncoder {}

impl FfmpegAudioEncoder {
    /// AAC-LC through `FFmpeg`'s native encoder, which every `libavcodec` has.
    ///
    /// # Errors
    ///
    /// [`Error::Settings`] for a zero bitrate, [`Error::Unavailable`] without
    /// the encoder, [`Error::Native`] when opening fails.
    pub fn aac(settings: AudioSettings) -> Result<Self> {
        Self::open("aac", settings)
    }

    fn open(name: &str, settings: AudioSettings) -> Result<Self> {
        if settings.bitrate_kbps == 0 {
            return Err(Error::Settings("bitrate must be positive".to_owned()));
        }
        let (codec, context) = open_context(name)?;
        let channels = to_int(u32::try_from(settings.layout.channels()).unwrap_or(u32::MAX))?;
        let pointer = context.0.as_ptr();
        // SAFETY: the context is valid and not yet opened; these are the plain
        // fields `avcodec_open2` reads, and `ch_layout` is set through the
        // function meant for it.
        let (frame_size, delay) = unsafe {
            (*pointer).sample_fmt = ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP;
            (*pointer).sample_rate = to_int(settings.rate.hz())?;
            ff::av_channel_layout_default(&raw mut (*pointer).ch_layout, channels);
            (*pointer).bit_rate = i64::from(settings.bitrate_kbps) * 1_000;
            (*pointer).time_base = ff::AVRational {
                num: 1,
                den: to_int(settings.rate.hz())?,
            };
            (*pointer).flags |= c_int::try_from(ff::AV_CODEC_FLAG_GLOBAL_HEADER).unwrap_or(0);
            check(
                "avcodec_open2",
                ff::avcodec_open2(pointer, codec, ptr::null_mut()),
            )?;
            let frame_size = usize::try_from((*pointer).frame_size).unwrap_or(0);
            let delay = u32::try_from((*pointer).initial_padding).unwrap_or(0);
            (if frame_size == 0 { 1024 } else { frame_size }, delay)
        };
        let headers = extradata(&context);
        let frame = Frame::new()?;
        // SAFETY: both are valid; these are the fields `av_frame_get_buffer`
        // reads to size a planar float buffer of one encoder frame.
        unsafe {
            let raw = frame.0.as_ptr();
            (*raw).format = ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP as c_int;
            (*raw).nb_samples = to_int(u32::try_from(frame_size).unwrap_or(u32::MAX))?;
            (*raw).sample_rate = (*pointer).sample_rate;
            check(
                "av_channel_layout_copy",
                ff::av_channel_layout_copy(
                    &raw mut (*raw).ch_layout,
                    &raw const (*pointer).ch_layout,
                ),
            )?;
            check("av_frame_get_buffer", ff::av_frame_get_buffer(raw, 0))?;
        }
        Ok(Self {
            packet: Packet::new()?,
            frame,
            context,
            info: AudioEncoderInfo {
                codec: AudioCodec::Aac,
                name: name.to_owned(),
                settings,
                frame_size,
                delay,
            },
            headers,
            filled: 0,
            next: None,
            flushed: false,
        })
    }

    /// Send the frame being filled — `samples` long — and reset it.
    fn send(&mut self, samples: usize, sink: &mut PacketSink<'_>) -> Result<()> {
        let start = self.next.unwrap_or(0);
        let frame = self.frame.0.as_ptr();
        // SAFETY: the frame is ours; these fields describe how much of its
        // buffer holds samples and where they start in time.
        unsafe {
            (*frame).nb_samples = to_int(u32::try_from(samples).unwrap_or(u32::MAX))?;
            (*frame).pts = i64::try_from(start).unwrap_or(i64::MAX);
        }
        let rate = self.info.settings.rate;
        send_and_drain(
            &self.context,
            &self.packet,
            frame,
            |t| sample_time(rate, t),
            sink,
        )?;
        self.next = Some(start + samples as u64);
        self.filled = 0;
        Ok(())
    }
}

impl AudioEncoder for FfmpegAudioEncoder {
    fn info(&self) -> &AudioEncoderInfo {
        &self.info
    }

    fn headers(&self) -> Option<&[u8]> {
        self.headers.as_deref()
    }

    fn encode(&mut self, block: &AudioBuffer, sink: &mut PacketSink<'_>) -> Result<()> {
        let settings = self.info.settings;
        if self.flushed {
            return Err(Error::Settings("the encoder has been flushed".to_owned()));
        }
        if block.rate() != settings.rate || block.layout() != settings.layout {
            return Err(Error::Settings(format!(
                "encoder takes {} {}, got {} {}",
                settings.rate,
                settings.layout,
                block.rate(),
                block.layout()
            )));
        }
        if self.next.is_none() {
            self.next = Some(sample_index(settings.rate, block.pts()));
        }
        let frame_size = self.info.frame_size;
        let mut offset = 0;
        while offset < block.frames() {
            let take = (frame_size - self.filled).min(block.frames() - offset);
            let frame = self.frame.0.as_ptr();
            // SAFETY: the frame's buffer holds `frame_size` floats per channel
            // plane (it was allocated so); `make_writable` makes it ours before
            // the first write of each frame; and `filled + take` never passes
            // `frame_size`.
            unsafe {
                if self.filled == 0 {
                    check("av_frame_make_writable", ff::av_frame_make_writable(frame))?;
                }
                for (channel, samples) in block.channels().enumerate() {
                    #[allow(
                        clippy::cast_ptr_alignment,
                        reason = "av_frame_get_buffer aligns planes to at least 32 bytes"
                    )]
                    let plane = (*frame).data[channel].cast::<f32>();
                    if let Some(source) = samples.get(offset..offset + take) {
                        ptr::copy_nonoverlapping(source.as_ptr(), plane.add(self.filled), take);
                    }
                }
            }
            self.filled += take;
            offset += take;
            if self.filled == frame_size {
                self.send(frame_size, sink)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        if self.filled > 0 {
            // AAC accepts a short last frame.
            self.send(self.filled, sink)?;
        }
        let rate = self.info.settings.rate;
        send_and_drain(
            &self.context,
            &self.packet,
            ptr::null(),
            |t| sample_time(rate, t),
            sink,
        )
    }
}

/// The sample at which `time` falls, rounded to the nearest.
fn sample_index(rate: SampleRate, time: Timestamp) -> u64 {
    let scaled = u128::from(time.as_nanos()) * u128::from(rate.hz()) + 500_000_000;
    u64::try_from(scaled / 1_000_000_000).unwrap_or(u64::MAX)
}

/// A packet time in samples back to a timestamp. A packet before zero — the
/// priming of a stream whose input started within the delay of zero — clamps.
fn sample_time(rate: SampleRate, samples: i64) -> Timestamp {
    rate.pts(u64::try_from(samples).unwrap_or(0))
}

#[cfg(test)]
mod tests;
