//! Anchoring video to the audio clock.
//!
//! CLAUDE.md §5 states the rule: **audio drives synchronisation and video
//! follows.** The reason is that the audio clock is the only one in the system
//! that advances in units the hardware actually counts. A video frame rate is a
//! convention; a sample is electricity.
//!
//! So the master clock here is a **count of samples**, and time is derived from
//! it with the exact arithmetic of plan 011 — never accumulated from rounded
//! block durations, which is what drifts.
//!
//! # The decision is a pure function
//!
//! [`AvSync::classify`] takes two timestamps and returns what to do. It touches
//! no queues, mutates nothing, and does not know what a frame is. That is what
//! makes it exhaustively testable, which sync code usually is not — and it is
//! why this module works on timestamps rather than on
//! [`VideoFrame`](voltra_core::VideoFrame), which also keeps `voltra-audio`
//! from depending on the renderer.
//!
//! The loop that calls it — with its queues, its threads and its ~1 s of
//! buffering — is `voltra-engine`, and belongs to the phase that first has real
//! hardware clocks to disagree with each other.

use std::time::Duration;

use voltra_core::{Error, Fps, Result, SampleRate, Timestamp};

/// The master clock: samples counted, time derived.
///
/// # Examples
///
/// ```
/// use voltra_audio::{AudioClock, SampleRate};
/// use voltra_core::Timestamp;
///
/// let mut clock = AudioClock::new(SampleRate::HZ_48000);
/// assert_eq!(clock.now(), Timestamp::ZERO);
///
/// // Forty-seven blocks of 1024 samples is not a round number of milliseconds,
/// // and the clock does not care: it counts samples.
/// for _ in 0..47 {
///     clock.advance(1024);
/// }
/// assert_eq!(clock.frames(), 48_128);
/// assert_eq!(clock.now(), SampleRate::HZ_48000.pts(48_128));
/// ```
#[derive(Debug, Clone)]
pub struct AudioClock {
    rate: SampleRate,
    frames: u64,
}

impl AudioClock {
    /// A clock at `rate`, starting at zero.
    #[must_use]
    pub const fn new(rate: SampleRate) -> Self {
        Self { rate, frames: 0 }
    }

    /// The rate this clock counts at.
    #[must_use]
    pub const fn rate(&self) -> SampleRate {
        self.rate
    }

    /// Total samples counted since the start.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// The current time.
    ///
    /// Derived from the absolute sample count, so it is exact however long the
    /// broadcast runs, and it can never go backwards.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.rate.pts(self.frames)
    }

    /// Count `frames` more samples and return the new time.
    pub fn advance(&mut self, frames: usize) -> Timestamp {
        self.frames = self
            .frames
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        self.now()
    }

    /// Reset to zero, for a session that starts again.
    pub const fn reset(&mut self) {
        self.frames = 0;
    }
}

/// A manual timing correction for one source.
///
/// **Positive delays the video**, negative advances it. The sense is fixed by a
/// test rather than left to whoever reads the arithmetic, because a control that
/// works the wrong way round is the kind of bug nobody reports as a bug — they
/// just say the slider is confusing.
///
/// This exists because no automatic calculation gets an external mixing desk or
/// a Bluetooth microphone right, and the person watching can see the problem.
/// libobs offers the same thing as `Sync Offset (ms)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct SyncOffset(i64);

impl SyncOffset {
    /// No correction.
    pub const NONE: SyncOffset = SyncOffset(0);

    /// The largest correction that can be asked for, either way.
    ///
    /// Five seconds is far past any real device latency; a request beyond it is
    /// a unit mistake, and refusing it beats silently sitting on it.
    pub const LIMIT_NANOS: i64 = 5_000_000_000;

    /// Build an offset from milliseconds, which is how it is shown to a person.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the magnitude exceeds [`SyncOffset::LIMIT_NANOS`].
    pub fn from_millis(millis: i64) -> Result<Self> {
        let nanos = millis
            .checked_mul(1_000_000)
            .ok_or_else(|| Error::config(format!("sync offset {millis} ms is out of range")))?;
        Self::from_nanos(nanos)
    }

    /// Build an offset from nanoseconds.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the magnitude exceeds [`SyncOffset::LIMIT_NANOS`].
    pub fn from_nanos(nanos: i64) -> Result<Self> {
        if nanos.abs() > Self::LIMIT_NANOS {
            return Err(Error::config(format!(
                "sync offset {nanos} ns exceeds the ±5 s limit"
            )));
        }
        Ok(SyncOffset(nanos))
    }

    /// The offset in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// The offset in milliseconds, rounded toward zero.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0 / 1_000_000
    }
}

impl std::fmt::Display for SyncOffset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:+} ms", self.as_millis())
    }
}

/// What to do with a video frame at the current audio position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameFate {
    /// Not due yet. Keep showing the previous frame and come back.
    Early,
    /// Due now, within tolerance. Present it.
    OnTime,
    /// Past due but still inside the window. Discard it and look at the next —
    /// which is what stops one stall from becoming permanent lag.
    Late,
    /// So far outside the window that the source has lost the plot. Report it
    /// and carry on without it (CLAUDE.md §5).
    Dropout,
}

/// What the sync policy has been doing.
///
/// Always on, per CLAUDE.md §4.10.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Frames presented.
    pub presented: u64,
    /// Times the previous frame was held because the next was not due.
    pub held: u64,
    /// Frames discarded for arriving late.
    pub dropped: u64,
    /// Frames discarded for falling outside the window.
    pub dropouts: u64,
    /// How far the last classified frame was from its deadline, in nanoseconds.
    ///
    /// Positive means the frame was ahead of where it should be.
    pub drift_nanos: i64,
}

impl SyncStats {
    /// Nothing has happened yet.
    pub const ZERO: SyncStats = SyncStats {
        presented: 0,
        held: 0,
        dropped: 0,
        dropouts: 0,
        drift_nanos: 0,
    };
}

/// Decides where video should be, given where audio is.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use voltra_audio::{AvSync, FrameFate, SyncOffset};
/// use voltra_core::{Fps, Timestamp};
///
/// let sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
/// let audio = Timestamp::from_millis(1_000);
///
/// // A frame stamped for now is on time.
/// assert_eq!(sync.classify(audio, audio), FrameFate::OnTime);
/// // One stamped a second into the future is not due yet.
/// assert_eq!(
///     sync.classify(audio, Timestamp::from_millis(2_000)),
///     FrameFate::Early
/// );
/// // One stamped ten seconds ago has lost the plot.
/// assert_eq!(sync.classify(Timestamp::from_millis(11_000), audio), FrameFate::Dropout);
/// ```
#[derive(Debug, Clone)]
pub struct AvSync {
    offset: SyncOffset,
    tolerance: Duration,
    window: Duration,
    stats: SyncStats,
}

impl AvSync {
    /// How far outside its deadline a frame may fall before it is a dropout.
    ///
    /// One second, the same order libobs buffers to.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(1);

    /// A policy for video at `fps`, with `offset` applied.
    ///
    /// The tolerance is **half a frame**, which is the furthest a frame can be
    /// from the deadline and still be the right one to show.
    #[must_use]
    pub fn new(fps: Fps, offset: SyncOffset) -> Self {
        Self {
            offset,
            tolerance: fps.frame_duration() / 2,
            window: Self::DEFAULT_WINDOW,
            stats: SyncStats::ZERO,
        }
    }

    /// Replace the manual offset.
    pub const fn set_offset(&mut self, offset: SyncOffset) {
        self.offset = offset;
    }

    /// The manual offset in force.
    #[must_use]
    pub const fn offset(&self) -> SyncOffset {
        self.offset
    }

    /// How far from the deadline a frame may be and still be presented.
    #[must_use]
    pub const fn tolerance(&self) -> Duration {
        self.tolerance
    }

    /// Widen or narrow the dropout window.
    pub const fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// What the policy has been doing.
    #[must_use]
    pub const fn stats(&self) -> SyncStats {
        self.stats
    }

    /// Forget the counters, for a session that starts again.
    pub const fn reset_stats(&mut self) {
        self.stats = SyncStats::ZERO;
    }

    /// Where video should be for this instant of audio.
    ///
    /// A positive offset moves the deadline **back**, so a frame that was on
    /// time is now early — which is what "delay the video" means.
    #[must_use]
    pub fn deadline(&self, audio: Timestamp) -> Timestamp {
        let nanos = i128::from(audio.as_nanos()) - i128::from(self.offset.as_nanos());
        Timestamp::from_nanos(u64::try_from(nanos.max(0)).unwrap_or(u64::MAX))
    }

    /// How far `pts` is from its deadline, in nanoseconds. Positive is ahead.
    #[must_use]
    pub fn drift_nanos(&self, audio: Timestamp, pts: Timestamp) -> i64 {
        let deadline = self.deadline(audio);
        let difference = i128::from(pts.as_nanos()) - i128::from(deadline.as_nanos());
        i64::try_from(difference).unwrap_or(if difference > 0 { i64::MAX } else { i64::MIN })
    }

    /// What to do with a frame stamped `pts` at audio time `audio`.
    ///
    /// Pure: nothing here is recorded. Use [`AvSync::decide`] to classify and
    /// count in one go.
    #[must_use]
    pub fn classify(&self, audio: Timestamp, pts: Timestamp) -> FrameFate {
        let drift = self.drift_nanos(audio, pts);
        let tolerance = i64::try_from(self.tolerance.as_nanos()).unwrap_or(i64::MAX);
        let window = i64::try_from(self.window.as_nanos()).unwrap_or(i64::MAX);

        // The boundaries are inclusive on the tolerance and exclusive on the
        // window, so a frame exactly one tolerance away is still presented and
        // one exactly a window away is still merely late. Stated rather than
        // left to whichever comparison got typed.
        if drift.abs() <= tolerance {
            FrameFate::OnTime
        } else if drift > tolerance {
            FrameFate::Early
        } else if -drift <= window {
            FrameFate::Late
        } else {
            FrameFate::Dropout
        }
    }

    /// Classify a frame and record what happened.
    pub fn decide(&mut self, audio: Timestamp, pts: Timestamp) -> FrameFate {
        let fate = self.classify(audio, pts);
        self.stats.drift_nanos = self.drift_nanos(audio, pts);
        match fate {
            FrameFate::OnTime => self.stats.presented += 1,
            FrameFate::Early => self.stats.held += 1,
            FrameFate::Late => self.stats.dropped += 1,
            FrameFate::Dropout => self.stats.dropouts += 1,
        }
        fate
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioClock, AvSync, FrameFate, SyncOffset, SyncStats};
    use std::time::Duration;
    use voltra_core::{Fps, SampleRate, Timestamp};

    const RATE: SampleRate = SampleRate::HZ_48000;

    /// Counting blocks has to give the same answer as counting samples, or the
    /// clock drifts by exactly the rounding of a block duration per block.
    #[test]
    fn counting_blocks_equals_counting_samples() {
        let mut clock = AudioClock::new(RATE);
        for _ in 0..1_000 {
            clock.advance(1024);
        }
        assert_eq!(clock.frames(), 1_024_000);
        assert_eq!(clock.now(), RATE.pts(1_024_000));

        let mut sample_at_a_time = AudioClock::new(RATE);
        for _ in 0..1024 {
            sample_at_a_time.advance(1000);
        }
        assert_eq!(sample_at_a_time.now(), clock.now());
    }

    /// Ten hours in, still exact — the property the whole design exists for.
    #[test]
    fn the_clock_stays_exact_over_hours() {
        let mut clock = AudioClock::new(RATE);
        // Ten hours of 1024-sample blocks.
        let blocks = 48_000u64 * 36_000 / 1024;
        for _ in 0..blocks {
            clock.advance(1024);
        }
        let expected = RATE.pts(blocks * 1024);
        assert_eq!(clock.now(), expected);
        // And within a block of ten hours exactly.
        let error = 36_000_000_000_000i64 - i64::try_from(clock.now().as_nanos()).unwrap();
        assert!(error.abs() < 25_000, "off by {error} ns after ten hours");
    }

    #[test]
    fn the_clock_never_goes_backwards() {
        let mut clock = AudioClock::new(RATE);
        let mut previous = clock.now();
        for frames in [0usize, 1, 1024, 0, 7, 4096] {
            let now = clock.advance(frames);
            assert!(now >= previous, "{now:?} came before {previous:?}");
            previous = now;
        }
        clock.reset();
        assert_eq!(clock.now(), Timestamp::ZERO);
        assert_eq!(clock.frames(), 0);
    }

    /// The test that stops the offset being implemented backwards. Stated in
    /// terms of what a person sees, not in terms of which way the sign goes.
    #[test]
    fn a_positive_offset_delays_the_video() {
        let audio = Timestamp::from_millis(1_000);
        let frame = audio;

        let none = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        assert_eq!(none.classify(audio, frame), FrameFate::OnTime);

        // "Delay the video by 100 ms" must make a frame that was on time become
        // one that is not due yet.
        let delayed = AvSync::new(Fps::FPS_60, SyncOffset::from_millis(100).unwrap());
        assert_eq!(delayed.classify(audio, frame), FrameFate::Early);

        // And advancing it makes the same frame late.
        let advanced = AvSync::new(Fps::FPS_60, SyncOffset::from_millis(-100).unwrap());
        assert_eq!(advanced.classify(audio, frame), FrameFate::Late);
    }

    #[test]
    fn offsets_outside_the_limit_are_refused() {
        assert!(SyncOffset::from_millis(5_000).is_ok());
        assert!(SyncOffset::from_millis(-5_000).is_ok());
        assert!(SyncOffset::from_millis(5_001).is_err());
        assert!(SyncOffset::from_millis(-5_001).is_err());
        // A caller passing microseconds by mistake finds out.
        assert!(SyncOffset::from_millis(100_000).is_err());
        assert_eq!(SyncOffset::from_millis(-40).unwrap().to_string(), "-40 ms");
        assert_eq!(SyncOffset::NONE.as_nanos(), 0);
    }

    /// Each boundary checked on both sides, so none of them is whichever
    /// comparison happened to get typed.
    #[test]
    fn every_boundary_has_a_side() {
        let sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        let tolerance = i64::try_from(sync.tolerance().as_nanos()).unwrap();
        let audio = Timestamp::from_millis(10_000);
        let at = |offset: i64| {
            Timestamp::from_nanos(
                u64::try_from(i64::try_from(audio.as_nanos()).unwrap() + offset).unwrap(),
            )
        };

        // Exactly one tolerance ahead is still on time; one nanosecond more is
        // early.
        assert_eq!(sync.classify(audio, at(tolerance)), FrameFate::OnTime);
        assert_eq!(sync.classify(audio, at(tolerance + 1)), FrameFate::Early);

        // Exactly one tolerance behind is still on time; one more is late.
        assert_eq!(sync.classify(audio, at(-tolerance)), FrameFate::OnTime);
        assert_eq!(sync.classify(audio, at(-tolerance - 1)), FrameFate::Late);

        // Exactly one window behind is still late; one more is a dropout.
        let window = i64::try_from(AvSync::DEFAULT_WINDOW.as_nanos()).unwrap();
        assert_eq!(sync.classify(audio, at(-window)), FrameFate::Late);
        assert_eq!(sync.classify(audio, at(-window - 1)), FrameFate::Dropout);
    }

    /// A source that stops must be reported, not waited for.
    #[test]
    fn a_stalled_source_becomes_a_dropout() {
        let mut sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        let stuck = Timestamp::from_millis(1_000);

        // The clock keeps going while the source does not.
        assert_eq!(
            sync.decide(Timestamp::from_millis(1_000), stuck),
            FrameFate::OnTime
        );
        assert_eq!(
            sync.decide(Timestamp::from_millis(1_500), stuck),
            FrameFate::Late
        );
        assert_eq!(
            sync.decide(Timestamp::from_millis(5_000), stuck),
            FrameFate::Dropout
        );

        let stats = sync.stats();
        assert_eq!(stats.presented, 1);
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.dropouts, 1);
        assert!(
            stats.drift_nanos < 0,
            "the frame was behind, drift should be negative"
        );
    }

    /// A backlog is discarded rather than played out, which is what stops one
    /// stall from becoming permanent lag.
    #[test]
    fn a_backlog_is_discarded_rather_than_accumulated() {
        let mut sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        let audio = Timestamp::from_millis(1_000);

        // Ten frames' worth of backlog, all stamped in the recent past.
        let mut discarded = 0;
        for step in 1..=10u64 {
            let pts = Timestamp::from_millis(1_000 - step * 20);
            if sync.decide(audio, pts) == FrameFate::Late {
                discarded += 1;
            }
        }
        assert_eq!(discarded, 10);
        assert_eq!(sync.stats().presented, 0);
    }

    #[test]
    fn statistics_start_at_zero_and_can_be_reset() {
        let mut sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        assert_eq!(sync.stats(), SyncStats::ZERO);
        sync.decide(Timestamp::ZERO, Timestamp::ZERO);
        assert_eq!(sync.stats().presented, 1);
        sync.reset_stats();
        assert_eq!(sync.stats(), SyncStats::ZERO);
    }

    #[test]
    fn the_offset_and_window_can_be_changed_after_construction() {
        let mut sync = AvSync::new(Fps::FPS_60, SyncOffset::NONE);
        assert_eq!(sync.offset(), SyncOffset::NONE);

        let offset = SyncOffset::from_millis(-25).unwrap();
        sync.set_offset(offset);
        assert_eq!(sync.offset(), offset);

        sync.set_window(Duration::from_millis(100));
        let audio = Timestamp::from_millis(10_000);
        let far_behind = Timestamp::from_millis(9_800);
        assert_eq!(sync.classify(audio, far_behind), FrameFate::Dropout);
    }

    /// The acceptance criterion: a minute of 29.97 fps video against a 48 kHz
    /// audio clock has to present exactly the frames the rate implies. A biased
    /// policy would show up here as a count that walked away.
    #[test]
    fn a_minute_of_drop_frame_video_presents_the_frames_it_should() {
        let fps = Fps::FPS_29_97;
        let mut clock = AudioClock::new(RATE);
        let mut sync = AvSync::new(fps, SyncOffset::NONE);

        // Blocks of 1024 samples, which is 21.3 ms — deliberately not a
        // multiple of the frame period, so the two grids never line up.
        let blocks = u64::from(RATE.hz()) * 60 / 1024;
        let mut next_frame = 0u64;

        for _ in 0..blocks {
            let audio = clock.advance(1024);
            // Present every frame that has come due since the last block.
            loop {
                let pts = fps.pts(next_frame);
                match sync.decide(audio, pts) {
                    FrameFate::OnTime | FrameFate::Late => next_frame += 1,
                    FrameFate::Early | FrameFate::Dropout => break,
                }
            }
        }

        // How many frames should have gone by. Two details the arithmetic has
        // to include, both of which are behaviour and not slack:
        //
        // - a frame due within half a period *ahead* is presented now, because
        //   that is what the tolerance means, so the horizon is
        //   `elapsed + tolerance`;
        // - frame indices start at zero, so the count is the last index plus one.
        let elapsed = clock.now();
        let horizon = u128::from(elapsed.as_nanos()) + sync.tolerance().as_nanos();
        let last_index =
            horizon * u128::from(fps.num()) / (u128::from(fps.den()) * 1_000_000_000u128);
        let expected = u64::try_from(last_index).unwrap() + 1;

        assert!(
            next_frame.abs_diff(expected) <= 1,
            "presented {next_frame} frames in {elapsed}, expected about {expected}"
        );
        assert_eq!(sync.stats().dropouts, 0, "nothing should have dropped out");
        // Every block should have ended by holding, since video is finer-grained
        // than nothing and coarser than the block.
        assert!(sync.stats().held > 0);
    }
}
