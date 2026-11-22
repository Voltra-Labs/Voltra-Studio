//! The encoder trait and the types it speaks.

use voltra_core::{Fps, FrameSize, Timestamp, VideoFrame};

use crate::Result;

/// A video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
    /// H.264 / AVC.
    H264,
}

/// How an encoder frames its NAL units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Bitstream {
    /// Start codes (`00 00 01`), as x264, NVENC and VAAPI produce.
    AnnexB,
    /// A four-byte big-endian length before each unit, as `VideoToolbox` produces
    /// and Matroska and MP4 store.
    LengthPrefixed,
}

/// What to encode and how hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSettings {
    /// Frame size; even in both dimensions for 4:2:0.
    pub size: FrameSize,
    /// Frame rate. Also the time base: frame `n` is `fps.pts(n)`.
    pub fps: Fps,
    /// Target bitrate in kilobits per second, constant.
    pub bitrate_kbps: u32,
    /// Seconds between keyframes.
    pub keyframe_seconds: u32,
}

impl VideoSettings {
    /// OBS's x264 defaults: 6000 kbps, keyframes every 2 s.
    pub const fn new(size: FrameSize, fps: Fps) -> Self {
        Self {
            size,
            fps,
            bitrate_kbps: 6_000,
            keyframe_seconds: 2,
        }
    }
}

/// What an encoder is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderInfo {
    /// The codec it produces.
    pub codec: Codec,
    /// A human-readable name, such as `libx264`.
    pub name: String,
    /// How its packets frame their units.
    pub bitstream: Bitstream,
    /// The settings it was opened with.
    pub settings: VideoSettings,
}

/// What an encoder is fed.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum EncoderInput<'a> {
    /// A frame in system memory, stamped with its presentation time.
    Frame(&'a VideoFrame),
}

/// One encoded packet, lent for the duration of the call that receives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedPacket<'a> {
    /// The packet, in the encoder's [`Bitstream`].
    pub data: &'a [u8],
    /// Presentation time.
    pub pts: Timestamp,
    /// Decode time; equal to `pts` without B-frames.
    pub dts: Timestamp,
    /// Whether a decoder can start here.
    pub keyframe: bool,
}

/// Where packets go: called once per packet, not per pixel, so dynamic
/// dispatch here costs nothing that can be measured (CLAUDE.md §4.4).
pub type PacketSink<'s> = dyn FnMut(EncodedPacket<'_>) -> Result<()> + 's;

/// A video encoder.
///
/// # Examples
///
/// ```
/// use voltra_core::Timestamp;
/// use voltra_encode::{EncodedPacket, Result};
///
/// // Any closure that takes a packet is a sink.
/// let mut sizes = Vec::new();
/// let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
///     sizes.push(packet.data.len());
///     Ok(())
/// };
/// sink(EncodedPacket { data: &[0, 0, 0, 1], pts: Timestamp::ZERO, dts: Timestamp::ZERO, keyframe: true })?;
/// assert_eq!(sizes, [4]);
/// # Ok::<(), voltra_encode::Error>(())
/// ```
pub trait VideoEncoder: Send {
    /// What this encoder is.
    fn info(&self) -> &EncoderInfo;

    /// The codec's parameter sets (SPS and PPS for H.264) in the encoder's
    /// bitstream format, once known. A container writes them once, up front.
    fn headers(&self) -> Option<&[u8]>;

    /// Encode one input, handing any packets it completes to `sink`. There may
    /// be none: encoders hold frames to look ahead.
    ///
    /// # Errors
    ///
    /// When the input does not match the settings, the encoder fails, or the
    /// sink refuses a packet.
    fn encode(&mut self, input: EncoderInput<'_>, sink: &mut PacketSink<'_>) -> Result<()>;

    /// Hand every packet the encoder still holds to `sink`. The encoder is
    /// finished afterwards.
    ///
    /// # Errors
    ///
    /// When the encoder fails or the sink refuses a packet.
    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()>;
}
