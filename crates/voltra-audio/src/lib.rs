//! Audio processing for Voltra Studio.
//!
//! The vocabulary — sample rate, channel layout, the float planar buffer — lives
//! in `voltra-core`, so a capture backend can hand over samples without
//! depending on the mixer. This crate is what *does* things to them.
//!
//! Everything here obeys CLAUDE.md §4.3: the audio callback allocates nothing,
//! locks nothing, and makes no syscalls. Control changes arrive through atomics
//! that the data path reads once per block. The claim is enforced by
//! `tests/no_allocation.rs`, which counts allocations with a global allocator
//! rather than trusting anyone to remember.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Gains and samples are floats, and several tests assert against a value that
// was *assigned* from an exact constant rather than computed toward it — unity
// really is 1.0. Comparing those within a margin would assert less, not more;
// where a value is genuinely computed, the tests use a tolerance explicitly.
#![cfg_attr(test, allow(clippy::float_cmp))]

mod balance;
mod gain;
mod level;
mod mixer;
mod resample;
mod track;

pub use balance::Balance;
pub use gain::Gain;
pub use level::{ChannelLevel, Levels};
pub use mixer::{MixInput, MixStats, Mixer};
pub use resample::{ResampleRatio, Resampler};
pub use track::{TrackHandle, TrackId};

// Re-exported so a caller configuring a mixer does not need `voltra-core` in
// their imports just to name a rate.
pub use voltra_core::{ChannelLayout, SampleRate};
