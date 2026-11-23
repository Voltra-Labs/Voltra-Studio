//! Audio input: from a source's thread to the mix's tick.
//!
//! The design against libobs is in `docs/references/audio-sync.md` and
//! `docs/plans/015-entrada-de-audio.md`.

mod reader;
mod ring;
mod spec;
mod stats;
mod writer;

pub use reader::{InputReader, PullOutcome};
pub use spec::{InputSpec, audio_input};
pub use stats::InputStats;
pub use writer::InputWriter;

#[cfg(test)]
mod tests;
