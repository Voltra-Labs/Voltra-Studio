//! What the process holds: a counting wrapper around the system allocator, and
//! the resident size the operating system reports.
//!
//! The counter is always on, as OBS's `bmalloc` count is: three relaxed atomic
//! additions per allocation do not show up next to the allocation itself. It
//! counts bytes as well as blocks, so a buffer that grows by reallocation shows
//! up too, and it can be read at any moment rather than only on exit — a
//! queue that grows for an hour and drains on close leaks nothing on exit.
//! It sees every Rust allocation, dependencies included; what C libraries such
//! as `FFmpeg` reserve with their own `malloc` only shows in the resident size.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// The system allocator, counted. Installed as the `#[global_allocator]` of
/// the `voltra` binary.
#[derive(Debug)]
pub struct Counting;

/// The heap as the counter sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    /// Bytes allocated and not yet freed.
    pub live_bytes: u64,
    /// Blocks allocated and not yet freed.
    pub live_blocks: u64,
    /// The most bytes ever live at once.
    pub peak_bytes: u64,
    /// Allocations and reallocations so far.
    pub allocations: u64,
}

/// The heap now.
pub fn stats() -> HeapStats {
    COUNTERS.snapshot()
}

/// The process's resident memory, in KiB, now and at its peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resident {
    /// Resident now (`VmRSS`).
    pub now_kib: u64,
    /// The most ever resident (`VmHWM`).
    pub peak_kib: u64,
}

/// What the operating system says the process holds, from
/// `/proc/self/status`. That file gives KiB directly, so — unlike reading
/// `/proc/self/statm`, as OBS does — no page size, and no `libc`, is needed.
/// `None` anywhere else than Linux, or if the file cannot be read.
///
/// The file is read into `buffer`, which keeps its capacity: with a few KiB
/// already reserved, asking does not allocate, so a report does not show up in
/// the counts it reports.
pub fn resident(buffer: &mut String) -> Option<Resident> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;

        buffer.clear();
        std::fs::File::open("/proc/self/status")
            .and_then(|mut file| file.read_to_string(buffer))
            .ok()?;
        parse_status(buffer)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = buffer;
        None
    }
}

/// Room for `/proc/self/status`, which is about 1.5 KiB.
pub const STATUS_CAPACITY: usize = 8 * 1024;

/// `VmRSS` and `VmHWM` from the text of `/proc/<pid>/status`.
fn parse_status(status: &str) -> Option<Resident> {
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        line[name.len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()
    };
    Some(Resident {
        now_kib: field("VmRSS:")?,
        peak_kib: field("VmHWM:")?,
    })
}

static COUNTERS: Counters = Counters::new();

/// The counts, apart from the allocator so they can be tested on their own.
/// Relaxed: they are tallies, and publish nothing else.
struct Counters {
    live_bytes: AtomicU64,
    live_blocks: AtomicU64,
    peak_bytes: AtomicU64,
    allocations: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            live_bytes: AtomicU64::new(0),
            live_blocks: AtomicU64::new(0),
            peak_bytes: AtomicU64::new(0),
            allocations: AtomicU64::new(0),
        }
    }

    fn allocated(&self, size: usize) {
        let size = bytes(size);
        self.allocations.fetch_add(1, Ordering::Relaxed);
        self.live_blocks.fetch_add(1, Ordering::Relaxed);
        let live = self.live_bytes.fetch_add(size, Ordering::Relaxed) + size;
        self.peak_bytes.fetch_max(live, Ordering::Relaxed);
    }

    fn freed(&self, size: usize) {
        self.live_blocks.fetch_sub(1, Ordering::Relaxed);
        self.live_bytes.fetch_sub(bytes(size), Ordering::Relaxed);
    }

    fn reallocated(&self, old: usize, new: usize) {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        if new >= old {
            let grown = bytes(new - old);
            let live = self.live_bytes.fetch_add(grown, Ordering::Relaxed) + grown;
            self.peak_bytes.fetch_max(live, Ordering::Relaxed);
        } else {
            self.live_bytes
                .fetch_sub(bytes(old - new), Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> HeapStats {
        HeapStats {
            live_bytes: self.live_bytes.load(Ordering::Relaxed),
            live_blocks: self.live_blocks.load(Ordering::Relaxed),
            peak_bytes: self.peak_bytes.load(Ordering::Relaxed),
            allocations: self.allocations.load(Ordering::Relaxed),
        }
    }
}

fn bytes(size: usize) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees are this allocator's. The only addition
// is a few atomic additions on statics, which neither allocate nor unwind, made
// only when `System` succeeded, so a failed call is not counted.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            COUNTERS.allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `alloc_zeroed`'s
        // contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            COUNTERS.allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from this allocator, so from `System`, with
        // `layout`; the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) };
        COUNTERS.freed(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `pointer` came from this allocator, so from `System`, with
        // `layout`; the caller upholds `realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        // On failure the old block is untouched, and so are the counts.
        if !moved.is_null() {
            COUNTERS.reallocated(layout.size(), new_size);
        }
        moved
    }
}

#[cfg(test)]
mod tests;
