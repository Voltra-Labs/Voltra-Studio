//! From a capture's own timestamps to the process's monotonic clock.
//!
//! A system capture stamps each frame on the system's monotonic clock
//! (`CLOCK_MONOTONIC` for PipeWire, QPC on Windows). The process measures time
//! with [`Instant`], whose zero is unknowable but which runs on the same clock.
//! The two differ by a constant, measured once by reading both together, and
//! measured again if a stamp ever lands more than [`MAX_JUMP`] away from when
//! it arrived — the 2 s `MAX_TS_VAR` with which OBS re-anchors a source.

use std::sync::OnceLock;
use std::time::Instant;

use voltra_core::Timestamp;

/// How far a mapped stamp may be from its arrival before the mapping is
/// measured again. OBS's `MAX_TS_VAR`.
pub(crate) const MAX_JUMP: u64 = 2_000_000_000;

/// The process's monotonic time, from an epoch fixed on first use. Every
/// capture stamps its frames on this timeline.
pub(crate) fn process_time() -> Timestamp {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = EPOCH.get_or_init(Instant::now).elapsed();
    Timestamp::from_nanos(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
}

/// The offset from a capture's clock to the process's, and how often it had to
/// be measured.
#[derive(Debug, Default)]
pub(crate) struct ClockMap {
    /// Process time minus capture time, in nanoseconds.
    offset: Option<i128>,
    anchors: u64,
}

impl ClockMap {
    /// The process time of a frame stamped `stamp` on the capture's clock,
    /// which arrived when the capture's clock read `capture_now` and the
    /// process's read `process_now`.
    pub(crate) fn map(
        &mut self,
        stamp: i64,
        capture_now: i64,
        process_now: Timestamp,
    ) -> Timestamp {
        let process_now = i128::from(process_now.as_nanos());
        let measured = process_now - i128::from(capture_now);
        let offset = *self.offset.get_or_insert_with(|| {
            self.anchors += 1;
            measured
        });
        let mut mapped = i128::from(stamp) + offset;
        if (mapped - process_now).unsigned_abs() > u128::from(MAX_JUMP) {
            self.offset = Some(measured);
            self.anchors += 1;
            mapped = i128::from(stamp) + measured;
        }
        Timestamp::from_nanos(u64::try_from(mapped.max(0)).unwrap_or(u64::MAX))
    }

    /// How many times the offset was measured: once, plus once per jump.
    pub(crate) fn anchors(&self) -> u64 {
        self.anchors
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::Timestamp;

    use super::{ClockMap, process_time};

    #[test]
    fn stamps_keep_their_spacing_on_the_process_clock() {
        let mut map = ClockMap::default();
        // The capture's clock reads 5 s when the process's reads 1 s.
        let first = map.map(4_990_000_000, 5_000_000_000, Timestamp::from_millis(1_000));
        assert_eq!(first, Timestamp::from_millis(990));
        // Later frames keep their own spacing, whatever their arrival jitter.
        let later = map.map(5_006_666_667, 5_010_000_000, Timestamp::from_millis(1_011));
        assert_eq!(later, Timestamp::from_nanos(1_006_666_667));
        assert_eq!(map.anchors(), 1);
    }

    #[test]
    fn a_jump_of_more_than_two_seconds_re_anchors() {
        let mut map = ClockMap::default();
        map.map(5_000_000_000, 5_000_000_000, Timestamp::from_millis(1_000));
        // The source's clock restarts near zero.
        let after = map.map(10_000_000, 10_000_000, Timestamp::from_millis(2_000));
        assert_eq!(after, Timestamp::from_millis(2_000));
        assert_eq!(map.anchors(), 2);
    }

    #[test]
    fn the_process_clock_runs_forward() {
        let a = process_time();
        let b = process_time();
        assert!(b >= a);
    }
}
