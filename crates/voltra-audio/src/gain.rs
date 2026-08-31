//! Amplitude, and the two ways people talk about it.
//!
//! Internally a gain is a plain linear multiplier, because that is what the
//! mixer does with it. Faders and metering are in decibels, because that is what
//! ears do. Both directions live here so the conversion happens once, at the
//! edge of the interface, and never inside a sample loop.

use voltra_core::{Error, Result};

/// A linear amplitude multiplier.
///
/// Always finite and never negative: a negative gain is a phase inversion, which
/// is a different operation with a different name, and letting it in through the
/// volume control would surprise everyone.
///
/// # Examples
///
/// ```
/// use voltra_audio::Gain;
///
/// assert_eq!(Gain::UNITY.linear(), 1.0);
/// // Six decibels down is half the amplitude, near enough.
/// assert!((Gain::from_db(-6.0).linear() - 0.501).abs() < 0.001);
/// // Silence is the floor, and it is exactly zero rather than very small.
/// assert_eq!(Gain::SILENT.linear(), 0.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Gain(f32);

impl Gain {
    /// No change: a multiplier of exactly one.
    pub const UNITY: Gain = Gain(1.0);

    /// Silence: a multiplier of exactly zero.
    pub const SILENT: Gain = Gain(0.0);

    /// Below this, a gain is treated as silence.
    ///
    /// −120 dB is far below the noise floor of any real signal and well below
    /// the resolution of 16-bit output. Snapping to zero there keeps a fader
    /// pulled all the way down from leaving an inaudible but non-zero residue
    /// that still costs a multiply per sample.
    const SILENCE_THRESHOLD: f32 = 0.000_001;

    /// Build a gain from a linear multiplier.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the value is negative or not finite. A `NaN` gain
    /// would silently turn a whole track into `NaN` and, once summed, the entire
    /// mix — the kind of failure that is very loud and very hard to trace.
    pub fn from_linear(value: f32) -> Result<Self> {
        if !value.is_finite() || value < 0.0 {
            return Err(Error::config(format!(
                "gain must be finite and non-negative, got {value}"
            )));
        }
        Ok(Gain(if value < Self::SILENCE_THRESHOLD {
            0.0
        } else {
            value
        }))
    }

    /// Build a gain from decibels.
    ///
    /// `f32::NEG_INFINITY` is silence, which is what a fader at the bottom of
    /// its travel means.
    #[must_use]
    pub fn from_db(db: f32) -> Self {
        if !db.is_finite() {
            // −inf is silence; +inf and NaN have no sensible reading, and
            // silence is the safe one to pick.
            return if db == f32::INFINITY {
                Gain::UNITY
            } else {
                Gain::SILENT
            };
        }
        let linear = 10f32.powf(db / 20.0);
        Gain::from_linear(linear).unwrap_or(Gain::SILENT)
    }

    /// The linear multiplier.
    #[must_use]
    pub const fn linear(self) -> f32 {
        self.0
    }

    /// The gain in decibels, `f32::NEG_INFINITY` for silence.
    #[must_use]
    pub fn db(self) -> f32 {
        if self.0 <= 0.0 {
            return f32::NEG_INFINITY;
        }
        20.0 * self.0.log10()
    }

    /// Whether this gain is silence.
    #[must_use]
    pub fn is_silent(self) -> bool {
        self.0 == 0.0
    }
}

impl Default for Gain {
    fn default() -> Self {
        Gain::UNITY
    }
}

impl std::fmt::Display for Gain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_silent() {
            f.write_str("-inf dB")
        } else {
            write!(f, "{:.1} dB", self.db())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Gain;

    #[test]
    fn unity_is_exactly_one_and_silence_exactly_zero() {
        assert_eq!(Gain::UNITY.linear(), 1.0);
        assert_eq!(Gain::SILENT.linear(), 0.0);
        assert!(Gain::SILENT.is_silent());
        assert!(!Gain::UNITY.is_silent());
    }

    /// The decibel scale people expect: 0 is unity, −6 is about half the
    /// amplitude, −20 is a tenth.
    #[test]
    fn decibels_convert_the_way_a_fader_says() {
        assert!((Gain::from_db(0.0).linear() - 1.0).abs() < 1e-6);
        assert!((Gain::from_db(-6.0).linear() - 0.501_187).abs() < 1e-5);
        assert!((Gain::from_db(-20.0).linear() - 0.1).abs() < 1e-6);
        assert!((Gain::from_db(6.0).linear() - 1.995_262).abs() < 1e-5);
    }

    #[test]
    fn decibels_round_trip() {
        for db in [-60.0f32, -20.0, -6.0, 0.0, 3.0, 12.0] {
            let back = Gain::from_db(db).db();
            assert!((back - db).abs() < 1e-4, "{db} became {back}");
        }
    }

    #[test]
    fn a_fader_at_the_bottom_is_silence() {
        assert!(Gain::from_db(f32::NEG_INFINITY).is_silent());
        assert_eq!(Gain::SILENT.db(), f32::NEG_INFINITY);
        // Far below anything audible snaps to true zero.
        assert!(Gain::from_db(-200.0).is_silent());
    }

    /// A NaN gain would turn one track, and then the whole sum, into NaN.
    #[test]
    fn nonsense_gains_are_refused() {
        assert!(Gain::from_linear(f32::NAN).is_err());
        assert!(Gain::from_linear(f32::INFINITY).is_err());
        assert!(Gain::from_linear(-1.0).is_err());
        assert!(Gain::from_linear(0.0).is_ok());
        assert!(Gain::from_db(f32::NAN).is_silent());
    }

    #[test]
    fn gains_read_well() {
        assert_eq!(Gain::UNITY.to_string(), "0.0 dB");
        assert_eq!(Gain::SILENT.to_string(), "-inf dB");
    }
}
