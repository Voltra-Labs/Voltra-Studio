//! A meter for any signal, typically the mix output.

use std::sync::Arc;

use voltra_core::{AudioBuffer, ChannelLayout};

use super::cell::LevelCells;
use super::levels::Levels;
use super::stats::peak_and_energy;

/// Whether a meter looks between samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PeakMode {
    /// The largest sample. Cheap, and can under-read by 3 dB or more.
    #[default]
    Sample,
    /// The largest sample *and* the largest value between samples, by 4×
    /// oversampling as ITU-R BS.1770 annex 2 describes. What an encoder will
    /// clip on; costs 48 multiply-adds per sample.
    True,
}

/// The audio-thread half of a meter.
///
/// [`process`](Self::process) never allocates and never blocks.
#[derive(Debug)]
pub struct Meter {
    layout: ChannelLayout,
    mode: PeakMode,
    cells: Arc<LevelCells>,
}

/// The reading half of a meter. One per meter: `Send`, not `Clone`, because
/// two readers would each take half the data.
#[derive(Debug)]
pub struct MeterReader {
    count: usize,
    mode: PeakMode,
    cells: Arc<LevelCells>,
}

impl Meter {
    /// A meter for blocks of `layout`, and its reader.
    pub fn new(layout: ChannelLayout, mode: PeakMode) -> (Self, MeterReader) {
        let cells = Arc::new(LevelCells::new());
        (
            Self {
                layout,
                mode,
                cells: Arc::clone(&cells),
            },
            MeterReader {
                count: layout.channels(),
                mode,
                cells,
            },
        )
    }

    /// The layout this meter expects.
    pub const fn layout(&self) -> ChannelLayout {
        self.layout
    }

    /// Measure one block. Returns `false`, measuring nothing, when the block's
    /// layout is not the meter's.
    ///
    /// Blocks are expected to be finite, as the mixer's output always is. A NaN
    /// never reaches the peak; it does make that block's RMS meaningless.
    pub fn process(&mut self, block: &AudioBuffer) -> bool {
        if block.layout() != self.layout {
            return false;
        }
        let frames = u32::try_from(block.frames()).unwrap_or(u32::MAX);
        for (channel, samples) in block.channels().enumerate() {
            let (peak, energy) = peak_and_energy(samples);
            let true_peak = match self.mode {
                PeakMode::Sample => 0.0,
                PeakMode::True => peak,
            };
            self.cells.add(channel, peak, true_peak, energy, frames);
        }
        self.cells.add_frames(frames);
        true
    }
}

impl MeterReader {
    /// Everything measured since the previous call, and reset.
    pub fn take(&mut self) -> Levels {
        Levels::take(&self.cells, self.count, self.mode == PeakMode::True)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

    use super::{Meter, PeakMode};

    fn block(layout: ChannelLayout, frames: usize) -> AudioBuffer {
        AudioBuffer::new(SampleRate::HZ_48000, layout, frames, Timestamp::ZERO).unwrap()
    }

    #[test]
    fn a_sine_reads_its_amplitude_and_rms() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::Sample);
        let mut sine = block(ChannelLayout::Mono, 960);
        for (k, sample) in sine.channel_mut(0).unwrap().iter_mut().enumerate() {
            // 500 Hz at 48 kHz: 960 samples are exactly 10 periods, so the
            // RMS has no partial-period error.
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / 48_000.0;
            *sample = 0.8 * (2.0 * std::f32::consts::PI * 500.0 * t).sin();
        }
        assert!(meter.process(&sine));
        let levels = reader.take();
        let mono = levels.channel(0).unwrap();
        assert!((mono.peak - 0.8).abs() < 1e-3, "peak {}", mono.peak);
        assert!(
            (mono.rms - 0.8 / 2f32.sqrt()).abs() < 1e-4,
            "rms {}",
            mono.rms
        );
        assert_eq!(mono.true_peak, None);
    }

    #[test]
    fn a_constant_reads_the_same_peak_and_rms() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
        let mut constant = block(ChannelLayout::Stereo, 100);
        constant.as_mut_slice().fill(-0.25);
        meter.process(&constant);
        let levels = reader.take();
        for channel in levels.channels() {
            assert_eq!(channel.peak, 0.25);
            assert!((channel.rms - 0.25).abs() < 1e-6);
        }
        assert_eq!(levels.channel_count(), 2);
        assert!(levels.channel(2).is_none());
    }

    #[test]
    fn no_peak_is_lost_between_reads() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
        let silence = block(ChannelLayout::Stereo, 1024);
        let mut click = silence.clone();
        click.channel_mut(1).unwrap()[500] = 0.9;
        for index in 0..100 {
            meter.process(if index == 37 { &click } else { &silence });
        }
        let levels = reader.take();
        assert_eq!(levels.frames(), 100 * 1024);
        assert_eq!(levels.channel(1).unwrap().peak, 0.9);
        assert_eq!(levels.channel(0).unwrap().peak, 0.0);
    }

    #[test]
    fn rms_spans_every_block_since_the_last_read() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::Sample);
        let mut loud = block(ChannelLayout::Mono, 100);
        loud.as_mut_slice().fill(1.0);
        let quiet = block(ChannelLayout::Mono, 100);
        meter.process(&loud);
        meter.process(&quiet);
        // Half the time at 1, half at 0: mean square 0.5.
        let rms = reader.take().channel(0).unwrap().rms;
        assert!((rms - 0.5f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn a_block_of_another_layout_is_ignored() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
        assert!(!meter.process(&block(ChannelLayout::Mono, 64)));
        assert_eq!(reader.take().frames(), 0);
    }

    #[test]
    fn true_peak_mode_never_reads_below_the_sample_peak() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::True);
        let mut constant = block(ChannelLayout::Mono, 64);
        constant.as_mut_slice().fill(0.5);
        meter.process(&constant);
        let mono = *reader.take().channel(0).unwrap();
        assert!(mono.true_peak.unwrap() >= mono.peak);
    }

    #[test]
    fn decibels() {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Mono, PeakMode::Sample);
        let mut half = block(ChannelLayout::Mono, 10);
        half.as_mut_slice().fill(0.5);
        meter.process(&half);
        let mono = *reader.take().channel(0).unwrap();
        assert!((mono.peak_db() + 6.0206).abs() < 1e-3);
        assert_eq!(
            reader.take().channel(0).unwrap().peak_db(),
            f32::NEG_INFINITY
        );
    }
}
