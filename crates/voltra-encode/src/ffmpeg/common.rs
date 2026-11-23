//! What every `libavcodec` encoder here shares: owned wrappers for the
//! context, frame and packet, error translation, and the log level.

use std::ffi::{CStr, c_int};
use std::ptr::NonNull;

use ffmpeg_sys_next as ff;

use crate::{Error, Result};

/// An owned `AVCodecContext`, freed on drop.
#[derive(Debug)]
pub(super) struct Context(pub(super) NonNull<ff::AVCodecContext>);
/// An owned `AVFrame`, freed on drop.
#[derive(Debug)]
pub(super) struct Frame(pub(super) NonNull<ff::AVFrame>);
/// An owned `AVPacket`, freed on drop.
#[derive(Debug)]
pub(super) struct Packet(pub(super) NonNull<ff::AVPacket>);

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

impl Frame {
    /// A new, empty frame.
    pub(super) fn new() -> Result<Self> {
        // SAFETY: an allocation with no preconditions; the result is checked.
        NonNull::new(unsafe { ff::av_frame_alloc() })
            .map(Self)
            .ok_or_else(|| native("av_frame_alloc", ff::AVERROR(ff::ENOMEM)))
    }
}

impl Packet {
    /// A new, empty packet.
    pub(super) fn new() -> Result<Self> {
        // SAFETY: an allocation with no preconditions; the result is checked.
        NonNull::new(unsafe { ff::av_packet_alloc() })
            .map(Self)
            .ok_or_else(|| native("av_packet_alloc", ff::AVERROR(ff::ENOMEM)))
    }
}

/// Find the encoder called `name` and allocate a context for it.
pub(super) fn open_context(name: &str) -> Result<(*const ff::AVCodec, Context)> {
    quiet_info_logs();
    let wanted = std::ffi::CString::new(name)
        .map_err(|_| Error::Settings(format!("encoder name {name:?}")))?;
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
    Ok((codec, context))
}

/// The encoder's `extradata`, once opened.
pub(super) fn extradata(context: &Context) -> Option<Vec<u8>> {
    let context = context.0.as_ptr();
    // SAFETY: the context is open; `extradata` is read within the size it
    // reports.
    unsafe {
        let size = usize::try_from((*context).extradata_size).unwrap_or(0);
        if (*context).extradata.is_null() || size == 0 {
            None
        } else {
            Some(std::slice::from_raw_parts((*context).extradata, size).to_vec())
        }
    }
}

/// Send `frame` (or null, to flush) and hand every packet that comes back to
/// `sink`, with timestamps converted from the context's time base by `time`.
pub(super) fn send_and_drain(
    context: &Context,
    packet: &Packet,
    frame: *const ff::AVFrame,
    time: impl Fn(i64) -> voltra_core::Timestamp,
    sink: &mut crate::PacketSink<'_>,
) -> Result<()> {
    let context = context.0.as_ptr();
    let packet = packet.0.as_ptr();
    // SAFETY: the context is open; `frame` is ours and filled, or null.
    check("avcodec_send_frame", unsafe {
        ff::avcodec_send_frame(context, frame)
    })?;
    loop {
        // SAFETY: context and packet are valid (invariant 1).
        let status = unsafe { ff::avcodec_receive_packet(context, packet) };
        if status == ff::AVERROR(ff::EAGAIN) || status == ff::AVERROR_EOF {
            return Ok(());
        }
        check("avcodec_receive_packet", status)?;
        // SAFETY: a packet was just received; its data is lent to the sink and
        // unreferenced right after (invariant 3).
        let result = unsafe {
            let size = usize::try_from((*packet).size).unwrap_or(0);
            let data = if (*packet).data.is_null() || size == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts((*packet).data, size)
            };
            let encoded = crate::EncodedPacket {
                data,
                pts: time((*packet).pts),
                dts: time((*packet).dts),
                keyframe: (*packet).flags & ff::AV_PKT_FLAG_KEY != 0,
            };
            let result = sink(encoded);
            ff::av_packet_unref(packet);
            result
        };
        result?;
    }
}

/// libav logs at `info` to stderr by default — x264 prints its whole option
/// string on every open. Warnings and errors are still worth seeing; the rest
/// is noise in a tool's output. Process-wide, so set once.
pub(super) fn quiet_info_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: `av_log_set_level` stores an integer; it has no preconditions.
    ONCE.call_once(|| unsafe { ff::av_log_set_level(ff::AV_LOG_WARNING) });
}

pub(super) fn to_int(value: u32) -> Result<c_int> {
    c_int::try_from(value).map_err(|_| Error::Settings(format!("{value} is out of range")))
}

pub(super) fn check(call: &'static str, status: c_int) -> Result<()> {
    if status < 0 {
        Err(native(call, status))
    } else {
        Ok(())
    }
}

pub(super) fn native(call: &'static str, status: c_int) -> Error {
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
