//! Rate, channel layout and the edge sample formats.
//!
//! See the module documentation in `mod.rs` for why depth and interleaving are
//! separate axes here.

use std::time::Duration;

use crate::time::Timestamp;
use crate::{Error, Result};

/// The largest number of channels a buffer can carry.
///
/// Eight, matching libobs's `MAX_AUDIO_CHANNELS`, which is 7.1 — the widest
/// layout consumer hardware actually produces.
pub const MAX_CHANNELS: usize = 8;

/// Samples per second.
///
/// A newtype rather than a bare `u32` (CLAUDE.md §3), because what it carries is
/// not the number but the exact arithmetic built on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(u32);

impl SampleRate {
    /// CD rate, and what a lot of consumer hardware still reports.
    pub const HZ_44100: SampleRate = SampleRate(44_100);

    /// The broadcast and production default, and what OBS mixes at.
    pub const HZ_48000: SampleRate = SampleRate(48_000);

    /// Build a sample rate.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the rate is zero. Every downstream calculation
    /// divides by it.
    pub fn new(hz: u32) -> Result<Self> {
        if hz == 0 {
            return Err(Error::config("sample rate must be greater than zero"));
        }
        Ok(SampleRate(hz))
    }

    /// The rate in hertz.
    pub const fn hz(self) -> u32 {
        self.0
    }

    /// The exact timestamp of sample `index`.
    ///
    /// Computed from the absolute index in 128-bit arithmetic, never by adding
    /// up block durations. A block of 1024 samples at 48 kHz lasts 21.333… ms,
    /// and adding that a thousand times is not the same number as dividing
    /// once — which is exactly the drift the audio clock exists to not have.
    ///
    /// # Examples
    ///
    /// ```
    /// use voltra_core::{SampleRate, Timestamp};
    ///
    /// let rate = SampleRate::HZ_48000;
    /// assert_eq!(rate.pts(48_000), Timestamp::from_nanos(1_000_000_000));
    /// // Ten hours in, still exact.
    /// assert_eq!(rate.pts(48_000 * 36_000).as_nanos(), 36_000_000_000_000);
    /// ```
    pub fn pts(self, index: u64) -> Timestamp {
        let nanos = (u128::from(index) * 1_000_000_000u128) / u128::from(self.0);
        Timestamp::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// How long `frames` samples last.
    ///
    /// Rounded, and therefore fine for sleeping between blocks. Timestamps come
    /// from [`SampleRate::pts`], never from summing this.
    pub fn duration_of(self, frames: u64) -> Duration {
        let nanos = (u128::from(frames) * 1_000_000_000u128) / u128::from(self.0);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// How many whole samples fit in `duration`, rounding down.
    pub fn frames_in(self, duration: Duration) -> u64 {
        let nanos = duration.as_nanos();
        let frames = (nanos * u128::from(self.0)) / 1_000_000_000u128;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }
}

impl std::fmt::Display for SampleRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

/// Which speakers a buffer is for.
///
/// The seven layouts of libobs's `speaker_layout`, kept identical so a scene
/// authored in OBS means the same thing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum ChannelLayout {
    /// One channel.
    Mono,
    /// Left and right. The default.
    #[default]
    Stereo,
    /// Stereo with a low-frequency channel.
    TwoPoint1,
    /// Four channels, no subwoofer.
    FourPoint0,
    /// Four channels with a low-frequency channel.
    FourPoint1,
    /// Five channels with a low-frequency channel.
    FivePoint1,
    /// Seven channels with a low-frequency channel.
    SevenPoint1,
}

impl ChannelLayout {
    /// How many channels this layout has.
    ///
    /// Derived, never stored alongside the layout: two fields that can
    /// contradict each other is one field too many.
    pub const fn channels(self) -> usize {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::TwoPoint1 => 3,
            ChannelLayout::FourPoint0 => 4,
            ChannelLayout::FourPoint1 => 5,
            ChannelLayout::FivePoint1 => 6,
            ChannelLayout::SevenPoint1 => 8,
        }
    }

    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            ChannelLayout::Mono => "mono",
            ChannelLayout::Stereo => "stereo",
            ChannelLayout::TwoPoint1 => "2.1",
            ChannelLayout::FourPoint0 => "4.0",
            ChannelLayout::FourPoint1 => "4.1",
            ChannelLayout::FivePoint1 => "5.1",
            ChannelLayout::SevenPoint1 => "7.1",
        }
    }
}

impl std::fmt::Display for ChannelLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How wide one sample is, at the edge of the system.
///
/// Only used when converting to or from a device or an encoder. The mixer never
/// sees one of these: everything inside is `f32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// Unsigned 8-bit, with 128 as silence. What cheap capture hardware emits.
    U8,
    /// Signed 16-bit. The most common interchange format there is.
    I16,
    /// Signed 32-bit.
    I32,
    /// 32-bit float, nominally in `[-1, 1]`.
    F32,
}

impl SampleFormat {
    /// Bytes per sample.
    pub const fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I32 | SampleFormat::F32 => 4,
        }
    }

    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            SampleFormat::U8 => "u8",
            SampleFormat::I16 => "i16",
            SampleFormat::I32 => "i32",
            SampleFormat::F32 => "f32",
        }
    }
}

impl std::fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How samples of different channels are arranged in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SampleOrder {
    /// `L R L R …`. What devices and most containers use. The default.
    #[default]
    Interleaved,
    /// `L L L … R R R …`. What the mixer uses internally.
    Planar,
}

/// A complete description of an edge buffer: how wide, and how arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleSpec {
    /// Bytes per sample and their interpretation.
    pub format: SampleFormat,
    /// Interleaved or planar.
    pub order: SampleOrder,
}

impl SampleSpec {
    /// Build a spec.
    pub const fn new(format: SampleFormat, order: SampleOrder) -> Self {
        Self { format, order }
    }

    /// Interleaved 16-bit, which is what a sound card almost always gives you.
    pub const I16_INTERLEAVED: SampleSpec =
        SampleSpec::new(SampleFormat::I16, SampleOrder::Interleaved);

    /// Interleaved float, what most encoders and WAV files want.
    pub const F32_INTERLEAVED: SampleSpec =
        SampleSpec::new(SampleFormat::F32, SampleOrder::Interleaved);

    /// How many bytes `frames` samples of `channels` channels occupy.
    pub const fn byte_len(self, frames: usize, channels: usize) -> usize {
        self.format.bytes() * frames * channels
    }
}

impl std::fmt::Display for SampleSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let order = match self.order {
            SampleOrder::Interleaved => "interleaved",
            SampleOrder::Planar => "planar",
        };
        write!(f, "{} {order}", self.format)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelLayout, MAX_CHANNELS, SampleFormat, SampleOrder, SampleRate, SampleSpec};
    use std::time::Duration;

    #[test]
    fn a_zero_rate_is_refused() {
        assert!(SampleRate::new(0).is_err());
        assert_eq!(SampleRate::new(48_000).unwrap(), SampleRate::HZ_48000);
    }

    /// The whole point of computing from the absolute index: the sample at one
    /// second is at exactly one second, however far in you are.
    #[test]
    fn sample_timestamps_are_exact() {
        let rate = SampleRate::HZ_48000;
        assert_eq!(rate.pts(0).as_nanos(), 0);
        assert_eq!(rate.pts(48_000).as_nanos(), 1_000_000_000);
        assert_eq!(rate.pts(24_000).as_nanos(), 500_000_000);

        // Ten hours of broadcast.
        let ten_hours = 48_000u64 * 3_600 * 10;
        assert_eq!(rate.pts(ten_hours).as_nanos(), 36_000_000_000_000);
    }

    /// This is the test the module exists for. A 1024-sample block at 48 kHz
    /// lasts `21_333_333.33…` ns, which `Duration` has to round down, losing a
    /// third of a nanosecond every block.
    ///
    /// The point is not the size of that — it is 33 µs over half an hour, well
    /// under any audible sync error. The point is that it **grows without
    /// bound and never comes back**, while computing from the absolute index
    /// costs exactly the same and does not. A pipeline that runs for days has no
    /// business accumulating an error it can simply not have.
    #[test]
    fn accumulating_block_durations_drifts_but_pts_does_not() {
        let rate = SampleRate::HZ_48000;
        let block = 1024u64;
        let one_block = u64::try_from(rate.duration_of(block).as_nanos()).unwrap();

        let drift_after = |blocks: u64| {
            let exact = rate.pts(block * blocks).as_nanos();
            let accumulated = one_block * blocks;
            assert!(
                accumulated <= exact,
                "rounding down should fall short, never overshoot"
            );
            exact - accumulated
        };

        // Linear and unbounded: ten times the blocks, ten times the error. Not
        // exactly ten times, because `pts` itself truncates to whole
        // nanoseconds, so a few nanoseconds of slack is the measurement's own
        // resolution rather than drift.
        let half_hour = drift_after(100_000);
        let five_hours = drift_after(1_000_000);
        assert!(half_hour > 0, "the naive sum should already be behind");
        assert!(
            five_hours.abs_diff(half_hour * 10) <= 10,
            "drift should grow linearly: {half_hour} ns became {five_hours} ns"
        );

        // And the exact path is exact at both scales, which is the whole claim.
        assert_eq!(rate.pts(48_000).as_nanos(), 1_000_000_000);
        assert_eq!(rate.pts(48_000 * 18_000).as_nanos(), 18_000_000_000_000);
    }

    #[test]
    fn durations_and_frame_counts_round_trip() {
        let rate = SampleRate::HZ_48000;
        assert_eq!(rate.duration_of(48_000), Duration::from_secs(1));
        assert_eq!(rate.frames_in(Duration::from_secs(1)), 48_000);
        // Rounding down, never up: a partial sample is not a sample.
        assert_eq!(rate.frames_in(Duration::from_nanos(20_832)), 0);
        assert_eq!(rate.frames_in(Duration::from_nanos(20_834)), 1);
    }

    #[test]
    fn forty_four_one_is_exact_too() {
        let rate = SampleRate::HZ_44100;
        assert_eq!(rate.pts(44_100).as_nanos(), 1_000_000_000);
        assert_eq!(rate.hz(), 44_100);
    }

    /// Channel counts come from the layout and must match libobs's, or a scene
    /// authored in OBS would mean something different here.
    #[test]
    fn layouts_report_the_channels_they_have() {
        let expected = [
            (ChannelLayout::Mono, 1),
            (ChannelLayout::Stereo, 2),
            (ChannelLayout::TwoPoint1, 3),
            (ChannelLayout::FourPoint0, 4),
            (ChannelLayout::FourPoint1, 5),
            (ChannelLayout::FivePoint1, 6),
            (ChannelLayout::SevenPoint1, 8),
        ];
        for (layout, channels) in expected {
            assert_eq!(layout.channels(), channels, "{layout}");
            assert!(layout.channels() <= MAX_CHANNELS);
            assert!(!layout.name().is_empty());
        }
    }

    #[test]
    fn stereo_is_the_default_layout() {
        assert_eq!(ChannelLayout::default(), ChannelLayout::Stereo);
        assert_eq!(SampleOrder::default(), SampleOrder::Interleaved);
    }

    #[test]
    fn sample_widths_are_what_the_names_say() {
        assert_eq!(SampleFormat::U8.bytes(), 1);
        assert_eq!(SampleFormat::I16.bytes(), 2);
        assert_eq!(SampleFormat::I32.bytes(), 4);
        assert_eq!(SampleFormat::F32.bytes(), 4);
    }

    #[test]
    fn byte_lengths_multiply_out() {
        let spec = SampleSpec::I16_INTERLEAVED;
        assert_eq!(spec.byte_len(1024, 2), 1024 * 2 * 2);
        assert_eq!(SampleSpec::F32_INTERLEAVED.byte_len(1024, 6), 1024 * 6 * 4);
    }

    #[test]
    fn specs_describe_themselves() {
        assert_eq!(SampleSpec::I16_INTERLEAVED.to_string(), "i16 interleaved");
        assert_eq!(
            SampleSpec::new(SampleFormat::F32, SampleOrder::Planar).to_string(),
            "f32 planar"
        );
        assert_eq!(SampleRate::HZ_48000.to_string(), "48000 Hz");
    }
}
