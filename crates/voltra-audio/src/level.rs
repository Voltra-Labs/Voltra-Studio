//! What a meter reads, and how it crosses threads.
//!
//! The audio thread computes two cheap numbers per channel per block — the peak
//! and the RMS — and publishes them. Nothing here decides how they are
//! *displayed*: no decay, no peak hold, no smoothing.
//!
//! That is not an omission, it is where libobs draws the same line
//! (`docs/references/audio.md` §5): `obs_volmeter` computes peak and magnitude
//! and stops, and every ballistic constant lives in the OBS interface. Decay is
//! presentation policy, and presentation policy has no business on a real-time
//! thread.
//!
//! # Crossing the thread boundary
//!
//! Each channel's pair of numbers travels in **one** `AtomicU64`: the two `f32`
//! bit patterns packed into a single word. So a reader can never see a peak from
//! one block beside an RMS from another.
//!
//! Across channels it still can — channel 0 might come from block *n* and
//! channel 1 from block *n+1*. That is left alone deliberately. Closing it would
//! need a sequence lock and a retry loop on the audio thread, to fix a 21 ms
//! skew between two bars on a screen.

use std::sync::atomic::{AtomicU64, Ordering};

use voltra_core::MAX_CHANNELS;

use crate::gain::Gain;

/// One channel's level over one block, as linear amplitude.
///
/// Linear rather than decibels: converting is a logarithm per channel per block
/// on the audio thread, and whoever draws the meter has to map to pixels
/// anyway. [`ChannelLevel::peak_db`] is there when a number for a human is
/// wanted.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChannelLevel {
    /// The largest absolute sample in the block.
    pub peak: f32,
    /// The root mean square of the block.
    pub rms: f32,
}

impl ChannelLevel {
    /// Silence.
    pub const SILENT: ChannelLevel = ChannelLevel {
        peak: 0.0,
        rms: 0.0,
    };

    /// The peak in decibels relative to full scale, `-inf` for silence.
    #[must_use]
    pub fn peak_db(self) -> f32 {
        Gain::from_linear(self.peak).unwrap_or(Gain::SILENT).db()
    }

    /// The RMS in decibels relative to full scale, `-inf` for silence.
    #[must_use]
    pub fn rms_db(self) -> f32 {
        Gain::from_linear(self.rms).unwrap_or(Gain::SILENT).db()
    }

    /// Whether the block reached or passed full scale.
    ///
    /// The mixer does not clip — that happens once, at the edge conversion — so
    /// this is the warning that it is about to.
    #[must_use]
    pub fn is_clipping(self) -> bool {
        self.peak >= 1.0
    }

    fn pack(self) -> u64 {
        (u64::from(self.peak.to_bits()) << 32) | u64::from(self.rms.to_bits())
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the two halves are extracted deliberately, not narrowed"
    )]
    fn unpack(word: u64) -> Self {
        Self {
            peak: f32::from_bits((word >> 32) as u32),
            rms: f32::from_bits(word as u32),
        }
    }
}

/// Levels for every channel of one track or of the mix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    channels: [ChannelLevel; MAX_CHANNELS],
    count: usize,
}

impl Levels {
    /// Silence across `count` channels.
    #[must_use]
    pub fn silent(count: usize) -> Self {
        Self {
            channels: [ChannelLevel::SILENT; MAX_CHANNELS],
            count: count.min(MAX_CHANNELS),
        }
    }

    /// How many channels these levels describe.
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.count
    }

    /// One channel's level.
    #[must_use]
    pub fn channel(&self, index: usize) -> Option<ChannelLevel> {
        if index >= self.count {
            return None;
        }
        self.channels.get(index).copied()
    }

    /// Every channel's level, in order.
    pub fn channels(&self) -> impl Iterator<Item = ChannelLevel> + '_ {
        self.channels[..self.count].iter().copied()
    }

    /// The loudest peak across all channels.
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.channels()
            .fold(0.0f32, |worst, level| worst.max(level.peak))
    }

    /// Whether any channel reached full scale.
    #[must_use]
    pub fn is_clipping(&self) -> bool {
        self.channels().any(ChannelLevel::is_clipping)
    }
}

impl Default for Levels {
    fn default() -> Self {
        Levels::silent(0)
    }
}

/// Where the audio thread leaves levels for anyone else to read.
///
/// One `AtomicU64` per channel. Writing is one relaxed store per channel per
/// block; reading is one relaxed load. Neither side ever waits.
#[derive(Debug)]
pub(crate) struct LevelPublisher {
    channels: [AtomicU64; MAX_CHANNELS],
}

impl LevelPublisher {
    pub(crate) fn new() -> Self {
        Self {
            channels: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Publish one channel's level.
    pub(crate) fn publish(&self, index: usize, level: ChannelLevel) {
        if let Some(slot) = self.channels.get(index) {
            slot.store(level.pack(), Ordering::Relaxed);
        }
    }

    /// Publish silence for every channel, for a track that contributed nothing.
    pub(crate) fn publish_silence(&self, count: usize) {
        for index in 0..count.min(MAX_CHANNELS) {
            self.publish(index, ChannelLevel::SILENT);
        }
    }

    /// Read back the levels for `count` channels.
    pub(crate) fn read(&self, count: usize) -> Levels {
        let count = count.min(MAX_CHANNELS);
        let mut levels = Levels::silent(count);
        for index in 0..count {
            if let Some(slot) = self.channels.get(index) {
                levels.channels[index] = ChannelLevel::unpack(slot.load(Ordering::Relaxed));
            }
        }
        levels
    }
}

/// The running peak and sum of squares for one channel.
///
/// Accumulated inside the mix loop so metering costs one pass, not two.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LevelAccumulator {
    peak: f32,
    sum_of_squares: f32,
}

impl LevelAccumulator {
    /// Fold one sample in.
    ///
    /// Two things here are the opposite of what the rest of this codebase would
    /// suggest, and both were measured (`docs/PERFORMANCE.md` §3.8):
    ///
    /// - **The branch stays.** `self.peak.max(magnitude)` looks branchless, but
    ///   Rust's `f32::max` carries IEEE NaN semantics and does not compile to a
    ///   single instruction; it measured **1.8× slower** than this comparison.
    ///   Plan 008's branchless-clamp lesson does not transfer.
    /// - **No `mul_add`.** Without hardware FMA it is a libm call per sample —
    ///   3× slower here, and 12× in the mixer (§3.7).
    #[inline]
    pub(crate) fn push(&mut self, sample: f32) {
        let magnitude = sample.abs();
        if magnitude > self.peak {
            self.peak = magnitude;
        }
        self.sum_of_squares += sample * sample;
    }

    /// The level over `frames` samples.
    pub(crate) fn finish(self, frames: usize) -> ChannelLevel {
        if frames == 0 {
            return ChannelLevel::SILENT;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "a block length is far below 2^24"
        )]
        let mean = self.sum_of_squares / frames as f32;
        ChannelLevel {
            peak: self.peak,
            rms: mean.sqrt(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelLevel, LevelAccumulator, LevelPublisher, Levels};

    #[test]
    fn a_level_survives_the_round_trip_through_a_word() {
        let publisher = LevelPublisher::new();
        let level = ChannelLevel {
            peak: 0.75,
            rms: 0.125,
        };
        publisher.publish(1, level);

        let read = publisher.read(2);
        assert_eq!(read.channel(0), Some(ChannelLevel::SILENT));
        assert_eq!(read.channel(1), Some(level));
        assert_eq!(read.channel(2), None);
    }

    /// The peak is the largest absolute sample, sign ignored.
    #[test]
    fn the_peak_is_the_largest_magnitude() {
        let mut accumulator = LevelAccumulator::default();
        for sample in [0.1, -0.8, 0.3, -0.2] {
            accumulator.push(sample);
        }
        let level = accumulator.finish(4);
        assert!((level.peak - 0.8).abs() < 1e-6);
    }

    /// A square wave of amplitude 1 has an RMS of exactly 1: every sample is
    /// full scale, so the mean of the squares is 1.
    #[test]
    fn a_square_wave_has_unit_rms() {
        let mut accumulator = LevelAccumulator::default();
        for index in 0..64 {
            accumulator.push(if index % 2 == 0 { 1.0 } else { -1.0 });
        }
        let level = accumulator.finish(64);
        assert!((level.rms - 1.0).abs() < 1e-6, "rms was {}", level.rms);
        assert!((level.peak - 1.0).abs() < 1e-6);
    }

    /// The test that separates a real RMS from an average of absolute values:
    /// a sine of amplitude 1 has RMS 1/sqrt(2), while its mean magnitude is
    /// 2/pi ≈ 0.637.
    #[test]
    fn a_sine_has_the_rms_a_sine_should_have() {
        let mut accumulator = LevelAccumulator::default();
        let frames = 4_800;
        for index in 0..frames {
            #[allow(clippy::cast_precision_loss)]
            let phase = index as f32 / 100.0 * std::f32::consts::TAU;
            accumulator.push(phase.sin());
        }
        let level = accumulator.finish(frames);
        let expected = 1.0 / 2.0f32.sqrt();
        assert!(
            (level.rms - expected).abs() < 1e-3,
            "rms was {}, expected {expected}",
            level.rms
        );
    }

    #[test]
    fn silence_reports_nothing_at_all() {
        let mut accumulator = LevelAccumulator::default();
        for _ in 0..128 {
            accumulator.push(0.0);
        }
        let level = accumulator.finish(128);
        assert_eq!(level, ChannelLevel::SILENT);
        assert_eq!(level.peak_db(), f32::NEG_INFINITY);
        assert_eq!(level.rms_db(), f32::NEG_INFINITY);
        assert!(!level.is_clipping());
    }

    #[test]
    fn full_scale_is_reported_as_clipping() {
        let over = ChannelLevel {
            peak: 1.2,
            rms: 0.9,
        };
        assert!(over.is_clipping());
        assert!(over.peak_db() > 0.0);

        let under = ChannelLevel {
            peak: 0.999,
            rms: 0.5,
        };
        assert!(!under.is_clipping());
    }

    #[test]
    fn levels_summarise_across_channels() {
        let publisher = LevelPublisher::new();
        publisher.publish(
            0,
            ChannelLevel {
                peak: 0.2,
                rms: 0.1,
            },
        );
        publisher.publish(
            1,
            ChannelLevel {
                peak: 1.4,
                rms: 0.9,
            },
        );

        let levels = publisher.read(2);
        assert!((levels.peak() - 1.4).abs() < 1e-6);
        assert!(levels.is_clipping());
        assert_eq!(levels.channels().count(), 2);
        assert_eq!(levels.channel_count(), 2);
    }

    #[test]
    fn silence_can_be_published_wholesale() {
        let publisher = LevelPublisher::new();
        publisher.publish(
            0,
            ChannelLevel {
                peak: 0.5,
                rms: 0.5,
            },
        );
        publisher.publish_silence(2);
        assert_eq!(publisher.read(2), Levels::silent(2));
    }
}
