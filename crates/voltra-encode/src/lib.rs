//! Encoding: the encoder trait, H.264 bitstream tools, and the encoders.
//!
//! The shape of [`VideoEncoder`] follows libobs's `obs_encoder_info` and was
//! checked on paper against x264, NVENC, VAAPI and `VideoToolbox` before being
//! written down (`docs/references/encoding.md` §4, as ADR 0003 asks):
//!
//! - **Output is a stream.** A frame in may produce no packet, or several;
//!   [`VideoEncoder::flush`] hands over what the encoder still holds.
//! - **Packets are lent, not copied.** Each [`EncodedPacket`] borrows the
//!   encoder's buffer for the duration of the call, so it can go to a muxer
//!   without a copy.
//! - **Headers may arrive late.** [`VideoEncoder::headers`] is `None` until the
//!   encoder knows them, which on `VideoToolbox` is after the first packet.
//! - **The input can become a texture.** [`EncoderInput`] is an enum; the GPU
//!   path of ADR 0003 adds a variant instead of a second trait.
//!
//! [`h264`] converts between the bitstream an encoder produces and what a
//! container wants, in pure Rust. The encoders that need native libraries sit
//! behind features and never in the public API's types.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![allow(clippy::must_use_candidate)]

mod encoder;
mod error;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
pub mod h264;

pub use encoder::{
    Bitstream, Codec, EncodedPacket, EncoderInfo, EncoderInput, PacketSink, VideoEncoder,
    VideoSettings,
};
pub use error::{Error, Result};
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::FfmpegVideoEncoder;
