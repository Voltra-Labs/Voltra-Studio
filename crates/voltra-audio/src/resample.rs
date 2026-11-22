//! Sample-rate conversion: an exact-ratio polyphase resampler.
//!
//! The design against libswresample, which OBS uses with its defaults, is in
//! `docs/references/audio-resampling.md` and `docs/plans/014-remuestreo.md`.

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "consumed by the resampler, next step")
)]
mod bank;
