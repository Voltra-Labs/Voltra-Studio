//! Reports during a recording: what the process holds every so many seconds
//! of recorded time, so a long recording shows whether anything grows.

use voltra_core::Timestamp;

use crate::heap;

/// Writes a report line to standard error at each multiple of a period of
/// recorded time.
///
/// ```text
/// report at=60 heap_bytes=1234567 heap_blocks=345 allocations=67890 resident_kib=35448 queued=3
/// ```
///
/// Key=value, so a script or a test can read it without guessing. Reading the
/// resident size is a system call per report, never per frame.
#[derive(Debug)]
pub(super) struct Reporter {
    every: u64,
    next: u64,
    status: String,
}

impl Reporter {
    /// A report every `seconds` of recorded time; 0 for none.
    pub(super) fn new(seconds: u64) -> Self {
        Self {
            every: seconds,
            next: seconds,
            status: String::with_capacity(heap::STATUS_CAPACITY),
        }
    }

    /// The recording has reached `recorded` past its start: report every
    /// period it has crossed.
    pub(super) fn reached(&mut self, recorded: Timestamp, queued: usize) {
        if self.every == 0 {
            return;
        }
        while recorded.as_nanos() >= self.next.saturating_mul(1_000_000_000) {
            let heap = heap::stats();
            let resident = heap::resident(&mut self.status).map_or(0, |resident| resident.now_kib);
            eprintln!(
                "report at={} heap_bytes={} heap_blocks={} allocations={} resident_kib={} queued={}",
                self.next, heap.live_bytes, heap.live_blocks, heap.allocations, resident, queued
            );
            self.next += self.every;
        }
    }
}
