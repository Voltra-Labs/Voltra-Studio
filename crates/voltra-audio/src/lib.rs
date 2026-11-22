//! Audio processing: the real-time mixer.
//!
//! The mixer sums tracks into one output block with per-track gain, balance and
//! mute, and is built to be called from an audio callback: it will neither
//! allocate nor block, and one track failing will never silence another.
//! Nothing in this crate may take a lock — `clippy.toml` bans them.
//!
//! The vocabulary — [`AudioBuffer`](voltra_core::AudioBuffer), rates, layouts —
//! lives in `voltra-core`. The design against libobs is in
//! `docs/references/audio-mixer.md` and `docs/plans/012-mezclador.md`.
//!
//! This first step carries the parameter types and the laws that turn them into
//! per-channel gains; the mixer that applies them comes next.
//!
//! # Examples
//!
//! ```
//! use voltra_audio::{Balance, Gain};
//!
//! let fader = Gain::from_db(-12.0)?;
//! let pan = Balance::new(-0.25)?;
//! assert!(fader.linear() < 0.26 && pan.position() < 0.0);
//! # Ok::<(), voltra_audio::Error>(())
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

pub use error::{Error, Result};
pub use gain::{Balance, Gain};
