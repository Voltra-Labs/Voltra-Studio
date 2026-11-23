//! The mix's half of an input: one exact block per tick.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use voltra_core::{AudioBuffer, ChannelLayout, Timestamp};

use super::ring::Ring;
use super::spec::{InputSpec, index_of};
use super::stats::{InputStats, StatsCells};
use crate::{Error, MixSpec, Result};

/// What a tick got from an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PullOutcome {
    /// Every sample of the tick: mix it.
    Full,
    /// Some samples, the rest silence. Mix it; it is a cut, and it is counted.
    Partial,
    /// Nothing for this tick. The block was not touched: leave the track out
    /// of this mix and the mixer fades it and counts it missing.
    Empty,
}

/// The reading half of an input, for the mix's thread.
///
/// [`pull`](Self::pull) never allocates and never waits for the source.
#[derive(Debug)]
pub struct InputReader {
    mix: MixSpec,
    layout: ChannelLayout,
    ring: Arc<Ring>,
    stats: Arc<StatsCells>,
}

impl InputReader {
    pub(crate) fn new(spec: InputSpec, ring: Arc<Ring>, stats: Arc<StatsCells>) -> Self {
        Self {
            mix: spec.mix,
            layout: spec.layout,
            ring,
            stats,
        }
    }

    /// Fill `out` with this input's samples for the tick starting at `tick`.
    ///
    /// `out` must be at the mix's rate and block length, in the source's
    /// layout. On [`Full`](PullOutcome::Full) and
    /// [`Partial`](PullOutcome::Partial) it is stamped `tick`, so it goes
    /// straight into [`Mixer::mix`](crate::Mixer::mix) for this source's track.
    /// Samples older than the tick are discarded as late: their moment has been
    /// mixed.
    ///
    /// # Errors
    ///
    /// [`Error::FormatMismatch`] for an `out` of the wrong rate or layout, and
    /// [`Error::OutputTooSmall`] for one not exactly a mix block long.
    pub fn pull(&mut self, tick: Timestamp, out: &mut AudioBuffer) -> Result<PullOutcome> {
        if out.rate() != self.mix.rate || out.layout() != self.layout {
            return Err(Error::FormatMismatch {
                expected_rate: self.mix.rate,
                expected_layout: self.layout,
                found_rate: out.rate(),
                found_layout: out.layout(),
            });
        }
        if out.frames() != self.mix.frames {
            return Err(Error::OutputTooSmall {
                needed: self.mix.frames,
                capacity: out.frames(),
            });
        }

        let frames = self.mix.frames as u64;
        let first = index_of(self.mix.rate, tick);
        let end = first + frames;
        // Written first: see the ring's documentation for why the order matters.
        let written = self.ring.written_index();
        let read = self.ring.read_index();

        if read < first && written > read {
            StatsCells::add(&self.stats.late_samples, written.min(first) - read);
        }
        let low = read.max(first);
        let high = written.min(end);
        self.ring.advance_read(end);

        let headroom =
            i64::try_from(written).unwrap_or(i64::MAX) - i64::try_from(end).unwrap_or(i64::MAX);
        self.stats.headroom.store(headroom, Ordering::Relaxed);
        self.stats
            .min_headroom
            .fetch_min(headroom, Ordering::Relaxed);

        if low >= high {
            StatsCells::add(&self.stats.empty, 1);
            return Ok(PullOutcome::Empty);
        }

        let before = usize::try_from(low - first).unwrap_or(0);
        let data = usize::try_from(high - low).unwrap_or(0);
        for (channel, samples) in out.channels_mut().enumerate() {
            let (head, rest) = samples.split_at_mut(before.min(samples.len()));
            head.fill(0.0);
            let (body, tail) = rest.split_at_mut(data.min(rest.len()));
            self.ring.read(channel, low, body);
            tail.fill(0.0);
        }
        out.set_pts(tick);

        if low == first && high == end {
            StatsCells::add(&self.stats.full, 1);
            Ok(PullOutcome::Full)
        } else {
            StatsCells::add(&self.stats.partial, 1);
            Ok(PullOutcome::Partial)
        }
    }

    /// What this input has been through.
    pub fn stats(&self) -> InputStats {
        self.stats.snapshot()
    }
}
