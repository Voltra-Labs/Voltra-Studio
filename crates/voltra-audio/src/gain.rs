//! Gain, balance, and the laws that turn them into one gain per channel.
//!
//! The mixing loop never sees a [`Gain`] or a [`Balance`]. Once per track and
//! block they are reduced to an array of per-output-channel gains, and the loop
//! multiplies by that. Which law does the reducing depends on the routing:
//!
//! | Track → mix | Law | At centre |
//! |---|---|---|
//! | Same layout, not stereo | gain on every channel, balance ignored | gain |
//! | Stereo → stereo | **balance**: attenuate the opposite side only | gain, exactly |
//! | Mono → stereo | **constant-power pan** | gain × 0.707 (−3 dB) |
//!
//! libobs uses the sine law for both, which is −3 dB at centre, and hides that
//! behind a dead band at 0.49–0.51 — so a stereo source drops about 3 dB the
//! moment the control leaves it (`docs/references/audio-mixer.md` §3). Balance
//! and pan are different operations and get different laws here.

use std::f32::consts::FRAC_PI_2;

use voltra_core::{ChannelLayout, MAX_CHANNELS};

use crate::{Error, Result};

/// A linear amplitude factor, from silence to +20 dB.
///
/// Linear because that is what gets multiplied; decibels are a view for the
/// interface. The ceiling is not taken from any reference: above +20 dB a value
/// is almost certainly a configuration mistake, and an unbounded float turns
/// that mistake into a full-scale signal.
///
/// # Examples
///
/// ```
/// use voltra_audio::Gain;
///
/// let half = Gain::from_db(-6.0)?;
/// assert!((half.linear() - 0.501).abs() < 0.001);
/// assert_eq!(Gain::SILENT.db(), f32::NEG_INFINITY);
/// assert!(Gain::new(f32::NAN).is_err());
/// # Ok::<(), voltra_audio::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Gain(f32);

impl Gain {
    /// 0 dB: the signal passes unchanged.
    pub const UNITY: Self = Self(1.0);
    /// Silence.
    pub const SILENT: Self = Self(0.0);
    /// +20 dB, the loudest gain accepted.
    pub const MAX: Self = Self(10.0);
    /// [`Gain::MAX`] in decibels.
    pub const MAX_DB: f32 = 20.0;

    /// A gain from a linear factor.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidGain`] for NaN, infinity, negative values and anything
    /// above [`Gain::MAX`].
    pub fn new(linear: f32) -> Result<Self> {
        if linear.is_finite() && (0.0..=Self::MAX.0).contains(&linear) {
            // `+ 0.0` turns a `-0.0` into `0.0`, so silence has one bit pattern.
            Ok(Self(linear + 0.0))
        } else {
            Err(Error::InvalidGain(linear))
        }
    }

    /// A gain from decibels. `-inf` dB is silence.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidGain`] for NaN, `+inf` and anything above
    /// [`Gain::MAX_DB`].
    pub fn from_db(db: f32) -> Result<Self> {
        if db == f32::NEG_INFINITY {
            return Ok(Self::SILENT);
        }
        if db.is_nan() || db > Self::MAX_DB {
            return Err(Error::InvalidGain(db));
        }
        // Rounding at exactly +20 dB must not push the result over the ceiling.
        Self::new(10f32.powf(db / 20.0).min(Self::MAX.0))
    }

    /// The linear factor.
    pub const fn linear(self) -> f32 {
        self.0
    }

    /// The gain in decibels; `-inf` for silence.
    pub fn db(self) -> f32 {
        20.0 * self.0.log10()
    }

    /// Rebuild from bits that were stored from a valid gain.
    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self(f32::from_bits(bits))
    }
}

impl Default for Gain {
    fn default() -> Self {
        Self::UNITY
    }
}

/// Left/right position: `-1.0` is hard left, `0.0` centre, `1.0` hard right.
///
/// libobs uses `0 ..= 1` with `0.5` at centre; a value imported from an OBS
/// scene is `2 × b_obs − 1`. Here the centre is zero and the sign says the side.
///
/// For a stereo track it is a **balance**; for a mono track in a stereo mix it
/// is a **pan**. See the module documentation for why those differ.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Balance(f32);

impl Balance {
    /// Hard left.
    pub const LEFT: Self = Self(-1.0);
    /// Centred.
    pub const CENTER: Self = Self(0.0);
    /// Hard right.
    pub const RIGHT: Self = Self(1.0);

    /// A balance from a position in `-1 ..= 1`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidBalance`] for NaN and values outside the range.
    pub fn new(position: f32) -> Result<Self> {
        if (-1.0..=1.0).contains(&position) {
            Ok(Self(position + 0.0))
        } else {
            Err(Error::InvalidBalance(position))
        }
    }

    /// The position.
    pub const fn position(self) -> f32 {
        self.0
    }

    /// Rebuild from bits that were stored from a valid balance.
    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self(f32::from_bits(bits))
    }
}

/// Gains per output channel. Channels beyond the mix's are zero.
pub(crate) type ChannelGains = [f32; MAX_CHANNELS];

/// How a track's channels reach the mix's channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Routing {
    /// Channel `c` feeds channel `c`; balance does not apply.
    Direct,
    /// Stereo into stereo: channel `c` feeds channel `c`, with balance.
    StereoBalance,
    /// One channel feeds both sides of a stereo mix, panned.
    MonoToStereo,
}

impl Routing {
    /// The routing for a track layout into a mix layout, if there is one.
    ///
    /// Anything else needs a remix matrix, which comes with resampling.
    pub(crate) fn between(track: ChannelLayout, mix: ChannelLayout) -> Option<Self> {
        match (track, mix) {
            (ChannelLayout::Stereo, ChannelLayout::Stereo) => Some(Self::StereoBalance),
            (ChannelLayout::Mono, ChannelLayout::Stereo) => Some(Self::MonoToStereo),
            (track, mix) if track == mix => Some(Self::Direct),
            _ => None,
        }
    }

    /// Which track channel feeds mix channel `output`.
    pub(crate) const fn source_channel(self, output: usize) -> usize {
        match self {
            Self::Direct | Self::StereoBalance => output,
            Self::MonoToStereo => 0,
        }
    }

    /// The gain each mix channel receives.
    ///
    /// Trigonometry runs here, once per track and block, never per sample.
    pub(crate) fn channel_gains(
        self,
        channels: usize,
        gain: Gain,
        balance: Balance,
    ) -> ChannelGains {
        let g = gain.0;
        let b = balance.0;
        let mut gains = [0.0; MAX_CHANNELS];
        match self {
            Self::Direct => {
                for slot in gains.iter_mut().take(channels) {
                    *slot = g;
                }
            }
            Self::StereoBalance => {
                // Only the side the balance moves away from is attenuated, so
                // the centre is exactly unity and the curve has no corner.
                gains[0] = g * side(b.max(0.0));
                gains[1] = g * side((-b).max(0.0));
            }
            Self::MonoToStereo => {
                // Constant power: L² + R² = g² everywhere, −3 dB per side at
                // centre, which is the point of a pan.
                let theta = (b + 1.0) * (FRAC_PI_2 / 2.0);
                gains[0] = g * theta.cos().max(0.0);
                gains[1] = g * theta.sin().max(0.0);
            }
        }
        gains
    }
}

/// The attenuation of one side for a balance moved `away` (0 ..= 1) from it.
///
/// `cos(π/2)` in `f32` is `-4e-8`, not zero; clamping makes hard left and hard
/// right exactly silent on the other side.
fn side(away: f32) -> f32 {
    (away * FRAC_PI_2).cos().max(0.0)
}

#[cfg(test)]
// Exact values are the point of these tests.
#[allow(clippy::float_cmp)]
mod tests {
    use super::{Balance, Gain, Routing};
    use crate::Error;
    use voltra_core::ChannelLayout;

    #[test]
    fn gain_rejects_what_is_not_a_gain() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 10.01] {
            assert!(
                matches!(Gain::new(bad), Err(Error::InvalidGain(_))),
                "{bad}"
            );
        }
        assert_eq!(
            Gain::new(-0.0).unwrap().linear().to_bits(),
            0.0f32.to_bits()
        );
        assert_eq!(Gain::new(10.0).unwrap(), Gain::MAX);
    }

    #[test]
    fn decibels_round_trip() {
        let minus_six = Gain::from_db(-6.0).unwrap();
        assert!((minus_six.linear() - 0.501_187).abs() < 1e-5);
        for db in [-60.0, -12.0, -0.5, 0.0, 6.0, 20.0] {
            let back = Gain::from_db(db).unwrap().db();
            assert!((back - db).abs() < 1e-3, "{db} came back as {back}");
        }
        assert_eq!(Gain::from_db(0.0).unwrap(), Gain::UNITY);
        assert_eq!(Gain::from_db(20.0).unwrap(), Gain::MAX);
        assert_eq!(Gain::from_db(f32::NEG_INFINITY).unwrap(), Gain::SILENT);
        assert!(Gain::from_db(20.01).is_err());
        assert!(Gain::from_db(f32::NAN).is_err());
        assert!(Gain::from_db(f32::INFINITY).is_err());
    }

    #[test]
    fn balance_rejects_out_of_range() {
        for bad in [f32::NAN, -1.01, 1.01, f32::INFINITY] {
            assert!(
                matches!(Balance::new(bad), Err(Error::InvalidBalance(_))),
                "{bad}"
            );
        }
        assert_eq!(Balance::new(-1.0).unwrap(), Balance::LEFT);
    }

    #[test]
    fn routing_covers_same_layout_and_mono_to_stereo_only() {
        use ChannelLayout::{FivePoint1, Mono, Stereo};
        assert_eq!(
            Routing::between(Stereo, Stereo),
            Some(Routing::StereoBalance)
        );
        assert_eq!(Routing::between(Mono, Stereo), Some(Routing::MonoToStereo));
        assert_eq!(Routing::between(Mono, Mono), Some(Routing::Direct));
        assert_eq!(
            Routing::between(FivePoint1, FivePoint1),
            Some(Routing::Direct)
        );
        assert_eq!(Routing::between(FivePoint1, Stereo), None);
        assert_eq!(Routing::between(Stereo, Mono), None);
    }

    fn stereo(balance: f32) -> (f32, f32) {
        let gains =
            Routing::StereoBalance.channel_gains(2, Gain::UNITY, Balance::new(balance).unwrap());
        (gains[0], gains[1])
    }

    fn pan(balance: f32) -> (f32, f32) {
        let gains =
            Routing::MonoToStereo.channel_gains(2, Gain::UNITY, Balance::new(balance).unwrap());
        (gains[0], gains[1])
    }

    #[test]
    fn stereo_balance_is_exactly_unity_at_centre() {
        assert_eq!(stereo(0.0), (1.0, 1.0));
    }

    #[test]
    fn stereo_balance_extremes_silence_the_other_side_exactly() {
        assert_eq!(stereo(-1.0), (1.0, 0.0));
        assert_eq!(stereo(1.0), (0.0, 1.0));
    }

    #[test]
    fn stereo_balance_has_no_jump_anywhere() {
        // libobs's dead band ends at 0.51 with a ~3 dB step. Sweep the whole
        // range and require every step to stay small.
        let mut previous = stereo(-1.0);
        for step in -999..=1000 {
            #[allow(clippy::cast_precision_loss, reason = "small integers")]
            let current = stereo(step as f32 / 1000.0);
            assert!(
                (current.0 - previous.0).abs() < 0.01,
                "left jumps at {step}"
            );
            assert!(
                (current.1 - previous.1).abs() < 0.01,
                "right jumps at {step}"
            );
            previous = current;
        }
    }

    #[test]
    fn mono_pan_keeps_power_constant() {
        for step in -100..=100 {
            #[allow(clippy::cast_precision_loss, reason = "small integers")]
            let (left, right) = pan(step as f32 / 100.0);
            assert!(
                (left * left + right * right - 1.0).abs() < 1e-5,
                "power at {step}"
            );
        }
        let (left, right) = pan(0.0);
        assert!((left - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((left - right).abs() < 1e-6);
        assert_eq!(pan(-1.0), (1.0, 0.0));
        assert_eq!(pan(1.0).0, 0.0);
    }

    #[test]
    fn direct_routing_ignores_balance_and_zeroes_unused_channels() {
        let gain = Gain::new(0.5).unwrap();
        let gains = Routing::Direct.channel_gains(6, gain, Balance::LEFT);
        assert_eq!(&gains[..6], &[0.5; 6]);
        assert_eq!(&gains[6..], &[0.0; 2]);
    }

    #[test]
    fn gain_scales_every_law() {
        let gain = Gain::new(0.25).unwrap();
        let gains = Routing::StereoBalance.channel_gains(2, gain, Balance::CENTER);
        assert_eq!(&gains[..2], &[0.25, 0.25]);
    }
}
