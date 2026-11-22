//! Audio processing: the real-time mixer.
//!
//! [`Mixer`] sums tracks into one output block with per-track gain, balance
//! and mute, and is built to be called from an audio callback. Its invariants
//! are the point of the crate:
//!
//! - **[`Mixer::mix`] never allocates and never blocks.** Track parameters cross
//!   threads as atomics ([`TrackControl`]); no lock exists anywhere in the crate
//!   (`clippy.toml` bans them) and an allocation-counting test holds the line on
//!   the heap.
//! - **One track failing never silences another.** A block with a non-finite
//!   sample, the wrong rate, the wrong length or a misaligned timestamp is
//!   dropped and counted against its own track; everything else still sounds.
//! - **No change of gain is a step.** Volume, balance and mute all ramp across
//!   one block, so muting does not click and dragging a fader does not zipper.
//! - **The sum is not clipped.** Float carries a sum above unity; it is limited
//!   once, when the edge conversion writes integers.
//!
//! The vocabulary — [`AudioBuffer`](voltra_core::AudioBuffer), rates, layouts —
//! lives in `voltra-core`. The design against libobs is in
//! `docs/references/audio-mixer.md` and `docs/plans/012-mezclador.md`.
//!
//! # Examples
//!
//! ```
//! use voltra_audio::{Gain, MixSpec, Mixer};
//! use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
//!
//! let spec = MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 1024)?;
//! let mut mixer = Mixer::new(spec)?;
//! let voice = mixer.add_track(ChannelLayout::Mono)?;
//! let music = mixer.add_track(ChannelLayout::Stereo)?;
//!
//! // The UI keeps the controls; the audio thread keeps the mixer.
//! music.set_gain(Gain::from_db(-12.0)?);
//!
//! // In real use these come from capture; here they are silence.
//! let mic_block = AudioBuffer::new(spec.rate, ChannelLayout::Mono, spec.frames, Timestamp::ZERO)?;
//! let music_block = spec.buffer(Timestamp::ZERO)?;
//! let mut out = spec.buffer(Timestamp::ZERO)?;
//!
//! let report = mixer.mix([(voice.id(), &mic_block), (music.id(), &music_block)], &mut out)?;
//! assert_eq!(report.mixed, 2);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Small `Copy` value types with pure getters, as in voltra-core.
#![allow(clippy::must_use_candidate)]

// Parameters cross threads as 32- and 64-bit atomics. On a target where those
// are emulated with locks, the "never blocks" promise would silently be false.
#[cfg(not(all(target_has_atomic = "32", target_has_atomic = "64")))]
compile_error!("voltra-audio needs lock-free 32- and 64-bit atomics");

mod error;
mod gain;
mod kernel;
mod mixer;
mod track;

pub use error::{Error, Result};
pub use gain::{Balance, Gain};
pub use mixer::{MixReport, MixSpec, MixTotals, Mixer};
pub use track::{TrackControl, TrackId};
