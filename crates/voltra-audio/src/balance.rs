//! Where a stereo track sits between the speakers.
//!
//! # The law, and why this one
//!
//! Turning the control to the right attenuates the left channel and leaves the
//! right where it was. Nothing is ever raised:
//!
//! ```text
//! left = min(1, 1 - balance)      right = min(1, 1 + balance)
//! ```
//!
//! The obvious alternative is a constant-power law, where the centre sits at
//! −3 dB and each side rises as the other falls. That law exists to **pan a mono
//! source** across the stereo field, where raising one side as the other drops
//! is what keeps the perceived loudness even.
//!
//! Applied to material that is *already stereo*, it would have to push one
//! channel above the level it arrived at. A control the user believes only turns
//! things down would then be able to clip. An attenuate-only law cannot.
//!
//! libobs takes a `float` here and documents neither the range nor the law
//! (`docs/references/audio.md` §5), so this is a choice rather than something
//! inherited.

use voltra_core::{Error, Result};

/// Where a track sits between left and right.
///
/// `-1.0` is hard left, `0.0` is centre, `+1.0` is hard right. The range is
/// declared, unlike the reference implementation's bare `float`.
///
/// # Examples
///
/// ```
/// use voltra_audio::Balance;
///
/// // Centre leaves both channels alone.
/// assert_eq!(Balance::CENTRE.channel_gains(), (1.0, 1.0));
/// // Hard right silences the left and does not raise the right.
/// assert_eq!(Balance::from_position(1.0)?.channel_gains(), (0.0, 1.0));
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Balance(f32);

impl Balance {
    /// Both channels at their original level.
    pub const CENTRE: Balance = Balance(0.0);

    /// Build a balance from a position in `-1.0..=1.0`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the position is outside the range or not finite.
    /// Clamping silently would hide a caller's unit mistake — a control sending
    /// 0..100 would sit hard right for ever and look like it worked.
    pub fn from_position(position: f32) -> Result<Self> {
        if !position.is_finite() || !(-1.0..=1.0).contains(&position) {
            return Err(Error::config(format!(
                "balance must be between -1.0 and 1.0, got {position}"
            )));
        }
        Ok(Balance(position))
    }

    /// The position, `-1.0` left to `+1.0` right.
    #[must_use]
    pub const fn position(self) -> f32 {
        self.0
    }

    /// The `(left, right)` multipliers this balance implies.
    ///
    /// Neither ever exceeds 1.0. See the module note.
    #[must_use]
    pub fn channel_gains(self) -> (f32, f32) {
        ((1.0 - self.0).min(1.0), (1.0 + self.0).min(1.0))
    }

    /// The multiplier for channel `index` under this balance.
    ///
    /// Channels beyond the first two are left alone: a balance control is about
    /// the stereo field, and silently attenuating a surround channel with a
    /// stereo control would be a surprise. Panning in a wider layout is its own
    /// operation.
    #[must_use]
    pub fn gain_for(self, index: usize) -> f32 {
        let (left, right) = self.channel_gains();
        match index {
            0 => left,
            1 => right,
            _ => 1.0,
        }
    }
}

impl Default for Balance {
    fn default() -> Self {
        Balance::CENTRE
    }
}

impl std::fmt::Display for Balance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == 0.0 {
            f.write_str("centre")
        } else if self.0 < 0.0 {
            write!(f, "{:.0}% left", -self.0 * 100.0)
        } else {
            write!(f, "{:.0}% right", self.0 * 100.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Balance;

    #[test]
    fn the_centre_leaves_both_channels_alone() {
        assert_eq!(Balance::CENTRE.channel_gains(), (1.0, 1.0));
        assert_eq!(Balance::default(), Balance::CENTRE);
    }

    #[test]
    fn the_extremes_silence_the_far_channel() {
        assert_eq!(
            Balance::from_position(-1.0).unwrap().channel_gains(),
            (1.0, 0.0)
        );
        assert_eq!(
            Balance::from_position(1.0).unwrap().channel_gains(),
            (0.0, 1.0)
        );
    }

    /// The test that pins the chosen law. If someone swaps in constant power,
    /// this fails — which is the point: an attenuate-only control cannot clip.
    #[test]
    fn no_position_ever_raises_a_channel() {
        for step in -100..=100 {
            let position = f32::from(i16::try_from(step).unwrap()) / 100.0;
            let balance = Balance::from_position(position).unwrap();
            let (left, right) = balance.channel_gains();
            assert!(left <= 1.0, "{balance} raised the left channel to {left}");
            assert!(
                right <= 1.0,
                "{balance} raised the right channel to {right}"
            );
            assert!(left >= 0.0 && right >= 0.0);
        }
    }

    /// Halfway right should halve the left channel, not attenuate both.
    #[test]
    fn the_law_is_linear_attenuation_of_the_far_side() {
        let half_right = Balance::from_position(0.5).unwrap();
        let (left, right) = half_right.channel_gains();
        assert!((left - 0.5).abs() < 1e-6);
        assert!((right - 1.0).abs() < 1e-6);
    }

    /// A caller sending 0..100 instead of -1..1 must find out immediately.
    #[test]
    fn out_of_range_positions_are_refused_not_clamped() {
        assert!(Balance::from_position(1.5).is_err());
        assert!(Balance::from_position(-1.5).is_err());
        assert!(Balance::from_position(f32::NAN).is_err());
        assert!(Balance::from_position(50.0).is_err());
    }

    /// A stereo control must not quietly touch surround channels.
    #[test]
    fn channels_beyond_stereo_are_left_alone() {
        let hard_left = Balance::from_position(-1.0).unwrap();
        assert_eq!(hard_left.gain_for(0), 1.0);
        assert_eq!(hard_left.gain_for(1), 0.0);
        for index in 2..8 {
            assert_eq!(hard_left.gain_for(index), 1.0, "channel {index}");
        }
    }

    #[test]
    fn balances_read_well() {
        assert_eq!(Balance::CENTRE.to_string(), "centre");
        assert_eq!(
            Balance::from_position(-0.5).unwrap().to_string(),
            "50% left"
        );
        assert_eq!(
            Balance::from_position(1.0).unwrap().to_string(),
            "100% right"
        );
    }
}
