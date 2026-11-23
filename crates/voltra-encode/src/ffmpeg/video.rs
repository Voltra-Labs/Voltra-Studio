//! H.264 (and any `libavcodec` video encoder by name) behind the safe
//! [`FfmpegVideoEncoder`].

use std::ffi::c_int;
use std::ptr;

use ffmpeg_sys_next as ff;
use voltra_core::{Fps, PixelFormat, Timestamp};

use super::common::{
    Context, Frame, Packet, check, extradata, open_context, send_and_drain, to_int,
};
use crate::{
    Bitstream, Codec, EncoderInfo, EncoderInput, Error, PacketSink, Result, VideoEncoder,
    VideoSettings,
};

/// A video encoder backed by `libavcodec`.
///
/// # Examples
///
/// ```no_run
/// use voltra_core::{Fps, FrameSize};
/// use voltra_encode::{FfmpegVideoEncoder, VideoEncoder, VideoSettings};
///
/// let settings = VideoSettings::new(FrameSize::new(1280, 720)?, Fps::FPS_30);
/// let encoder = FfmpegVideoEncoder::h264(settings)?;
/// assert!(encoder.headers().is_some(), "x264 knows its SPS and PPS on opening");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct FfmpegVideoEncoder {
    // Field order is drop order: packet and frame before the context.
    packet: Packet,
    frame: Frame,
    context: Context,
    info: EncoderInfo,
    headers: Option<Vec<u8>>,
    flushed: bool,
}

// SAFETY: the three pointers are owned exclusively by this value (invariant 1),
// and a libavcodec context may be used from any thread, one at a time, which
// `&mut self` on every call that touches it guarantees.
unsafe impl Send for FfmpegVideoEncoder {}

impl FfmpegVideoEncoder {
    /// H.264 through x264, with OBS's defaults where they apply: preset
    /// `veryfast` and a bitrate-driven rate control with the VBV capped at the
    /// bitrate. Without filler, a simple scene stays under it rather than being
    /// padded up to it. B-frames are off until the muxer orders by decode time
    /// (plan 017).
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when `libavcodec` was built without x264,
    /// [`Error::Settings`] for settings it cannot encode, and
    /// [`Error::Native`] when opening fails.
    pub fn h264(settings: VideoSettings) -> Result<Self> {
        Self::open("libx264", settings)
    }

    /// The `libavcodec` encoder called `name`, such as `libx264` — or, on
    /// hardware this build has not been tested on, `h264_vaapi` or
    /// `h264_nvenc`.
    ///
    /// # Errors
    ///
    /// As [`h264`](Self::h264).
    pub fn open(name: &str, settings: VideoSettings) -> Result<Self> {
        validate(&settings)?;
        let (codec, context) = open_context(name)?;
        configure(&context, codec, settings)?;
        let headers = extradata(&context);
        let frame = allocate_frame(&context)?;
        Ok(Self {
            packet: Packet::new()?,
            frame,
            context,
            info: EncoderInfo {
                codec: Codec::H264,
                name: name.to_owned(),
                bitstream: Bitstream::AnnexB,
                settings,
            },
            headers,
            flushed: false,
        })
    }
}

impl VideoEncoder for FfmpegVideoEncoder {
    fn info(&self) -> &EncoderInfo {
        &self.info
    }

    fn headers(&self) -> Option<&[u8]> {
        self.headers.as_deref()
    }

    fn encode(&mut self, input: EncoderInput<'_>, sink: &mut PacketSink<'_>) -> Result<()> {
        let EncoderInput::Frame(source) = input;
        let settings = self.info.settings;
        if self.flushed {
            return Err(Error::Settings("the encoder has been flushed".to_owned()));
        }
        if source.format() != PixelFormat::I420 || source.size() != settings.size {
            return Err(Error::Settings(format!(
                "encoder takes I420 at {}, got {} at {}",
                settings.size,
                source.format(),
                source.size()
            )));
        }
        let index = frame_index(settings.fps, source.pts());
        let frame = self.frame.0.as_ptr();
        // SAFETY: the frame is valid; `av_frame_make_writable` gives us its
        // buffers, and every row copied fits within `linesize` and the plane's
        // height, both set for this size and format (invariant 2). This copy
        // is the one the plan names as unavoidable while input is memory.
        unsafe {
            check("av_frame_make_writable", ff::av_frame_make_writable(frame))?;
            for plane in 0..3 {
                let base = (*frame).data[plane];
                let stride = usize::try_from((*frame).linesize[plane]).unwrap_or(0);
                if let Some(rows) = source.rows(plane) {
                    for (row, bytes) in rows.enumerate() {
                        let len = bytes.len().min(stride);
                        ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(row * stride), len);
                    }
                }
            }
            (*frame).pts = i64::try_from(index).unwrap_or(i64::MAX);
        }
        let fps = settings.fps;
        send_and_drain(
            &self.context,
            &self.packet,
            frame,
            |t| frame_time(fps, t),
            sink,
        )
    }

    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        let fps = self.info.settings.fps;
        send_and_drain(
            &self.context,
            &self.packet,
            ptr::null(),
            |t| frame_time(fps, t),
            sink,
        )
    }
}

/// Set the fields `avcodec_open2` reads, and open.
fn configure(context: &Context, codec: *const ff::AVCodec, settings: VideoSettings) -> Result<()> {
    let fps = settings.fps;
    let bitrate = i64::from(settings.bitrate_kbps) * 1_000;
    let gop = settings.keyframe_seconds * fps.num() / fps.den().max(1);
    let context = context.0.as_ptr();
    // SAFETY: the context is valid and not yet opened (invariant 1); these are
    // plain fields `avcodec_open2` reads.
    unsafe {
        (*context).width = to_int(settings.size.width())?;
        (*context).height = to_int(settings.size.height())?;
        (*context).pix_fmt = ff::AVPixelFormat::AV_PIX_FMT_YUV420P;
        // The time base is one frame: pts is the frame index, exactly.
        (*context).time_base = ff::AVRational {
            num: to_int(fps.den())?,
            den: to_int(fps.num())?,
        };
        (*context).framerate = ff::AVRational {
            num: to_int(fps.num())?,
            den: to_int(fps.den())?,
        };
        (*context).bit_rate = bitrate;
        (*context).rc_max_rate = bitrate;
        (*context).rc_buffer_size = c_int::try_from(bitrate).unwrap_or(c_int::MAX);
        (*context).gop_size = to_int(gop.max(1))?;
        (*context).max_b_frames = 0;
        (*context).colorspace = ff::AVColorSpace::AVCOL_SPC_BT709;
        (*context).color_primaries = ff::AVColorPrimaries::AVCOL_PRI_BT709;
        (*context).color_trc = ff::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        (*context).color_range = ff::AVColorRange::AVCOL_RANGE_MPEG;
        // Parameter sets in `extradata`, once, for the container.
        (*context).flags |= c_int::try_from(ff::AV_CODEC_FLAG_GLOBAL_HEADER).unwrap_or(0);
        // Not every encoder has a preset; one that does not ignores it.
        let _ = ff::av_opt_set(
            (*context).priv_data,
            c"preset".as_ptr(),
            c"veryfast".as_ptr(),
            0,
        );

        check(
            "avcodec_open2",
            ff::avcodec_open2(context, codec, ptr::null_mut()),
        )
    }
}

/// A frame with buffers for the context's size and format.
fn allocate_frame(context: &Context) -> Result<Frame> {
    let frame = Frame::new()?;
    // SAFETY: both are valid; these are the fields `av_frame_get_buffer` reads
    // to size the buffers it allocates.
    unsafe {
        let pointer = frame.0.as_ptr();
        let context = context.0.as_ptr();
        (*pointer).format = (*context).pix_fmt as c_int;
        (*pointer).width = (*context).width;
        (*pointer).height = (*context).height;
        check("av_frame_get_buffer", ff::av_frame_get_buffer(pointer, 0))?;
    }
    Ok(frame)
}

fn validate(settings: &VideoSettings) -> Result<()> {
    let size = settings.size;
    if size.width() % 2 != 0 || size.height() % 2 != 0 {
        return Err(Error::Settings(format!(
            "4:2:0 needs an even size, not {size}"
        )));
    }
    if settings.bitrate_kbps == 0 || settings.keyframe_seconds == 0 {
        return Err(Error::Settings(
            "bitrate and keyframe interval must be positive".to_owned(),
        ));
    }
    Ok(())
}

/// The frame index a timestamp falls on, rounded to the nearest frame.
fn frame_index(fps: Fps, pts: Timestamp) -> u64 {
    let scaled = u128::from(pts.as_nanos()) * u128::from(fps.num());
    let per = u128::from(fps.den()) * 1_000_000_000;
    u64::try_from((scaled + per / 2) / per).unwrap_or(u64::MAX)
}

/// A packet time in frames back to a timestamp. Negative decode times, which
/// only B-frames produce, clamp to zero.
fn frame_time(fps: Fps, frames: i64) -> Timestamp {
    fps.pts(u64::try_from(frames).unwrap_or(0))
}

#[cfg(test)]
mod tests;
