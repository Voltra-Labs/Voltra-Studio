//! Errors of the audio crate.
//!
//! Every variant carries only `Copy` data, so building one never allocates.
//! That matters for the one error [`Mixer::mix`](crate::Mixer::mix) can return,
//! which is built on the audio thread.

use voltra_core::{ChannelLayout, SampleRate};

use crate::mixer::MixSpec;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What can go wrong configuring or driving a mixer.
///
/// Failures of a single track inside a block are **not** errors: they are
/// counted in [`MixReport`](crate::MixReport) and against the track, because one
/// broken source must not stop the others from sounding.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The mixer cannot route this track layout into its own.
    #[error("cannot mix a {track} track into a {mix} mix")]
    UnsupportedLayout {
        /// The layout of the track being added.
        track: ChannelLayout,
        /// The layout of the mix.
        mix: ChannelLayout,
    },

    /// A mix needs at least one frame per block.
    #[error("a mix block needs at least one frame")]
    EmptyBlock,

    /// A gain that is NaN, infinite, negative or above [`Gain::MAX`](crate::Gain::MAX).
    #[error("invalid gain: {0} (expected 0 ..= 10, linear)")]
    InvalidGain(f32),

    /// A balance that is NaN or outside `-1 ..= 1`.
    #[error("invalid balance: {0} (expected -1 ..= 1)")]
    InvalidBalance(f32),

    /// The output buffer handed to `mix` does not match the mixer.
    #[error("output buffer is {found:?}, mixer expects {expected:?}")]
    OutputMismatch {
        /// What the mixer was built for.
        expected: MixSpec,
        /// What the output buffer is.
        found: MixSpec,
    },

    /// Track identifiers are never reused, and this mixer has used them all.
    #[error("track identifiers exhausted")]
    TooManyTracks,

    /// A resampler from a rate to itself: nothing to convert.
    #[error("cannot resample {0} to itself")]
    SameRate(SampleRate),

    /// The reduced ratio between the rates needs more polyphase phases than
    /// the resampler supports. No pair of standard rates does.
    #[error("cannot resample {from} to {to}: the ratio needs too many phases")]
    UnsupportedRatio {
        /// The input rate.
        from: SampleRate,
        /// The output rate.
        to: SampleRate,
    },
}
