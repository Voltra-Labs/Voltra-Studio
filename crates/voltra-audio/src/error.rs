//! Errors of the audio crate.
//!
//! Every variant carries only `Copy` data, so building one never allocates.

use voltra_core::ChannelLayout;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What can go wrong configuring or driving a mixer.
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

    /// A gain that is NaN, infinite, negative or above [`Gain::MAX`](crate::Gain::MAX).
    #[error("invalid gain: {0} (expected 0 ..= 10, linear)")]
    InvalidGain(f32),

    /// A balance that is NaN or outside `-1 ..= 1`.
    #[error("invalid balance: {0} (expected -1 ..= 1)")]
    InvalidBalance(f32),
}
