//! What an input has been through, counted on both sides.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// A snapshot of an input's counters since it was created.
///
/// Samples are counted at the mix rate, per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputStats {
    /// Blocks the source pushed.
    pub blocks: u64,
    /// Times a block landed after a real gap, filled with silence.
    pub gaps: u64,
    /// Samples of silence those gaps took.
    pub gap_samples: u64,
    /// Samples dropped because they overlapped what was already written.
    pub overlap_samples: u64,
    /// Samples dropped because their moment had already been mixed — on
    /// arrival, or waiting in the ring when their tick passed.
    pub late_samples: u64,
    /// Blocks dropped whole because the ring had no room: the source is
    /// further ahead of the mix than the ring is long.
    pub overflows: u64,
    /// Times the source's timestamps jumped beyond the smoothing threshold.
    pub discontinuities: u64,
    /// The largest jitter smoothed away, in nanoseconds.
    pub max_jitter_ns: u64,
    /// Ticks served whole.
    pub full: u64,
    /// Ticks served in part, the rest silence: a cut.
    pub partial: u64,
    /// Ticks with nothing to serve.
    pub empty: u64,
    /// Samples already waiting beyond the end of the last tick served:
    /// negative when it was short. How much margin the latency leaves.
    pub headroom: i64,
    /// The smallest [`headroom`](Self::headroom) seen; `i64::MAX` before the
    /// first tick.
    pub min_headroom: i64,
}

/// The shared counters behind [`InputStats`].
#[derive(Debug)]
pub(crate) struct StatsCells {
    pub(crate) blocks: AtomicU64,
    pub(crate) gaps: AtomicU64,
    pub(crate) gap_samples: AtomicU64,
    pub(crate) overlap_samples: AtomicU64,
    pub(crate) late_samples: AtomicU64,
    pub(crate) overflows: AtomicU64,
    pub(crate) discontinuities: AtomicU64,
    pub(crate) max_jitter_ns: AtomicU64,
    pub(crate) full: AtomicU64,
    pub(crate) partial: AtomicU64,
    pub(crate) empty: AtomicU64,
    pub(crate) headroom: AtomicI64,
    pub(crate) min_headroom: AtomicI64,
}

impl StatsCells {
    pub(crate) const fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            gaps: AtomicU64::new(0),
            gap_samples: AtomicU64::new(0),
            overlap_samples: AtomicU64::new(0),
            late_samples: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            discontinuities: AtomicU64::new(0),
            max_jitter_ns: AtomicU64::new(0),
            full: AtomicU64::new(0),
            partial: AtomicU64::new(0),
            empty: AtomicU64::new(0),
            headroom: AtomicI64::new(0),
            min_headroom: AtomicI64::new(i64::MAX),
        }
    }

    /// Add to a counter. Relaxed: every counter stands alone.
    pub(crate) fn add(counter: &AtomicU64, amount: u64) {
        if amount > 0 {
            counter.fetch_add(amount, Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self) -> InputStats {
        let load = |cell: &AtomicU64| cell.load(Ordering::Relaxed);
        InputStats {
            blocks: load(&self.blocks),
            gaps: load(&self.gaps),
            gap_samples: load(&self.gap_samples),
            overlap_samples: load(&self.overlap_samples),
            late_samples: load(&self.late_samples),
            overflows: load(&self.overflows),
            discontinuities: load(&self.discontinuities),
            max_jitter_ns: load(&self.max_jitter_ns),
            full: load(&self.full),
            partial: load(&self.partial),
            empty: load(&self.empty),
            headroom: self.headroom.load(Ordering::Relaxed),
            min_headroom: self.min_headroom.load(Ordering::Relaxed),
        }
    }
}
