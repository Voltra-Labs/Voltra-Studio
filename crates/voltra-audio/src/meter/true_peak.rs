//! True peak: the largest magnitude between samples, by 4× oversampling.
//!
//! The filter is the one ITU-R BS.1770-5 annex 2 gives: order 48, four phases
//! of twelve taps. Its phases interpolate at 1/8, 3/8, 5/8 and 7/8 of a sample;
//! none reproduces the sample itself, so the meter takes the larger of this and
//! the sample peak (`docs/references/audio-meters.md` §3.2). The standard's
//! 12.04 dB pre-attenuation exists for integer arithmetic and, as the standard
//! says, is unnecessary in float.
//!
//! With tones from 100 Hz to 20 kHz at 48 kHz, the meter — this filter and the
//! samples — reads at most 0.11 dB under and 0.17 dB over the true amplitude.
//! The filter alone under-reads by up to 0.20 dB; taking the samples too halves
//! that. It is not flat, and over-reading is the safe side for a peak meter.

use voltra_core::MAX_CHANNELS;

const TAPS: usize = 12;
const PHASES: usize = 4;
/// Samples of the previous block each channel keeps, to interpolate across the
/// boundary between blocks.
const HISTORY: usize = TAPS - 1;

/// BS.1770-5 annex 2, transposed: row `k` holds tap `k` of phases 0 to 3, so
/// one window sample multiplies a four-lane row and the inner loop is a
/// four-wide multiply-add.
///
/// Phase 3 is phase 0 reversed and phase 2 is phase 1 reversed, so which end of
/// the window counts as "now" only permutes the phases; the maximum over all
/// four is the same either way.
#[rustfmt::skip]
#[allow(
    clippy::excessive_precision,
    reason = "copied digit for digit from the standard; every value is a multiple \
              of 2^-16 and f32 holds it exactly, which a test checks"
)]
const ROWS: [[f32; PHASES]; TAPS] = [
    [ 0.001_708_984_375_0, -0.029_174_804_687_5, -0.018_920_898_437_5, -0.008_300_781_250_0],
    [ 0.010_986_328_125_0,  0.029_296_875_000_0,  0.033_081_054_687_5,  0.014_892_578_125_0],
    [-0.019_653_320_312_5, -0.051_757_812_500_0, -0.058_227_539_062_5, -0.026_611_328_125_0],
    [ 0.033_203_125_000_0,  0.089_111_328_125_0,  0.101_562_500_000_0,  0.047_607_421_875_0],
    [-0.059_448_242_187_5, -0.166_503_906_250_0, -0.200_317_382_812_5, -0.102_294_921_875_0],
    [ 0.137_329_101_562_5,  0.465_087_890_625_0,  0.779_785_156_250_0,  0.972_167_968_750_0],
    [ 0.972_167_968_750_0,  0.779_785_156_250_0,  0.465_087_890_625_0,  0.137_329_101_562_5],
    [-0.102_294_921_875_0, -0.200_317_382_812_5, -0.166_503_906_250_0, -0.059_448_242_187_5],
    [ 0.047_607_421_875_0,  0.101_562_500_000_0,  0.089_111_328_125_0,  0.033_203_125_000_0],
    [-0.026_611_328_125_0, -0.058_227_539_062_5, -0.051_757_812_500_0, -0.019_653_320_312_5],
    [ 0.014_892_578_125_0,  0.033_081_054_687_5,  0.029_296_875_000_0,  0.010_986_328_125_0],
    [-0.008_300_781_250_0, -0.018_920_898_437_5, -0.029_174_804_687_5,  0.001_708_984_375_0],
];

/// Per-channel filter state across blocks.
#[derive(Debug, Clone)]
pub(crate) struct TruePeak {
    history: [[f32; HISTORY]; MAX_CHANNELS],
}

impl TruePeak {
    pub(crate) const fn new() -> Self {
        Self {
            history: [[0.0; HISTORY]; MAX_CHANNELS],
        }
    }

    /// The largest interpolated magnitude in `samples`, continuing `channel`'s
    /// history, which it then updates. Every sample is covered, whatever the
    /// block length. An out-of-range channel reads zero.
    pub(crate) fn measure(&mut self, channel: usize, samples: &[f32]) -> f32 {
        let Some(history) = self.history.get_mut(channel) else {
            return 0.0;
        };

        // The first `HISTORY` outputs need the previous block: interpolate them
        // over a small seam of history followed by the block's first samples.
        let head = samples.len().min(HISTORY);
        let mut seam = [0.0f32; 2 * HISTORY];
        seam[..HISTORY].copy_from_slice(history);
        seam[HISTORY..HISTORY + head].copy_from_slice(&samples[..head]);
        let mut lanes = window_peak([0.0; PHASES], &seam[..HISTORY + head]);

        // The rest lie entirely inside the block.
        lanes = window_peak(lanes, samples);

        // Keep the last `HISTORY` samples of history-then-block.
        if samples.len() >= HISTORY {
            history.copy_from_slice(&samples[samples.len() - HISTORY..]);
        } else {
            history.copy_from_slice(&seam[head..head + HISTORY]);
        }

        lanes.iter().fold(0.0f32, |a, &b| if b > a { b } else { a })
    }
}

/// Fold the four phases' magnitudes over every full window of `signal` into
/// `lanes`, a running maximum per phase.
fn window_peak(mut lanes: [f32; PHASES], signal: &[f32]) -> [f32; PHASES] {
    for window in signal.windows(TAPS) {
        let mut sums = [0.0f32; PHASES];
        for (&sample, row) in window.iter().zip(&ROWS) {
            for (sum, &tap) in sums.iter_mut().zip(row) {
                *sum += sample * tap;
            }
        }
        for (lane, sum) in lanes.iter_mut().zip(sums) {
            let magnitude = sum.abs();
            *lane = if magnitude > *lane { magnitude } else { *lane };
        }
    }
    lanes
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::{HISTORY, TruePeak};

    /// A tone of amplitude 1 at `hz`, 48 kHz, starting at sample `start`.
    fn tone(hz: f64, phase: f64, start: usize, len: usize) -> Vec<f32> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        (start..start + len)
            .map(|k| (std::f64::consts::TAU * hz * k as f64 / 48_000.0 + phase).sin() as f32)
            .collect()
    }

    /// True peak of a steady tone in dB, past the onset transient, as the
    /// meter reports it: the larger of the filter and the samples.
    fn steady_true_peak_db(hz: f64, phase: f64) -> f32 {
        let mut meter = TruePeak::new();
        meter.measure(0, &tone(hz, phase, 0, 1024));
        let block = tone(hz, phase, 1024, 1024);
        let samples = block.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        20.0 * meter.measure(0, &block).max(samples).log10()
    }

    #[test]
    fn the_standards_unlucky_quarter_rate_tone() {
        // Samples at ±0.707: a sample-peak meter reads −3.01 dB.
        let samples = tone(12_000.0, std::f64::consts::FRAC_PI_4, 1024, 1024);
        let sample_peak = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!((20.0 * sample_peak.log10() + 3.01).abs() < 0.01);

        let true_peak = steady_true_peak_db(12_000.0, std::f64::consts::FRAC_PI_4);
        assert!((-0.25..=0.1).contains(&true_peak), "{true_peak} dB TP");
    }

    #[test]
    fn tones_up_to_20_khz_read_within_the_margins() {
        let mut lowest = f32::MAX;
        let mut highest = f32::MIN;
        for hz in [
            100.0, 997.0, 3_000.0, 5_000.0, 7_919.0, 10_000.0, 12_000.0, 15_000.0, 17_000.0,
            18_000.0, 19_000.0, 19_500.0, 20_000.0,
        ] {
            for phase in [0.0, 0.3, 0.7, 1.1, std::f64::consts::FRAC_PI_4] {
                let db = steady_true_peak_db(hz, phase);
                lowest = lowest.min(db);
                highest = highest.max(db);
                assert!(db >= -0.5, "{hz} Hz at {phase}: {db} dB under-reads");
                assert!(db <= 0.25, "{hz} Hz at {phase}: {db} dB over-reads");
            }
        }
        // The f64 survey measured −0.11 and +0.17; f32 must land nearby. The
        // filter alone, without the samples, under-reads to −0.20 dB here.
        assert!(lowest > -0.2 && highest < 0.2, "{lowest} .. {highest}");
    }

    #[test]
    fn splitting_a_signal_into_blocks_changes_nothing() {
        let signal = tone(15_000.0, 0.4, 0, 4096);
        let mut whole = TruePeak::new();
        let expected = whole.measure(1, &signal);

        // Lengths below, at and above the history, and not multiples of four.
        for lengths in [
            &[1024usize, 1024, 2048][..],
            &[1, 5, 7, 10, 11, 12, 13, 4037],
        ] {
            let mut split = TruePeak::new();
            let mut start = 0;
            let mut peak = 0.0f32;
            for &len in lengths {
                peak = peak.max(split.measure(1, &signal[start..start + len]));
                start += len;
            }
            assert_eq!(start, signal.len());
            assert_eq!(peak, expected, "split as {lengths:?}");
        }
    }

    #[test]
    fn a_sample_at_the_end_of_a_block_is_interpolated_in_the_next() {
        // The filter is centred about six samples back, so the neighbourhood of
        // a block's last sample is interpolated once the next block arrives.
        // The meter loses nothing: it also takes the sample peak at once.
        let mut samples = vec![0.0f32; 1023];
        samples[1022] = 0.8;
        let mut meter = TruePeak::new();
        let now = meter.measure(0, &samples);
        let next = meter.measure(0, &[0.0; 16]);
        assert!(now < 0.1, "only the far taps see it yet: {now}");
        assert!(next > 0.7, "the main lobe lands in the next block: {next}");
    }

    #[test]
    fn the_coefficients_are_exact_in_f32() {
        for row in super::ROWS {
            for tap in row {
                let scaled = f64::from(tap) * 65_536.0;
                assert_eq!(scaled, scaled.round(), "{tap} is not a multiple of 2^-16");
            }
        }
    }

    #[test]
    fn history_survives_tiny_blocks() {
        let mut meter = TruePeak::new();
        for value in [0.1, 0.2, 0.3] {
            meter.measure(0, &[value]);
        }
        assert_eq!(&meter.history[0][HISTORY - 3..], &[0.1, 0.2, 0.3]);
        assert_eq!(meter.measure(42, &[1.0]), 0.0);
    }
}
