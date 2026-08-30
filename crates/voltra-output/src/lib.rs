//! Muxers and transport: where finished frames go.
//!
//! The first destination is the simplest one that produces something a person
//! can watch — a YUV4MPEG2 file. Encoding (`voltra-encode`) and live transport
//! (RTMP, SRT, WHIP) land in later phases and will share this crate.
//!
//! There is deliberately no `Output` trait yet. libobs has one
//! (`obs_output_info`) covering file, RTMP and raw output alike, and Voltra will
//! grow the equivalent — but a trait derived from a single implementation
//! encodes that implementation's accidents. It arrives once the file muxer and
//! the live transport exist to disagree with each other. See
//! `docs/references/y4m.md` §3.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod y4m;

pub use y4m::{Y4mInterlacing, Y4mParams, Y4mWriter};
