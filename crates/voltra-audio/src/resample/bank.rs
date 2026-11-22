//! The polyphase filter bank: one windowed-sinc row per output phase.
//!
//! For a reduced ratio `L/M` (44.1 → 48 kHz is 160/147) output sample `k` falls
//! at input position `k·M/L`, whose fractional part is one of `L` phases. Each
//! phase has its own row of coefficients, designed once here and only read on
//! the audio thread.
//!
//! The filter is longer than libswresample's default and cuts below the lower
//! Nyquist in **both** directions: swr cuts exactly at the input Nyquist when
//! upsampling and leaves images 25 dB down (reference §3).

use std::f64::consts::PI;

use voltra_core::SampleRate;

use crate::{Error, Result};

/// More phases than this would need interpolation between them, which no pair
/// of standard rates (8 to 192 kHz) requires.
pub(crate) const MAX_PHASES: usize = 1024;
/// Coefficients per phase at a cutoff of one input Nyquist; scaled up as the
/// cutoff falls.
const BASE_TAPS: f64 = 128.0;
/// Kaiser window shape: about 120 dB of stopband.
const BETA: f64 = 12.0;
/// Cutoff relative to the lower of the two Nyquist frequencies.
const CUTOFF: f64 = 0.95;
/// Rows are padded to a multiple of this so the dot product has no remainder.
pub(crate) const LANES: usize = 8;

#[derive(Debug)]
pub(crate) struct FilterBank {
    /// `L`: output rate over the greatest common divisor.
    up: usize,
    /// `M`: input rate over the greatest common divisor.
    down: usize,
    taps: usize,
    center: usize,
    /// `up` rows of `taps`, phase-major.
    coefficients: Vec<f32>,
}

impl FilterBank {
    /// The bank for converting `from` into `to`.
    ///
    /// # Errors
    ///
    /// [`Error::SameRate`] when there is nothing to convert, and
    /// [`Error::UnsupportedRatio`] when the reduced ratio needs more than
    /// [`MAX_PHASES`] phases.
    pub(crate) fn new(from: SampleRate, to: SampleRate) -> Result<Self> {
        if from == to {
            return Err(Error::SameRate(from));
        }
        let divisor = gcd(from.hz(), to.hz());
        let up = usize::try_from(to.hz() / divisor).unwrap_or(usize::MAX);
        let down = usize::try_from(from.hz() / divisor).unwrap_or(usize::MAX);
        if up > MAX_PHASES {
            return Err(Error::UnsupportedRatio { from, to });
        }

        let factor = (f64::from(to.hz()) / f64::from(from.hz())).min(1.0) * CUTOFF;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small positive tap count, rounded up"
        )]
        let taps = ((BASE_TAPS / factor).ceil() as usize).next_multiple_of(LANES);
        let center = (taps - 1) / 2;

        let mut coefficients = Vec::with_capacity(up * taps);
        let mut row = vec![0.0f64; taps];
        for phase in 0..up {
            #[allow(clippy::cast_precision_loss, reason = "small integers")]
            let offset = phase as f64 / up as f64;
            for (tap, value) in row.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss, reason = "small integers")]
                let t = (tap as f64 - center as f64) - offset;
                *value = windowed_sinc(t, factor, taps);
            }
            // Each phase on its own: unity at DC for every one of them.
            let sum: f64 = row.iter().sum();
            #[allow(clippy::cast_possible_truncation, reason = "stored as f32 by design")]
            coefficients.extend(row.iter().map(|&value| (value / sum) as f32));
        }

        Ok(Self {
            up,
            down,
            taps,
            center,
            coefficients,
        })
    }

    pub(crate) const fn up(&self) -> usize {
        self.up
    }

    pub(crate) const fn down(&self) -> usize {
        self.down
    }

    pub(crate) const fn taps(&self) -> usize {
        self.taps
    }

    pub(crate) const fn center(&self) -> usize {
        self.center
    }

    /// The coefficients of `phase`, `taps` long. Empty when out of range.
    pub(crate) fn phase(&self, phase: usize) -> &[f32] {
        let start = phase * self.taps;
        self.coefficients
            .get(start..start + self.taps)
            .unwrap_or(&[])
    }
}

/// `sinc(π·factor·t)` under a Kaiser window spanning `taps`.
fn windowed_sinc(t: f64, factor: f64, taps: usize) -> f64 {
    let x = PI * factor * t;
    let sinc = if x == 0.0 { 1.0 } else { x.sin() / x };
    #[allow(clippy::cast_precision_loss, reason = "a small tap count")]
    let w = 2.0 * t / taps as f64;
    let window = if w.abs() < 1.0 {
        bessel_i0(BETA * (1.0 - w * w).sqrt())
    } else {
        0.0
    };
    sinc * window
}

/// The zeroth-order modified Bessel function of the first kind, by its power
/// series. Only evaluated while designing a bank, never on the audio thread.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200u32 {
        let k = f64::from(k);
        term *= (half / k) * (half / k);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let rest = a % b;
        a = b;
        b = rest;
    }
    a
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use voltra_core::SampleRate;

    use super::{FilterBank, LANES, bessel_i0, gcd};
    use crate::Error;

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    #[test]
    fn ratios_reduce_exactly() {
        for (from, to, up, down) in [
            (44_100, 48_000, 160, 147),
            (48_000, 44_100, 147, 160),
            (8_000, 48_000, 6, 1),
            (96_000, 48_000, 1, 2),
            (11_025, 48_000, 640, 147),
        ] {
            let bank = FilterBank::new(rate(from), rate(to)).unwrap();
            assert_eq!((bank.up(), bank.down()), (up, down), "{from} -> {to}");
        }
        assert_eq!(gcd(44_100, 48_000), 300);
    }

    #[test]
    fn tap_counts_follow_the_cutoff() {
        let up = FilterBank::new(rate(44_100), rate(48_000)).unwrap();
        assert_eq!(up.taps(), 136, "128 / 0.95, rounded up to a multiple of 8");
        let down = FilterBank::new(rate(48_000), rate(44_100)).unwrap();
        assert_eq!(down.taps(), 152);
        assert_eq!(down.taps() % LANES, 0);
        assert_eq!(up.center(), 67);
    }

    #[test]
    fn every_phase_has_unity_gain_at_dc() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (32_000, 48_000)] {
            let bank = FilterBank::new(rate(from), rate(to)).unwrap();
            for phase in 0..bank.up() {
                let sum: f64 = bank.phase(phase).iter().map(|&c| f64::from(c)).sum();
                assert!(
                    (sum - 1.0).abs() < 1e-6,
                    "{from}->{to} phase {phase}: {sum}"
                );
            }
        }
    }

    #[test]
    fn phase_zero_peaks_at_its_centre() {
        let bank = FilterBank::new(rate(44_100), rate(48_000)).unwrap();
        let row = bank.phase(0);
        let (peak, _) =
            row.iter().enumerate().fold(
                (0, f32::MIN),
                |best, (i, &c)| if c > best.1 { (i, c) } else { best },
            );
        assert_eq!(peak, bank.center());
        assert!(bank.phase(bank.up()).is_empty());
    }

    #[test]
    fn same_rate_and_huge_ratios_are_refused() {
        assert_eq!(
            FilterBank::new(rate(48_000), rate(48_000)).unwrap_err(),
            Error::SameRate(rate(48_000))
        );
        assert!(matches!(
            FilterBank::new(rate(44_100), rate(48_001)).unwrap_err(),
            Error::UnsupportedRatio { .. }
        ));
    }

    #[test]
    fn bessel_matches_known_values() {
        assert_eq!(bessel_i0(0.0), 1.0);
        // I0(1) = 1.266065877752008, I0(12) = 18948.92534929630
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008).abs() < 1e-14);
        assert!((bessel_i0(12.0) / 18_948.925_349_296_3 - 1.0).abs() < 1e-12);
    }
}
