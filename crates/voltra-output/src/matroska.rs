//! Matroska, written as a stream.
//!
//! The recording container (ADR 0005). `Segment` and every `Cluster` are
//! written with **unknown size**, so the writer never goes back and the file is
//! valid up to its last complete block at every moment: a crash leaves a
//! playable recording, not an unindexed one. See
//! `docs/references/recording.md` §4.

mod ebml;
mod writer;

pub use writer::{MatroskaWriter, TrackSpec};
