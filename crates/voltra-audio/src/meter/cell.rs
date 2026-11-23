//! The atomics a meter accumulates into.
//!
//! One writer (the audio thread) adds; one reader takes and resets. Per channel:
//!
//! - **Peak and true peak** as `f32` bits with `fetch_max`. For non-negative,
//!   non-NaN floats the bit patterns order exactly like the values, so an
//!   integer maximum is a float maximum, wait-free. Peaks are computed so that
//!   NaN never gets here.
//! - **Energy and frame count packed in one word**, energy as `f32` in the high
//!   half and frames as `u32` in the low half, so a reader can never see one
//!   block's energy without its length. Adding is a compare-and-swap that only
//!   fails when the reader has just taken: lock-free, at most one retry per
//!   read.
//!
//! The per-channel frame count saturates at `u32::MAX`, 24.8 hours at 48 kHz.
//! A meter nobody has read for a day over-reads its RMS on that one reading.
//!
//! How much audio a reading covers is a **separate, meter-wide** counter, added
//! once per block after every channel. Deriving it from the channels' own
//! counts double-counts: a read that lands between channel 0 and channel 1 of a
//! block sees that block in one channel now and in the other next time.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use voltra_core::MAX_CHANNELS;

/// What one channel accumulated between two reads.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Taken {
    pub(crate) peak: f32,
    pub(crate) true_peak: f32,
    pub(crate) energy: f32,
    pub(crate) frames: u32,
}

#[derive(Debug)]
struct LevelCell {
    peak: AtomicU32,
    true_peak: AtomicU32,
    power: AtomicU64,
}

impl LevelCell {
    const fn new() -> Self {
        Self {
            peak: AtomicU32::new(0),
            true_peak: AtomicU32::new(0),
            power: AtomicU64::new(0),
        }
    }

    fn add(&self, peak: f32, true_peak: Option<f32>, energy: f32, frames: u32) {
        raise(&self.peak, peak);
        if let Some(true_peak) = true_peak {
            raise(&self.true_peak, true_peak);
        }
        // A compare-and-swap loop rather than `fetch_update`, which newer
        // toolchains deprecate in favour of a `try_update` the MSRV lacks.
        let mut old = self.power.load(Ordering::Relaxed);
        loop {
            let (old_energy, old_frames) = unpack(old);
            let new = pack(old_energy + energy, old_frames.saturating_add(frames));
            match self
                .power
                .compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
    }

    fn take(&self) -> Taken {
        let (energy, frames) = unpack(self.power.swap(0, Ordering::Relaxed));
        Taken {
            peak: f32::from_bits(self.peak.swap(0, Ordering::Relaxed)),
            true_peak: f32::from_bits(self.true_peak.swap(0, Ordering::Relaxed)),
            energy,
            frames,
        }
    }
}

/// Raise a stored peak to `value` if it is higher.
///
/// A plain load first: most blocks do not beat the peak already stored since
/// the last read, and a load costs a fraction of a read-modify-write. The
/// `fetch_max` after it keeps the result exact if the reader resets in between.
fn raise(cell: &AtomicU32, value: f32) {
    let bits = value.to_bits();
    if bits > cell.load(Ordering::Relaxed) {
        cell.fetch_max(bits, Ordering::Relaxed);
    }
}

fn pack(energy: f32, frames: u32) -> u64 {
    (u64::from(energy.to_bits()) << 32) | u64::from(frames)
}

fn unpack(word: u64) -> (f32, u32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "splitting a packed word in halves"
    )]
    let (high, low) = ((word >> 32) as u32, word as u32);
    (f32::from_bits(high), low)
}

/// The cells of every channel a meter can have.
#[derive(Debug)]
pub(crate) struct LevelCells {
    cells: [LevelCell; MAX_CHANNELS],
    /// Frames per channel of every block counted, meter-wide.
    frames: AtomicU64,
}

impl LevelCells {
    pub(crate) const fn new() -> Self {
        Self {
            cells: [const { LevelCell::new() }; MAX_CHANNELS],
            frames: AtomicU64::new(0),
        }
    }

    /// Add one block's measurements for `channel`. Out-of-range is ignored.
    ///
    /// `peak` and `true_peak` must be non-negative and not NaN.
    /// `true_peak` is `None` for a meter that does not measure it, which then
    /// costs no atomic at all.
    pub(crate) fn add(
        &self,
        channel: usize,
        peak: f32,
        true_peak: Option<f32>,
        energy: f32,
        frames: u32,
    ) {
        if let Some(cell) = self.cells.get(channel) {
            cell.add(peak, true_peak, energy, frames);
        }
    }

    /// Count one block's duration. Call once per block, after its channels.
    pub(crate) fn add_frames(&self, frames: u32) {
        self.frames.fetch_add(u64::from(frames), Ordering::Relaxed);
    }

    /// Take and reset the meter-wide duration.
    pub(crate) fn take_frames(&self) -> u64 {
        self.frames.swap(0, Ordering::Relaxed)
    }

    /// Take and reset what `channel` accumulated.
    pub(crate) fn take(&self, channel: usize) -> Taken {
        self.cells
            .get(channel)
            .map(LevelCell::take)
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::LevelCells;

    #[test]
    fn peaks_accumulate_as_maxima_and_reset_on_take() {
        let cells = LevelCells::new();
        cells.add(0, 0.25, Some(0.3), 1.0, 10);
        cells.add(0, 0.75, Some(0.7), 2.0, 10);
        cells.add(0, 0.5, Some(0.9), 0.5, 4);
        let taken = cells.take(0);
        assert_eq!((taken.peak, taken.true_peak), (0.75, 0.9));
        assert_eq!((taken.energy, taken.frames), (3.5, 24));
        assert_eq!(cells.take(0).frames, 0, "a take resets");
    }

    #[test]
    fn frame_count_saturates() {
        let cells = LevelCells::new();
        cells.add(1, 0.0, None, 0.0, u32::MAX);
        cells.add(1, 0.0, None, 0.0, 5);
        assert_eq!(cells.take(1).frames, u32::MAX);
    }

    #[test]
    fn duration_is_counted_once_per_block() {
        let cells = LevelCells::new();
        cells.add_frames(1024);
        cells.add_frames(1024);
        assert_eq!(cells.take_frames(), 2048);
        assert_eq!(cells.take_frames(), 0);
    }

    #[test]
    fn out_of_range_channels_are_ignored() {
        let cells = LevelCells::new();
        cells.add(99, 1.0, Some(1.0), 1.0, 1);
        assert_eq!(cells.take(99).frames, 0);
    }
}
