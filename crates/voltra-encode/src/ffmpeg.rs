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

mod audio;
mod common;
mod video;

pub use audio::FfmpegAudioEncoder;
pub use video::FfmpegVideoEncoder;
