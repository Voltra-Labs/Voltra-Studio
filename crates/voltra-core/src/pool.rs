//! Recycling video frames instead of allocating them.
//!
//! Allocating a 1080p BGRA frame costs 387 µs — roughly 46 µs per megabyte,
//! because `vec![0; n]` has to touch every page. At 60 fps that is 23 ms of
//! wasted work per second of broadcast. libobs solves it with `async_cache`, a
//! free list per source; this is the same idea with ownership enforced by the
//! type system.
//!
//! # Ownership, not a `used` flag
//!
//! In libobs a cached frame carries a `used` boolean and an atomic refcount, and
//! correctness rests on nobody touching a frame they gave back. Here [`acquire`]
//! hands the frame over by value: while you hold it, the pool does not. Using a
//! frame that is back in the free list does not compile.
//!
//! # No lock
//!
//! `cache_video` holds a mutex while it scans. This pool holds none, because it
//! is owned by one pipeline stage rather than shared between threads — a mutex
//! on the render path is forbidden (CLAUDE.md §4.3). Returning frames across
//! threads will be a channel, not a lock.
//!
//! [`acquire`]: FramePool::acquire

use crate::Result;
use crate::frame::{FrameSize, VideoFrame};
use crate::pixel::PixelFormat;
use crate::time::Timestamp;

/// How many acquisitions a frame may sit idle before the pool lets it go.
///
/// libobs uses five (`MAX_UNUSED_FRAME_DURATION`), so a burst does not leave
/// memory pinned forever once the demand drops.
pub const MAX_IDLE_ROUNDS: u8 = 5;

/// Default ceiling on retained frames.
///
/// libobs caps its async queue at thirty (`MAX_ASYNC_FRAMES`) and flushes past
/// that: better to drop frames from a stalled source than to grow without bound.
pub const DEFAULT_CAPACITY: usize = 30;

/// Counters describing what the pool has been doing.
///
/// A pool without counters is a pool nobody can tell is working (CLAUDE.md
/// §4.10). A healthy steady state shows `reused` climbing while `allocated`
/// stays flat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Frames allocated because the free list was empty.
    pub allocated: u64,
    /// Frames handed out from the free list.
    pub reused: u64,
    /// Frames accepted back into the free list.
    pub returned: u64,
    /// Frames dropped: wrong geometry, no room, or aged out.
    pub discarded: u64,
}

impl PoolStats {
    /// Share of acquisitions served without allocating, from 0.0 to 1.0.
    pub fn hit_rate(&self) -> f64 {
        let total = self.allocated + self.reused;
        if total == 0 {
            return 0.0;
        }
        // Exact for any realistic count: f64 holds integers up to 2^53.
        #[allow(clippy::cast_precision_loss, reason = "counts stay far below 2^53")]
        {
            self.reused as f64 / total as f64
        }
    }
}

/// A frame waiting to be handed out again.
#[derive(Debug)]
struct IdleFrame {
    frame: VideoFrame,
    idle_rounds: u8,
}

/// A free list of frames of one fixed geometry.
///
/// # Examples
///
/// ```
/// use voltra_core::{FramePool, FrameSize, PixelFormat, Timestamp};
///
/// let size = FrameSize::new(1920, 1080)?;
/// let mut pool = FramePool::new(PixelFormat::Bgra8, size);
///
/// let frame = pool.acquire(Timestamp::ZERO)?;
/// pool.release(frame);
///
/// // The next frame comes back from the free list rather than the allocator.
/// let recycled = pool.acquire(Timestamp::ZERO)?;
/// assert_eq!(pool.stats().reused, 1);
/// assert_eq!(pool.stats().allocated, 1);
/// # let _ = recycled;
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct FramePool {
    format: PixelFormat,
    size: FrameSize,
    capacity: usize,
    idle: Vec<IdleFrame>,
    stats: PoolStats,
}

impl FramePool {
    /// Build a pool for one geometry, with [`DEFAULT_CAPACITY`].
    pub fn new(format: PixelFormat, size: FrameSize) -> Self {
        Self::with_capacity(format, size, DEFAULT_CAPACITY)
    }

    /// Build a pool that retains at most `capacity` idle frames.
    pub fn with_capacity(format: PixelFormat, size: FrameSize, capacity: usize) -> Self {
        Self {
            format,
            size,
            capacity,
            idle: Vec::with_capacity(capacity.min(DEFAULT_CAPACITY)),
            stats: PoolStats::default(),
        }
    }

    /// The format this pool hands out.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// The size this pool hands out.
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// How many frames are waiting in the free list.
    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }

    /// The counters so far.
    pub const fn stats(&self) -> PoolStats {
        self.stats
    }

    /// Take a frame, recycling one when possible.
    ///
    /// # Stale pixels
    ///
    /// A recycled frame still holds the image written into it last time. That is
    /// the whole point — clearing it would reintroduce the cost the pool exists
    /// to remove — but it means the caller must overwrite every pixel it cares
    /// about. When that is not guaranteed, use [`FramePool::acquire_zeroed`].
    ///
    /// # Errors
    ///
    /// Propagates from [`VideoFrame::new`] when the free list is empty and the
    /// geometry cannot be allocated.
    pub fn acquire(&mut self, pts: Timestamp) -> Result<VideoFrame> {
        self.age_idle_frames();

        if let Some(idle) = self.idle.pop() {
            self.stats.reused += 1;
            let mut frame = idle.frame;
            frame.set_pts(pts);
            return Ok(frame);
        }

        self.stats.allocated += 1;
        VideoFrame::new(self.format, self.size, pts)
    }

    /// Take a frame with every byte cleared.
    ///
    /// Costs a `memset` — about 410 µs for 1080p BGRA — so prefer
    /// [`FramePool::acquire`] whenever the frame is fully overwritten anyway.
    ///
    /// # Errors
    ///
    /// As [`FramePool::acquire`].
    pub fn acquire_zeroed(&mut self, pts: Timestamp) -> Result<VideoFrame> {
        let mut frame = self.acquire(pts)?;
        frame.as_bytes_mut().fill(0);
        Ok(frame)
    }

    /// Give a frame back.
    ///
    /// A frame of the wrong geometry, or one arriving at a full pool, is dropped
    /// and counted. Forgetting to call this leaks no memory — the frame is freed
    /// normally — it only wastes the recycling, which the counters will show.
    pub fn release(&mut self, frame: VideoFrame) {
        if frame.format() != self.format || frame.size() != self.size {
            self.stats.discarded += 1;
            return;
        }
        if self.idle.len() >= self.capacity {
            self.stats.discarded += 1;
            return;
        }
        self.stats.returned += 1;
        self.idle.push(IdleFrame {
            frame,
            idle_rounds: 0,
        });
    }

    /// Point the pool at a different geometry.
    ///
    /// Frames of the old geometry are dropped, as `free_async_cache` does in
    /// libobs when the source resolution or format changes. A no-op when nothing
    /// changed, so it is safe to call every frame.
    pub fn reconfigure(&mut self, format: PixelFormat, size: FrameSize) {
        if format == self.format && size == self.size {
            return;
        }
        self.format = format;
        self.size = size;
        self.clear();
    }

    /// Drop every idle frame.
    pub fn clear(&mut self) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the free list is bounded by capacity, far below u64::MAX"
        )]
        {
            self.stats.discarded += self.idle.len() as u64;
        }
        self.idle.clear();
    }

    /// Age the free list and drop whatever has sat unused too long.
    ///
    /// Mirrors `clean_cache`: an idle frame survives [`MAX_IDLE_ROUNDS`]
    /// acquisitions, so a burst of demand does not pin memory once it passes.
    fn age_idle_frames(&mut self) {
        let before = self.idle.len();
        self.idle.retain_mut(|idle| {
            idle.idle_rounds += 1;
            idle.idle_rounds < MAX_IDLE_ROUNDS
        });
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the free list is bounded by capacity"
        )]
        {
            self.stats.discarded += (before - self.idle.len()) as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePool, MAX_IDLE_ROUNDS};
    use crate::frame::{FrameSize, VideoFrame};
    use crate::pixel::PixelFormat;
    use crate::time::Timestamp;

    fn size(width: u32, height: u32) -> FrameSize {
        FrameSize::new(width, height).expect("valid size")
    }

    fn pool() -> FramePool {
        FramePool::new(PixelFormat::Bgra8, size(64, 64))
    }

    #[test]
    fn a_released_frame_comes_back_as_the_same_buffer() {
        let mut pool = pool();
        let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
        let address = frame.as_bytes().as_ptr();
        pool.release(frame);

        let recycled = pool.acquire(Timestamp::ZERO).expect("acquire");
        assert_eq!(
            recycled.as_bytes().as_ptr(),
            address,
            "the pool allocated instead of recycling"
        );
        assert_eq!(pool.stats().allocated, 1);
        assert_eq!(pool.stats().reused, 1);
    }

    #[test]
    fn an_empty_pool_allocates() {
        let mut pool = pool();
        for _ in 0..3 {
            let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
            // Dropped rather than released, so the free list stays empty.
            drop(frame);
        }
        assert_eq!(pool.stats().allocated, 3);
        assert_eq!(pool.stats().reused, 0);
        assert_eq!(pool.idle_count(), 0);
    }

    /// The documented hazard of recycling, pinned by a test so it is a decision
    /// rather than a surprise.
    #[test]
    fn recycled_frames_keep_their_old_pixels() {
        let mut pool = pool();
        let mut frame = pool.acquire(Timestamp::ZERO).expect("acquire");
        frame.as_bytes_mut().fill(0xAB);
        pool.release(frame);

        let recycled = pool.acquire(Timestamp::ZERO).expect("acquire");
        assert!(recycled.as_bytes().iter().all(|&byte| byte == 0xAB));
        pool.release(recycled);

        let cleared = pool.acquire_zeroed(Timestamp::ZERO).expect("acquire");
        assert!(cleared.as_bytes().iter().all(|&byte| byte == 0));
    }

    #[test]
    fn acquire_sets_the_requested_timestamp() {
        let mut pool = pool();
        let frame = pool.acquire(Timestamp::from_millis(40)).expect("acquire");
        assert_eq!(frame.pts(), Timestamp::from_millis(40));
        pool.release(frame);

        let recycled = pool.acquire(Timestamp::from_millis(80)).expect("acquire");
        assert_eq!(recycled.pts(), Timestamp::from_millis(80));
    }

    #[test]
    fn frames_of_the_wrong_geometry_never_enter_the_free_list() {
        let mut pool = pool();

        let wrong_size = VideoFrame::new(PixelFormat::Bgra8, size(32, 32), Timestamp::ZERO)
            .expect("valid frame");
        pool.release(wrong_size);

        let wrong_format =
            VideoFrame::new(PixelFormat::I420, size(64, 64), Timestamp::ZERO).expect("valid frame");
        pool.release(wrong_format);

        assert_eq!(pool.idle_count(), 0);
        assert_eq!(pool.stats().discarded, 2);
        assert_eq!(pool.stats().returned, 0);
    }

    #[test]
    fn the_free_list_never_exceeds_its_capacity() {
        let mut pool = FramePool::with_capacity(PixelFormat::Bgra8, size(64, 64), 2);
        for _ in 0..5 {
            let frame =
                VideoFrame::new(PixelFormat::Bgra8, size(64, 64), Timestamp::ZERO).expect("frame");
            pool.release(frame);
        }
        assert_eq!(pool.idle_count(), 2);
        assert_eq!(pool.stats().returned, 2);
        assert_eq!(pool.stats().discarded, 3);
    }

    #[test]
    fn idle_frames_are_let_go_after_enough_rounds() {
        let mut pool = pool();

        // Park two frames, then keep acquiring without returning them.
        let first = pool.acquire(Timestamp::ZERO).expect("acquire");
        let second = pool.acquire(Timestamp::ZERO).expect("acquire");
        pool.release(first);
        pool.release(second);
        assert_eq!(pool.idle_count(), 2);

        // Each acquisition takes one frame and ages the rest.
        for _ in 0..MAX_IDLE_ROUNDS {
            let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
            drop(frame);
        }
        assert_eq!(pool.idle_count(), 0, "idle frames should have aged out");
    }

    #[test]
    fn reconfiguring_drops_frames_of_the_old_geometry() {
        let mut pool = pool();
        let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
        pool.release(frame);
        assert_eq!(pool.idle_count(), 1);

        // Same geometry: nothing happens.
        pool.reconfigure(PixelFormat::Bgra8, size(64, 64));
        assert_eq!(pool.idle_count(), 1);

        pool.reconfigure(PixelFormat::Nv12, size(64, 64));
        assert_eq!(pool.idle_count(), 0);
        assert_eq!(pool.format(), PixelFormat::Nv12);

        let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
        assert_eq!(frame.format(), PixelFormat::Nv12);
    }

    #[test]
    fn the_steady_state_stops_allocating() {
        let mut pool = pool();
        // One frame in flight at a time, as a pipeline stage would.
        for _ in 0..100 {
            let frame = pool.acquire(Timestamp::ZERO).expect("acquire");
            pool.release(frame);
        }
        assert_eq!(pool.stats().allocated, 1, "only the first frame is new");
        assert_eq!(pool.stats().reused, 99);
        assert!((pool.stats().hit_rate() - 0.99).abs() < 1e-9);
    }

    #[test]
    fn an_empty_pool_reports_a_zero_hit_rate() {
        assert!((pool().stats().hit_rate() - 0.0).abs() < f64::EPSILON);
        assert_eq!(
            FramePool::new(PixelFormat::Bgra8, size(8, 8)).idle_count(),
            0
        );
    }
}
