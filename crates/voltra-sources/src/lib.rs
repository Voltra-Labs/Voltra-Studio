//! Sources and filters that need nothing from the operating system.
//!
//! These are the equivalent of the built-in plugins that ship with OBS: simple
//! content anyone can put on a canvas, and the test material the compositor is
//! verified against. Platform capture lives in `voltra-capture`; this crate
//! builds and runs anywhere, which is what makes it usable in CI.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod color_source;
mod opacity;

pub use color_source::ColorSource;
pub use opacity::OpacityFilter;
