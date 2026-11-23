//! Encoders through `libavcodec`: x264 today, others by name (ADR 0003).
//!
//! Every `unsafe` block of the crate lives here, each with its `// SAFETY:`
//! note, behind the safe [`FfmpegVideoEncoder`]. The invariants they rely on:
//!
//! 1. The context, frame and packet are allocated in `open`, checked non-null,
//!    owned exclusively by their wrapper, and freed once by its `Drop` — so an
//!    `open` that fails halfway frees exactly what it had allocated.
//! 2. Writes into the frame's planes stay within `linesize × height` of a
//!    buffer `av_frame_get_buffer` allocated for this size and format, after
//!    `av_frame_make_writable` has made it ours.
//! 3. A packet's data is read only between a successful
//!    `avcodec_receive_packet` and the `av_packet_unref` that follows, and is
//!    lent to the sink for exactly that span.

use std::ffi::{CStr, CString, c_int};
use std::ptr::{self, NonNull};

use ffmpeg_sys_next as ff;
use voltra_core::{Fps, PixelFormat, Timestamp};

use crate::{
    Bitstream, Codec, EncodedPacket, EncoderInfo, EncoderInput, Error, PacketSink, Result,
    VideoEncoder, VideoSettings,
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

/// An owned `AVCodecContext`, freed on drop.
#[derive(Debug)]
struct Context(NonNull<ff::AVCodecContext>);
/// An owned `AVFrame`, freed on drop.
#[derive(Debug)]
struct Frame(NonNull<ff::AVFrame>);
/// An owned `AVPacket`, freed on drop.
#[derive(Debug)]
struct Packet(NonNull<ff::AVPacket>);

impl Drop for Context {
    fn drop(&mut self) {
        let mut pointer = self.0.as_ptr();
        // SAFETY: allocated by `avcodec_alloc_context3`, owned only here and
        // freed once (invariant 1); the call nulls the local copy.
        unsafe { ff::avcodec_free_context(&raw mut pointer) };
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let mut pointer = self.0.as_ptr();
        // SAFETY: allocated by `av_frame_alloc`, owned only here, freed once.
        unsafe { ff::av_frame_free(&raw mut pointer) };
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        let mut pointer = self.0.as_ptr();
        // SAFETY: allocated by `av_packet_alloc`, owned only here, freed once.
        unsafe { ff::av_packet_free(&raw mut pointer) };
    }
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
        quiet_info_logs();
        validate(&settings)?;
        let wanted =
            CString::new(name).map_err(|_| Error::Settings(format!("encoder name {name:?}")))?;
        // SAFETY: `wanted` is a valid NUL-terminated string for the call.
        let codec = unsafe { ff::avcodec_find_encoder_by_name(wanted.as_ptr()) };
        if codec.is_null() {
            return Err(Error::Unavailable(format!(
                "libavcodec has no encoder {name}"
            )));
        }
        // SAFETY: `codec` is a non-null encoder from libavcodec's own table.
        let context = NonNull::new(unsafe { ff::avcodec_alloc_context3(codec) })
            .map(Context)
            .ok_or_else(|| native("avcodec_alloc_context3", ff::AVERROR(ff::ENOMEM)))?;
        let headers = configure(&context, codec, settings)?;
        let frame = allocate_frame(&context)?;
        // SAFETY: an allocation with no preconditions; the result is checked.
        let packet = NonNull::new(unsafe { ff::av_packet_alloc() })
            .map(Packet)
            .ok_or_else(|| native("av_packet_alloc", ff::AVERROR(ff::ENOMEM)))?;
        Ok(Self {
            packet,
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

    /// Send `frame` (or null, to flush) and hand every packet that comes back
    /// to `sink`.
    fn send_and_drain(
        &mut self,
        frame: *const ff::AVFrame,
        sink: &mut PacketSink<'_>,
    ) -> Result<()> {
        let context = self.context.0.as_ptr();
        let packet = self.packet.0.as_ptr();
        // SAFETY: the context is open; `frame` is ours and filled, or null.
        check("avcodec_send_frame", unsafe {
            ff::avcodec_send_frame(context, frame)
        })?;
        let fps = self.info.settings.fps;
        loop {
            // SAFETY: context and packet are valid (invariant 1).
            let status = unsafe { ff::avcodec_receive_packet(context, packet) };
            if status == ff::AVERROR(ff::EAGAIN) || status == ff::AVERROR_EOF {
                return Ok(());
            }
            check("avcodec_receive_packet", status)?;
            // SAFETY: a packet was just received; its data is lent to the sink
            // and unreferenced right after (invariant 3).
            let result = unsafe {
                let size = usize::try_from((*packet).size).unwrap_or(0);
                let data = if (*packet).data.is_null() || size == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts((*packet).data, size)
                };
                let encoded = EncodedPacket {
                    data,
                    pts: frame_time(fps, (*packet).pts),
                    dts: frame_time(fps, (*packet).dts),
                    keyframe: (*packet).flags & ff::AV_PKT_FLAG_KEY != 0,
                };
                let result = sink(encoded);
                ff::av_packet_unref(packet);
                result
            };
            result?;
        }
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
        self.send_and_drain(frame, sink)
    }

    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()> {
        if self.flushed {
            return Ok(());
        }
        self.flushed = true;
        self.send_and_drain(ptr::null(), sink)
    }
}

/// Set the fields `avcodec_open2` reads, open, and copy the headers out.
fn configure(
    context: &Context,
    codec: *const ff::AVCodec,
    settings: VideoSettings,
) -> Result<Option<Vec<u8>>> {
    let fps = settings.fps;
    let bitrate = i64::from(settings.bitrate_kbps) * 1_000;
    let gop = settings.keyframe_seconds * fps.num() / fps.den().max(1);
    let context = context.0.as_ptr();
    // SAFETY: the context is valid and not yet opened (invariant 1); these are
    // plain fields `avcodec_open2` reads, and `extradata` is read after it
    // succeeds, within the size it reports.
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
        )?;

        let size = usize::try_from((*context).extradata_size).unwrap_or(0);
        if (*context).extradata.is_null() || size == 0 {
            Ok(None)
        } else {
            Ok(Some(
                std::slice::from_raw_parts((*context).extradata, size).to_vec(),
            ))
        }
    }
}

/// A frame with buffers for the context's size and format.
fn allocate_frame(context: &Context) -> Result<Frame> {
    // SAFETY: an allocation with no preconditions; the result is checked.
    let frame = NonNull::new(unsafe { ff::av_frame_alloc() })
        .map(Frame)
        .ok_or_else(|| native("av_frame_alloc", ff::AVERROR(ff::ENOMEM)))?;
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

/// libav logs at `info` to stderr by default — x264 prints its whole option
/// string on every open. Warnings and errors are still worth seeing; the rest
/// is noise in a tool's output. Process-wide, so set once.
fn quiet_info_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: `av_log_set_level` stores an integer; it has no preconditions.
    ONCE.call_once(|| unsafe { ff::av_log_set_level(ff::AV_LOG_WARNING) });
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

fn to_int(value: u32) -> Result<c_int> {
    c_int::try_from(value).map_err(|_| Error::Settings(format!("{value} is out of range")))
}

fn check(call: &'static str, status: c_int) -> Result<()> {
    if status < 0 {
        Err(native(call, status))
    } else {
        Ok(())
    }
}

fn native(call: &'static str, status: c_int) -> Error {
    let mut buffer = [0 as std::ffi::c_char; 128];
    // SAFETY: `av_strerror` writes at most `buffer.len()` bytes, NUL included,
    // into a buffer we own, and always terminates it.
    let message = unsafe {
        ff::av_strerror(status, buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    Error::Native { call, message }
}

#[cfg(test)]
mod tests;
