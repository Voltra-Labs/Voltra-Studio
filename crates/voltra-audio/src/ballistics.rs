//! Ballistics: how meter readings become bars that move like a meter.
//!
//! This runs on the interface thread, not the audio thread. It is a pure
//! function of the readings and the times it is given, so it is testable without
//! clocks or threads. The defaults are OBS's, so a meter here moves the way an
//! OBS user expects (`docs/references/audio-meters.md` §4).
//!
//! Two clocks, on purpose:
//!
//! - **Peak decay and hold follow the caller's `elapsed`**, wall time between
//!   updates, so a peak falls even when no audio arrives.
//! - **RMS integrates over the audio the reading covers** — its frames divided
//!   by the sample rate. An interface polling at 60 Hz while blocks arrive at
//!   47 Hz gets many empty readings; integrating those as silence would make
//!   the bar flicker down. A track that stops sounding still falls, because the
//!   mixer records its missing blocks as silence.
//!
//! RMS integrates **power**, with the exact exponential
//! `y ← y·a + (1 − a)·x`, `a = e^(−t/τ)`. It is stable for any step and gives
//! the same result however a span of audio is split into readings. OBS
//! integrates decibels with an Euler step that overshoots past 0.3 s and
//! diverges past 0.6 s (§4).

use std::time::Duration;

use voltra_core::{MAX_CHANNELS, SampleRate};

use crate::meter::{Levels, to_db};

/// How the bars move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BallisticsConfig {
    /// How fast a peak falls, in dB per second, once the signal drops.
    pub peak_decay_db_per_s: f32,
    /// How long the hold marker stays at the highest peak.
    pub peak_hold: Duration,
    /// How long RMS takes to reach 99 % of a new level's power.
    pub rms_integration: Duration,
}

impl BallisticsConfig {
    /// OBS's defaults: a Type I PPM decay of 20 dB in 1.7 s, a 20 s hold and
    /// 99 % in 300 ms.
    pub const OBS: Self = Self {
        peak_decay_db_per_s: 11.76,
        peak_hold: Duration::from_secs(20),
        rms_integration: Duration::from_millis(300),
    };
}

impl Default for BallisticsConfig {
    fn default() -> Self {
        Self::OBS
    }
}

/// What to draw for one channel, in dBFS. `-inf` is silence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelDisplay {
    /// The peak bar: immediate attack, linear decay in dB.
    pub peak_db: f32,
    /// The hold marker: the highest peak of the last
    /// [`peak_hold`](BallisticsConfig::peak_hold).
    pub hold_db: f32,
    /// The RMS bar.
    pub rms_db: f32,
    /// A peak reached full scale since the last
    /// [`reset_clip`](Ballistics::reset_clip).
    pub clipped: bool,
}

impl Default for ChannelDisplay {
    fn default() -> Self {
        Self {
            peak_db: f32::NEG_INFINITY,
            hold_db: f32::NEG_INFINITY,
            rms_db: f32::NEG_INFINITY,
            clipped: false,
        }
    }
}

/// What to draw for every channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterDisplay {
    count: usize,
    channels: [ChannelDisplay; MAX_CHANNELS],
}

impl MeterDisplay {
    /// One channel.
    pub fn channel(&self, index: usize) -> Option<&ChannelDisplay> {
        self.channels().get(index)
    }

    /// Every channel.
    pub fn channels(&self) -> &[ChannelDisplay] {
        self.channels.get(..self.count).unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Copy)]
struct ChannelState {
    peak_db: f32,
    hold_db: f32,
    hold_age: Duration,
    power: f32,
    clipped: bool,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            peak_db: f32::NEG_INFINITY,
            hold_db: f32::NEG_INFINITY,
            hold_age: Duration::ZERO,
            power: 0.0,
            clipped: false,
        }
    }
}

/// Turns successive [`Levels`] into what a meter shows.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use voltra_audio::{Ballistics, BallisticsConfig, Meter, PeakMode};
/// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
///
/// let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::Sample);
/// let mut ballistics = Ballistics::new(BallisticsConfig::OBS, SampleRate::HZ_48000, 1);
///
/// let mut block = AudioBuffer::new(SampleRate::HZ_48000, ChannelLayout::Mono, 1024, Timestamp::ZERO)?;
/// block.as_mut_slice().fill(0.5);
/// meter.process(&block);
///
/// // Each repaint: take what the audio thread measured, say how long it has been.
/// let shown = ballistics.update(&reader.take(), Duration::from_millis(16));
/// let mono = shown.channel(0).unwrap();
/// assert!((mono.peak_db + 6.02).abs() < 0.01, "peaks attack at once");
/// assert!(mono.rms_db < mono.peak_db, "RMS integrates towards it");
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Ballistics {
    config: BallisticsConfig,
    rate: SampleRate,
    /// `1 / τ`, with `τ` such that `1 − e^(−T/τ) = 0.99` at `T = rms_integration`.
    inverse_tau: f32,
    count: usize,
    channels: [ChannelState; MAX_CHANNELS],
}

impl Ballistics {
    /// Ballistics for `channels` channels of audio at `rate`.
    pub fn new(config: BallisticsConfig, rate: SampleRate, channels: usize) -> Self {
        let integration = config.rms_integration.as_secs_f32();
        Self {
            config,
            rate,
            inverse_tau: if integration > 0.0 {
                100f32.ln() / integration
            } else {
                f32::INFINITY
            },
            count: channels.min(MAX_CHANNELS),
            channels: [ChannelState::default(); MAX_CHANNELS],
        }
    }

    /// Fold in one reading, `elapsed` after the previous update.
    pub fn update(&mut self, levels: &Levels, elapsed: Duration) -> MeterDisplay {
        let seconds = elapsed.as_secs_f32();
        let decay = self.config.peak_decay_db_per_s * seconds;

        #[allow(
            clippy::cast_precision_loss,
            reason = "a duration in frames, as seconds"
        )]
        let audio = levels.frames() as f32 / self.rate.hz() as f32;
        // `a = e^(−t/τ)`; with no audio in this reading, `a = 1` and RMS holds.
        let keep = (-audio * self.inverse_tau).exp();

        let mut shown = [ChannelDisplay::default(); MAX_CHANNELS];
        for (index, (state, display)) in self
            .channels
            .iter_mut()
            .zip(shown.iter_mut())
            .enumerate()
            .take(self.count)
        {
            let reading = levels.channel(index).copied().unwrap_or_default();
            let incoming = to_db(reading.highest_peak());

            // Peak: immediate attack, linear decay in dB.
            let decayed = state.peak_db - decay;
            state.peak_db = if incoming > decayed {
                incoming
            } else {
                decayed
            };

            // Hold: the highest peak, until it is `peak_hold` old.
            if incoming >= state.hold_db {
                state.hold_db = incoming;
                state.hold_age = Duration::ZERO;
            } else {
                state.hold_age += elapsed;
                if state.hold_age > self.config.peak_hold {
                    state.hold_db = state.peak_db;
                    state.hold_age = Duration::ZERO;
                }
            }

            // RMS: power, exact exponential over the reading's audio.
            if levels.frames() > 0 {
                let power = reading.rms * reading.rms;
                state.power = state.power * keep + (1.0 - keep) * power;
            }

            state.clipped |= reading.highest_peak() >= 1.0;

            *display = ChannelDisplay {
                peak_db: state.peak_db,
                hold_db: state.hold_db,
                rms_db: 10.0 * state.power.log10(),
                clipped: state.clipped,
            };
        }
        MeterDisplay {
            count: self.count,
            channels: shown,
        }
    }

    /// Clear every channel's clip indicator.
    ///
    /// It stays lit until this is called. OBS turns its own off after a second;
    /// in a live broadcast, a clip nobody saw should not erase itself.
    pub fn reset_clip(&mut self) {
        for state in &mut self.channels {
            state.clipped = false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use std::time::Duration;

    use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

    use super::{Ballistics, BallisticsConfig};
    use crate::{Levels, Meter, PeakMode};

    /// A reading of `frames` mono samples at `level`, or an empty one.
    fn reading(level: f32, frames: usize) -> Levels {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::Sample);
        if frames > 0 {
            let mut block = AudioBuffer::new(
                SampleRate::HZ_48000,
                ChannelLayout::Mono,
                frames,
                Timestamp::ZERO,
            )
            .unwrap();
            block.as_mut_slice().fill(level);
            meter.process(&block);
        }
        reader.take()
    }

    fn mono() -> Ballistics {
        Ballistics::new(BallisticsConfig::OBS, SampleRate::HZ_48000, 1)
    }

    #[test]
    fn a_peak_falls_at_the_decay_rate_in_one_step_or_a_hundred() {
        let empty = reading(0.0, 0);
        let mut once = mono();
        once.update(&reading(1.0, 48), Duration::ZERO);
        let one_step = once
            .update(&empty, Duration::from_secs(1))
            .channel(0)
            .unwrap()
            .peak_db;

        let mut often = mono();
        often.update(&reading(1.0, 48), Duration::ZERO);
        let mut many_steps = 0.0;
        for _ in 0..100 {
            many_steps = often
                .update(&empty, Duration::from_millis(10))
                .channel(0)
                .unwrap()
                .peak_db;
        }
        assert!((one_step + 11.76).abs() < 1e-4, "{one_step}");
        assert!((many_steps + 11.76).abs() < 1e-3, "{many_steps}");
    }

    #[test]
    fn a_louder_peak_attacks_at_once() {
        let mut ballistics = mono();
        ballistics.update(&reading(0.1, 48), Duration::ZERO);
        let shown = ballistics.update(&reading(0.5, 48), Duration::from_millis(16));
        assert!((shown.channel(0).unwrap().peak_db + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn the_hold_stays_twenty_seconds_then_follows_the_peak() {
        let empty = reading(0.0, 0);
        let mut ballistics = mono();
        ballistics.update(&reading(1.0, 48), Duration::ZERO);
        let mut shown = ballistics.update(&empty, Duration::from_secs(19));
        assert_eq!(
            shown.channel(0).unwrap().hold_db,
            0.0,
            "still holding at 19 s"
        );
        shown = ballistics.update(&empty, Duration::from_secs(2));
        let channel = shown.channel(0).unwrap();
        assert_eq!(channel.hold_db, channel.peak_db, "released at 21 s");
        assert!(channel.hold_db < -200.0);
    }

    #[test]
    fn rms_reaches_99_percent_of_the_power_in_300_ms() {
        let mut ballistics = mono();
        // 300 ms at 48 kHz is 14 400 frames.
        let shown = ballistics.update(&reading(1.0, 14_400), Duration::from_millis(300));
        let power = 10f32.powf(shown.channel(0).unwrap().rms_db / 10.0);
        assert!((power - 0.99).abs() < 1e-5, "{power}");
    }

    #[test]
    fn rms_does_not_depend_on_how_audio_is_split_into_readings() {
        let mut once = mono();
        let whole = once.update(&reading(0.7, 14_400), Duration::from_millis(300));
        let mut tenths = mono();
        let mut parts = whole;
        for _ in 0..10 {
            parts = tenths.update(&reading(0.7, 1_440), Duration::from_millis(30));
        }
        let (a, b) = (
            whole.channel(0).unwrap().rms_db,
            parts.channel(0).unwrap().rms_db,
        );
        assert!((a - b).abs() < 1e-3, "{a} vs {b} dB");
    }

    #[test]
    fn a_long_step_never_overshoots() {
        let mut ballistics = mono();
        let shown = ballistics.update(&reading(0.5, 240_000), Duration::from_secs(5));
        let rms = shown.channel(0).unwrap().rms_db;
        assert!(rms <= -6.0206 + 1e-4 && rms > -6.03, "{rms}");
    }

    #[test]
    fn empty_readings_hold_the_rms() {
        let mut ballistics = mono();
        let before = ballistics.update(&reading(0.5, 4_800), Duration::from_millis(100));
        let after = ballistics.update(&reading(0.0, 0), Duration::from_millis(100));
        assert_eq!(
            before.channel(0).unwrap().rms_db,
            after.channel(0).unwrap().rms_db
        );
    }

    #[test]
    fn full_scale_lights_the_clip_until_reset() {
        let mut ballistics = mono();
        assert!(
            !ballistics
                .update(&reading(0.99, 48), Duration::ZERO)
                .channel(0)
                .unwrap()
                .clipped
        );
        assert!(
            ballistics
                .update(&reading(1.0, 48), Duration::ZERO)
                .channel(0)
                .unwrap()
                .clipped
        );
        let later = ballistics.update(&reading(0.0, 48_000), Duration::from_secs(60));
        assert!(
            later.channel(0).unwrap().clipped,
            "a clip does not erase itself"
        );
        ballistics.reset_clip();
        assert!(
            !ballistics
                .update(&reading(0.0, 48), Duration::ZERO)
                .channel(0)
                .unwrap()
                .clipped
        );
    }

    #[test]
    fn silence_reads_minus_infinity() {
        let mut ballistics = mono();
        let shown = ballistics.update(&reading(0.0, 0), Duration::ZERO);
        let channel = shown.channel(0).unwrap();
        assert_eq!(channel.peak_db, f32::NEG_INFINITY);
        assert_eq!(channel.rms_db, f32::NEG_INFINITY);
        assert!(shown.channel(1).is_none());
    }
}
