//! Capture: frames from the system, handed to the compositor.
//!
//! Every capture backend — a screen through `PipeWire`, a window, a camera, and
//! one day `Windows.Graphics.Capture` or `ScreenCaptureKit` — produces frames on a
//! thread of its own, at a rate of its own. The compositor consumes them on the
//! render thread, at the output's rate. This crate owns the edge between the
//! two, which every backend shares (ADR 0006):
//!
//! - **The latest frame wins.** A capture frame that arrives late is worth
//!   nothing; holding on to it only adds latency. A wait-free triple buffer
//!   hands the newest one over: neither thread ever waits for the other.
//! - **The producer writes in place.** Its buffer comes back to it after being
//!   read, so publishing a frame neither copies it nor allocates.
//! - **Every loss is counted.** A frame overwritten before anyone read it is a
//!   drop; a render tick with nothing new is a repeat. With frames captured,
//!   shown and dropped counted, `captured == shown + dropped + pending` holds,
//!   with at most one pending.
//!
//! See `docs/references/capture.md` for how OBS, Windows and macOS do it.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

#[cfg(any(feature = "pipewire", test))]
mod clock;
mod convert;
mod delivery;
#[cfg(feature = "pipewire")]
mod pipewire;
mod source;
mod test_pattern;

pub use convert::{SourceFormat, copy_to_bgra};
pub use delivery::{CaptureStats, FrameReader, FrameWriter, frame_channel};
#[cfg(feature = "pipewire")]
pub use pipewire::{PipeWireCapture, PipeWireTarget};
pub use source::{BackendStats, Capture, CaptureSource, CaptureStatus};
pub use test_pattern::TestPatternCapture;
