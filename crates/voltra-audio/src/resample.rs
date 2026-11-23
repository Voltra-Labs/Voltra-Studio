//! Sample-rate conversion: an exact-ratio polyphase resampler.
//!
//! The design against libswresample, which OBS uses with its defaults, is in
//! `docs/references/audio-resampling.md` and `docs/plans/014-remuestreo.md`.

mod bank;
mod resampler;

pub use resampler::Resampler;
