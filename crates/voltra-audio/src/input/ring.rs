//! A single-producer, single-consumer ring of planar samples.
//!
//! Positions are **absolute sample indices** that only grow; the slot of index
//! `i` is `i mod capacity`. The producer stores samples and then publishes how
//! far it has written with `Release`; the consumer loads that with `Acquire`,
//! reads, and advances how far it has read, which the producer loads with
//! `Acquire` before overwriting. Neither ever waits for the other.
//!
//! The read position only moves forward, by `fetch_max`, and **either side may
//! move it**: the consumer as ticks pass, and the producer when the ring holds
//! nothing unread and a block lands further ahead than the ring is long — the
//! skipped indices carry no data, so nothing is lost. A consumer must load the
//! written position *before* the read position: the producer raises the read
//! position before it publishes, so that order never pairs a new written
//! position with a read position from before the jump.
//!
//! Every sample is an `AtomicU32` holding `f32` bits, accessed with `Relaxed`.
//! That keeps the ring in safe code — no `UnsafeCell`, no `unsafe` — and makes
//! misuse harmless: the worst a protocol mistake can do is read a stale sample.
//! A relaxed 32-bit store or load is an ordinary move on x86 and ARM; the cost
//! is that the copy does not vectorise, which the benchmark measures.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Debug)]
pub(crate) struct Ring {
    /// `channels` runs of `capacity` slots.
    samples: Box<[AtomicU32]>,
    capacity: usize,
    /// One past the last index the producer has published.
    written: AtomicU64,
    /// The first index still wanted: everything before it is used or skipped.
    read: AtomicU64,
}

impl Ring {
    /// A ring for `channels` holding at least `frames` samples per channel,
    /// rounded up to a power of two. Both positions start at `start`.
    pub(crate) fn new(channels: usize, frames: usize, start: u64) -> Self {
        let capacity = frames.max(1).next_power_of_two();
        Self {
            samples: (0..channels * capacity)
                .map(|_| AtomicU32::new(0))
                .collect(),
            capacity,
            written: AtomicU64::new(start),
            read: AtomicU64::new(start),
        }
    }

    pub(crate) const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The consumer's position, as the producer must see it before writing.
    pub(crate) fn read_index(&self) -> u64 {
        self.read.load(Ordering::Acquire)
    }

    /// The producer's position, as the consumer must see it before reading.
    pub(crate) fn written_index(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Producer: store `samples` for `channel` from index `at`. The caller has
    /// checked that `at .. at + len` does not overwrite unread samples.
    pub(crate) fn write(&self, channel: usize, at: u64, samples: &[f32]) {
        for (slot, &sample) in self.slots(channel, at, samples.len()).zip(samples) {
            slot.store(sample.to_bits(), Ordering::Relaxed);
        }
    }

    /// Producer: store `len` zeros for `channel` from index `at`.
    pub(crate) fn silence(&self, channel: usize, at: u64, len: usize) {
        for slot in self.slots(channel, at, len) {
            slot.store(0, Ordering::Relaxed);
        }
    }

    /// Producer: make everything before `written` visible to the consumer.
    pub(crate) fn publish(&self, written: u64) {
        self.written.store(written, Ordering::Release);
    }

    /// Consumer: load `out.len()` samples of `channel` from index `at`. The
    /// caller has checked they are published.
    pub(crate) fn read(&self, channel: usize, at: u64, out: &mut [f32]) {
        let len = out.len();
        for (slot, sample) in self.slots(channel, at, len).zip(out) {
            *sample = f32::from_bits(slot.load(Ordering::Relaxed));
        }
    }

    /// Move the read position forward to `read`, never back: the consumer
    /// releasing what it has used, or the producer skipping an empty stretch.
    pub(crate) fn advance_read(&self, read: u64) {
        self.read.fetch_max(read, Ordering::AcqRel);
    }

    /// The slots of `channel` for indices `at .. at + len`, wrapping around.
    fn slots(&self, channel: usize, at: u64, len: usize) -> impl Iterator<Item = &AtomicU32> {
        let len = len.min(self.capacity);
        let run = self
            .samples
            .get(channel * self.capacity..(channel + 1) * self.capacity)
            .unwrap_or(&[]);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "reduced modulo a usize capacity first"
        )]
        let start = (at % self.capacity as u64) as usize;
        let first = len.min(self.capacity - start);
        let head = run.get(start..start + first).unwrap_or(&[]);
        let tail = run.get(..len - first).unwrap_or(&[]);
        head.iter().chain(tail)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
mod tests {
    use std::sync::Arc;

    use super::Ring;

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        assert_eq!(Ring::new(2, 1000, 0).capacity(), 1024);
        assert_eq!(Ring::new(1, 1024, 0).capacity(), 1024);
    }

    #[test]
    fn what_is_written_is_read_back_across_the_wrap() {
        let ring = Ring::new(2, 8, 1_000_005);
        let left = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let right = [-1.0, -2.0, -3.0, -4.0, -5.0, -6.0];
        // Index 1_000_005 sits at slot 5 of 8: this write wraps.
        ring.write(0, 1_000_005, &left);
        ring.write(1, 1_000_005, &right);
        ring.publish(1_000_011);
        assert_eq!(ring.written_index(), 1_000_011);

        let mut out = [0.0; 6];
        ring.read(0, 1_000_005, &mut out);
        assert_eq!(out, left);
        ring.read(1, 1_000_005, &mut out);
        assert_eq!(out, right);

        ring.silence(0, 1_000_007, 2);
        ring.read(0, 1_000_005, &mut out);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0, 5.0, 6.0]);
        ring.advance_read(1_000_011);
        ring.advance_read(1_000_000);
        assert_eq!(
            ring.read_index(),
            1_000_011,
            "the read position never moves back"
        );
    }

    #[test]
    fn a_concurrent_stream_arrives_whole_and_in_order() {
        const TOTAL: u64 = 2_000_000;
        let ring = Arc::new(Ring::new(1, 4096, 0));
        let producer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let mut written = 0u64;
                let mut chunk = [0.0f32; 300];
                while written < TOTAL {
                    let free = ring.capacity() as u64 - (written - ring.read_index());
                    let len = chunk
                        .len()
                        .min(free as usize)
                        .min((TOTAL - written) as usize);
                    if len == 0 {
                        std::hint::spin_loop();
                        continue;
                    }
                    for (offset, sample) in chunk.iter_mut().take(len).enumerate() {
                        // Exact in f32 below 2^24, so the check is exact too.
                        *sample = ((written + offset as u64) % 16_777_216) as f32;
                    }
                    ring.write(0, written, &chunk[..len]);
                    written += len as u64;
                    ring.publish(written);
                }
            })
        };

        let mut read = 0u64;
        let mut chunk = [0.0f32; 257];
        while read < TOTAL {
            let available = ring.written_index() - read;
            let len = chunk.len().min(available as usize);
            if len == 0 {
                std::hint::spin_loop();
                continue;
            }
            ring.read(0, read, &mut chunk[..len]);
            for (offset, &sample) in chunk.iter().take(len).enumerate() {
                assert_eq!(sample, ((read + offset as u64) % 16_777_216) as f32);
            }
            read += len as u64;
            ring.advance_read(read);
        }
        producer.join().unwrap();
    }
}
