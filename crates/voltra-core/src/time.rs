//! Frame timing.
//!
//! Two rules govern this module, and both exist because A/V desync is the
//! failure users notice first:
//!
//! 1. **Time is integral.** Nanoseconds in a `u64`, never seconds in an `f64`.
//! 2. **Frame rates are rational.** libobs expresses the canvas rate as
//!    `fps_num`/`fps_den` for the same reason: 30000/1001 has no exact decimal
//!    representation, so storing it as `29.97` guarantees drift.

use std::fmt;
use std::time::Duration;

/// A presentation timestamp in nanoseconds since the start of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The start of the stream.
    pub const ZERO: Timestamp = Timestamp(0);

    /// Build a timestamp from nanoseconds.
    pub const fn from_nanos(nanos: u64) -> Self {
        Timestamp(nanos)
    }

    /// Build a timestamp from whole milliseconds.
    pub const fn from_millis(millis: u64) -> Self {
        Timestamp(millis.saturating_mul(1_000_000))
    }

    /// The timestamp in nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// The timestamp as a [`Duration`].
    pub const fn as_duration(self) -> Duration {
        Duration::from_nanos(self.0)
    }

    /// The timestamp in seconds.
    ///
    /// For display and statistics only. Never feed this back into timing
    /// arithmetic: that is precisely the round trip this type exists to avoid.
    #[allow(
        clippy::cast_precision_loss,
        reason = "display path only; f64 holds nanosecond counts exactly for ~104 days"
    )]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1e9
    }

    /// Nanoseconds elapsed since `earlier`, saturating at zero.
    ///
    /// Saturating rather than wrapping: a source that reports an out-of-order
    /// timestamp should read as "no time passed", not as an enormous interval
    /// that would stall the pipeline.
    pub const fn since(self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for Timestamp {
    /// Formats as the timecode `HH:MM:SS.mmm`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_ms = self.0 / 1_000_000;
        let hours = total_ms / 3_600_000;
        let minutes = (total_ms / 60_000) % 60;
        let seconds = (total_ms / 1_000) % 60;
        let millis = total_ms % 1_000;
        write!(f, "{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    }
}

/// A frame rate expressed as the exact fraction `num / den`.
///
/// Always stored in lowest terms, so two rates that mean the same thing compare
/// equal and hash alike: `60000/1000` and `60/1` are the same frame rate and
/// must not behave like two different ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fps {
    num: u32,
    den: u32,
}

impl Fps {
    /// 24 fps — cinema.
    pub const FPS_24: Fps = Fps { num: 24, den: 1 };
    /// 30 fps.
    pub const FPS_30: Fps = Fps { num: 30, den: 1 };
    /// 60 fps.
    pub const FPS_60: Fps = Fps { num: 60, den: 1 };
    /// 30000/1001, commonly written 29.97 — NTSC.
    pub const FPS_29_97: Fps = Fps {
        num: 30_000,
        den: 1_001,
    };
    /// 60000/1001, commonly written 59.94 — NTSC.
    pub const FPS_59_94: Fps = Fps {
        num: 60_000,
        den: 1_001,
    };

    /// Build a frame rate from an exact fraction, reduced to lowest terms.
    ///
    /// Returns [`Error::Config`](crate::Error::Config) when either term is zero:
    /// a zero denominator is undefined and a zero numerator is a stopped clock.
    ///
    /// # Errors
    ///
    /// See above.
    pub fn new(num: u32, den: u32) -> crate::Result<Self> {
        if num == 0 || den == 0 {
            return Err(crate::Error::config(format!(
                "frame rate {num}/{den} must have non-zero terms"
            )));
        }
        let divisor = greatest_common_divisor(num, den);
        Ok(Fps {
            num: num / divisor,
            den: den / divisor,
        })
    }

    /// The numerator.
    pub const fn num(self) -> u32 {
        self.num
    }

    /// The denominator.
    pub const fn den(self) -> u32 {
        self.den
    }

    /// The rate as a decimal, for display and statistics only.
    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The exact presentation timestamp of frame `index`.
    ///
    /// Computed from the absolute frame index in 128-bit arithmetic rather than
    /// by accumulating frame durations, so the result carries no accumulated
    /// rounding error however long the broadcast runs.
    pub fn pts(self, index: u64) -> Timestamp {
        let nanos =
            (u128::from(index) * u128::from(self.den) * 1_000_000_000u128) / u128::from(self.num);
        Timestamp(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// The nominal duration of one frame.
    ///
    /// This is a *rounded* value, suitable for sleeping between frames. Frame
    /// timestamps must come from [`Fps::pts`], never from summing this.
    pub fn frame_duration(self) -> Duration {
        let nanos = (u128::from(self.den) * 1_000_000_000u128) / u128::from(self.num);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

impl Default for Fps {
    fn default() -> Self {
        Fps::FPS_60
    }
}

impl fmt::Display for Fps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{:.2}", self.as_f64())
        }
    }
}

impl std::str::FromStr for Fps {
    type Err = crate::Error;

    /// Accepts `60`, `30000/1001` and `29.97`.
    ///
    /// The two NTSC rates snap to their exact fractions: someone typing `29.97`
    /// wants 30000/1001, and honouring the decimal literally would reintroduce
    /// the drift this type prevents.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`](crate::Error::Config) for anything else.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let invalid = || crate::Error::config(format!("invalid frame rate `{text}`"));

        if let Some((num, den)) = text.split_once('/') {
            let num = num.trim().parse::<u32>().map_err(|_| invalid())?;
            let den = den.trim().parse::<u32>().map_err(|_| invalid())?;
            return Fps::new(num, den);
        }

        let value = text.parse::<f64>().map_err(|_| invalid())?;
        if !value.is_finite() || value <= 0.0 {
            return Err(invalid());
        }
        if (value - 29.97).abs() < 0.005 {
            return Ok(Fps::FPS_29_97);
        }
        if (value - 59.94).abs() < 0.005 {
            return Ok(Fps::FPS_59_94);
        }
        // Keep three decimals of whatever else was typed, as an exact fraction.
        let thousandths = scale_to_u32(value, 1_000.0).ok_or_else(invalid)?;
        Fps::new(thousandths, 1_000)
    }
}

/// Binary GCD, used to keep frame rates in lowest terms.
const fn greatest_common_divisor(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

/// Multiply and round to a `u32`, rejecting anything out of range.
fn scale_to_u32(value: f64, factor: f64) -> Option<u32> {
    let scaled = (value * factor).round();
    if scaled < 1.0 || scaled > f64::from(u32::MAX) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range checked immediately above; the value is rounded and positive"
    )]
    Some(scaled as u32)
}

#[cfg(test)]
mod tests {
    use super::{Fps, Timestamp};
    use std::str::FromStr;

    #[test]
    fn integer_rates_are_exact() {
        assert_eq!(Fps::FPS_60.pts(0), Timestamp::ZERO);
        assert_eq!(Fps::FPS_60.pts(60), Timestamp::from_nanos(1_000_000_000));
        assert_eq!(Fps::FPS_30.pts(90), Timestamp::from_nanos(3_000_000_000));
    }

    /// The invariant this module exists for. `108_000` NTSC frames take exactly
    /// 3603.6 s, and `Fps::pts` must say so to the nanosecond.
    ///
    /// The second half shows why the type is rational: accumulating the decimal
    /// `29.97` that users actually type drifts by milliseconds within one hour,
    /// and never stops growing.
    #[test]
    fn ntsc_does_not_drift() {
        assert_eq!(Fps::FPS_29_97.pts(108_000).as_nanos(), 3_603_600_000_000);

        let mut naive_seconds = 0.0f64;
        for _ in 0..108_000 {
            naive_seconds += 1.0 / 29.97;
        }
        let drift_ms = (naive_seconds - 3603.6).abs() * 1000.0;
        assert!(
            drift_ms > 1.0,
            "expected the decimal approach to drift; measured {drift_ms:.3} ms"
        );
    }

    #[test]
    fn equal_rates_written_differently_are_equal() {
        assert_eq!(Fps::new(60_000, 1_000).unwrap(), Fps::FPS_60);
        assert_eq!(Fps::new(120, 2).unwrap(), Fps::FPS_60);
        // NTSC is already in lowest terms and must survive untouched.
        let ntsc = Fps::new(30_000, 1_001).unwrap();
        assert_eq!((ntsc.num(), ntsc.den()), (30_000, 1_001));
    }

    #[test]
    fn pts_stays_monotonic_across_a_long_run() {
        let mut previous = Timestamp::ZERO;
        for index in 1..10_000 {
            let pts = Fps::FPS_29_97.pts(index);
            assert!(pts > previous, "frame {index} did not advance");
            previous = pts;
        }
    }

    #[test]
    fn degenerate_rates_are_rejected() {
        assert!(Fps::new(0, 1).is_err());
        assert!(Fps::new(60, 0).is_err());
    }

    #[test]
    fn parses_the_three_common_spellings() {
        assert_eq!(Fps::from_str("60").unwrap(), Fps::FPS_60);
        assert_eq!(Fps::from_str(" 30000 / 1001 ").unwrap(), Fps::FPS_29_97);
        assert_eq!(Fps::from_str("29.97").unwrap(), Fps::FPS_29_97);
        assert_eq!(Fps::from_str("59.94").unwrap(), Fps::FPS_59_94);
    }

    #[test]
    fn rejects_nonsense_rates() {
        for text in ["0", "1/0", "-30", "banana", "", "1e400"] {
            assert!(Fps::from_str(text).is_err(), "`{text}` should be rejected");
        }
    }

    #[test]
    fn rates_display_readably() {
        assert_eq!(Fps::FPS_60.to_string(), "60");
        assert_eq!(Fps::FPS_29_97.to_string(), "29.97");
    }

    #[test]
    fn timestamps_format_as_timecode() {
        assert_eq!(
            Timestamp::from_millis(3_723_456).to_string(),
            "01:02:03.456"
        );
        assert_eq!(Timestamp::ZERO.to_string(), "00:00:00.000");
    }

    #[test]
    fn out_of_order_timestamps_saturate() {
        let early = Timestamp::from_millis(10);
        let late = Timestamp::from_millis(20);
        assert_eq!(late.since(early), 10_000_000);
        assert_eq!(early.since(late), 0);
    }
}
