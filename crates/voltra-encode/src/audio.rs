//! The audio encoder trait and the types it speaks.

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate};

use crate::{PacketSink, Result};

/// An audio codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC-LC.
    Aac,
}

/// What to encode and how hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSettings {
    /// Sample rate of the input and the stream.
    pub rate: SampleRate,
    /// Channel layout of the input and the stream.
    pub layout: ChannelLayout,
    /// Bitrate in kilobits per second.
    pub bitrate_kbps: u32,
}

impl AudioSettings {
    /// OBS's AAC default: 128 kbps.
    pub const fn new(rate: SampleRate, layout: ChannelLayout) -> Self {
        Self {
            rate,
            layout,
            bitrate_kbps: 128,
        }
    }
}

/// What an audio encoder is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioEncoderInfo {
    /// The codec it produces.
    pub codec: AudioCodec,
    /// A human-readable name, such as `aac`.
    pub name: String,
    /// The settings it was opened with.
    pub settings: AudioSettings,
    /// Samples per channel in each packet: 1024 for AAC.
    pub frame_size: usize,
    /// Samples the encoder primes before the first one it is given. Its first
    /// packet starts this much before the input, and a container records it so
    /// the decoder discards them (Matroska's `CodecDelay`).
    pub delay: u32,
}

/// An audio encoder.
///
/// Blocks of any length go in; the encoder gathers them into its own frames
/// and hands each packet to the sink, stamped with the time of its first
/// decoded sample — which, for the first packets, is before the input's start
/// by [`AudioEncoderInfo::delay`]. The input's timestamps must therefore be at
/// least that far from zero, which a monotonic clock always is.
pub trait AudioEncoder: Send {
    /// What this encoder is.
    fn info(&self) -> &AudioEncoderInfo;

    /// The codec configuration a container stores (the `AudioSpecificConfig`
    /// for AAC), once known.
    fn headers(&self) -> Option<&[u8]>;

    /// Encode one block, which must follow the previous one without a gap.
    ///
    /// # Errors
    ///
    /// When the block does not match the settings, the encoder fails, or the
    /// sink refuses a packet.
    fn encode(&mut self, block: &AudioBuffer, sink: &mut PacketSink<'_>) -> Result<()>;

    /// Encode what is left, as a short last frame, and hand over everything the
    /// encoder still holds. The encoder is finished afterwards.
    ///
    /// # Errors
    ///
    /// When the encoder fails or the sink refuses a packet.
    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()>;
}
