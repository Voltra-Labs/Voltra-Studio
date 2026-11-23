//! What a reader gets: levels per channel since the last read.

use voltra_core::MAX_CHANNELS;

use super::cell::LevelCells;

/// The levels of one channel over one reading, as linear amplitudes.
///
/// `0.0` is silence and `1.0` full scale; a sum above unity reads above `1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChannelLevels {
    /// The largest sample magnitude.
    pub peak: f32,
    /// The largest magnitude between samples too (ITU-R BS.1770 annex 2), when
    /// the meter measures it. Never below [`peak`](Self::peak).
    pub true_peak: Option<f32>,
    /// The RMS over the reading.
    pub rms: f32,
}

impl ChannelLevels {
    /// The highest peak measured: the true peak when there is one.
    pub fn highest_peak(&self) -> f32 {
        self.true_peak.unwrap_or(self.peak)
    }

    /// [`peak`](Self::peak) in dBFS; `-inf` for silence.
    pub fn peak_db(&self) -> f32 {
        to_db(self.peak)
    }

    /// [`rms`](Self::rms) in dBFS; `-inf` for silence.
    pub fn rms_db(&self) -> f32 {
        to_db(self.rms)
    }

    /// [`true_peak`](Self::true_peak) in dB TP.
    pub fn true_peak_db(&self) -> Option<f32> {
        self.true_peak.map(to_db)
    }
}

/// A linear amplitude in decibels; `-inf` for zero.
pub(crate) fn to_db(amplitude: f32) -> f32 {
    20.0 * amplitude.log10()
}

/// Levels per channel accumulated since the previous reading.
///
/// # Examples
///
/// ```
/// use voltra_audio::{Meter, PeakMode};
/// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
///
/// let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
/// let mut block = AudioBuffer::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 1024, Timestamp::ZERO)?;
/// block.channel_mut(0).unwrap().fill(0.5);
/// meter.process(&block);
///
/// let levels = reader.take();
/// assert_eq!(levels.frames(), 1024);
/// assert_eq!(levels.channel(0).map(|left| left.peak), Some(0.5));
/// assert_eq!(levels.channel(1).map(|right| right.rms), Some(0.0));
/// // Nothing new since: an empty reading.
/// assert_eq!(reader.take().frames(), 0);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    frames: u64,
    count: usize,
    channels: [ChannelLevels; MAX_CHANNELS],
}

impl Levels {
    /// Take and reset `count` channels from `cells`.
    pub(crate) fn take(cells: &LevelCells, count: usize, true_peak: bool) -> Self {
        // Duration first: a block counted here has had all its channels added,
        // so the channels taken below include at least everything it covers.
        let frames = cells.take_frames();
        let mut channels = [ChannelLevels::default(); MAX_CHANNELS];
        for (index, slot) in channels.iter_mut().enumerate().take(count) {
            let taken = cells.take(index);
            #[allow(
                clippy::cast_precision_loss,
                reason = "a frame count only scales the energy"
            )]
            let rms = if taken.frames == 0 {
                0.0
            } else {
                (taken.energy / taken.frames as f32).sqrt()
            };
            *slot = ChannelLevels {
                peak: taken.peak,
                true_peak: true_peak.then_some(taken.true_peak.max(taken.peak)),
                rms,
            };
        }
        Self {
            frames,
            count,
            channels,
        }
    }

    /// How many samples per channel this reading covers. Zero when no audio
    /// arrived since the previous one.
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many channels.
    pub const fn channel_count(&self) -> usize {
        self.count
    }

    /// One channel's levels.
    pub fn channel(&self, index: usize) -> Option<&ChannelLevels> {
        self.channels().get(index)
    }

    /// Every channel's levels.
    pub fn channels(&self) -> &[ChannelLevels] {
        self.channels.get(..self.count).unwrap_or(&[])
    }
}
