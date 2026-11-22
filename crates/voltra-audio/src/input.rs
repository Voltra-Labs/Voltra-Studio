//! Audio input: from a source's thread to the mix's tick.
//!
//! The design against libobs is in `docs/references/audio-sync.md` and
//! `docs/plans/015-entrada-de-audio.md`.

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "consumed by the input queue, next step")
)]
mod ring;
