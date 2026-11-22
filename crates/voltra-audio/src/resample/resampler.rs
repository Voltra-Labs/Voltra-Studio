//! The streaming resampler: blocks in at one rate, blocks out at another.

use std::time::Duration;

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::bank::{FilterBank, LANES};
use crate::{Error, Result};

/// Converts a continuous stream of blocks from one sample rate to another.
///
/// Polyphase with an exact rational ratio: output sample `k` is the input
/// interpolated at exactly `k·from/to` input samples, tracked in integers, so a
/// stream of any length never drifts. The filter is flat to 20 kHz with images
/// and aliasing below −120 dB (`docs/references/audio-resampling.md` §3).
///
/// - **No allocation in [`process`](Self::process).** The working windows are
///   reserved by [`new`](Self::new) for the largest block it will be given, and
///   the output is the caller's block, whose capacity is checked first.
/// - **Output aligned with input in time.** The input before the first block is
///   taken as silence, so output sample 0 lands on input sample 0; the cost is
///   [`latency`](Self::latency) of look-ahead, about 1.5 ms.
/// - **Exact timestamps.** Each output block is stamped from its absolute
///   index, never by adding block durations.
/// - **Continuity is checked.** A block that does not start where the previous
///   one ended — a gap, a jump — restarts the stream at its own timestamp and
///   is counted in [`discontinuities`](Self::discontinuities).
///
/// # Examples
///
/// ```
/// use voltra_audio::Resampler;
/// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
///
/// let mut resampler =
///     Resampler::new(SampleRate::HZ_44100, SampleRate::HZ_48000, ChannelLayout::Stereo, 1024)?;
///
/// let input = AudioBuffer::new(SampleRate::HZ_44100, ChannelLayout::Stereo, 1024, Timestamp::ZERO)?;
/// let mut output = AudioBuffer::new(
///     SampleRate::HZ_48000,
///     ChannelLayout::Stereo,
///     resampler.max_output_frames(1024),
///     Timestamp::ZERO,
/// )?;
///
/// let produced = resampler.process(&input, &mut output)?;
/// assert_eq!(output.frames(), produced);
/// // 1024 samples at 44.1 kHz are 1114.6 at 48 kHz, less the look-ahead.
/// assert!(produced > 1000 && produced < 1115);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Resampler {
    from: SampleRate,
    to: SampleRate,
    layout: ChannelLayout,
    bank: FilterBank,
    max_input: usize,
    /// One window per channel, `window_len` apart: carried samples, then the
    /// new block. Copying the block in is the one copy, and unavoidable: the
    /// dot product needs the carried and the new samples contiguous.
    windows: Vec<f32>,
    window_len: usize,
    /// Valid samples at the front of each window.
    held: usize,
    /// Phase of the next output, `0..L`; its first tap is window index 0.
    phase: usize,
    /// Timestamp of input sample 0 of the current run, once there is one.
    start: Option<Timestamp>,
    consumed: u64,
    produced: u64,
    discontinuities: u64,
}

impl Resampler {
    /// A resampler from `from` to `to` for `layout`, taking blocks of up to
    /// `max_input_frames`. Allocates; build it off the audio thread.
    ///
    /// # Errors
    ///
    /// [`Error::SameRate`], [`Error::UnsupportedRatio`], and
    /// [`Error::EmptyBlock`] when `max_input_frames` is zero.
    pub fn new(
        from: SampleRate,
        to: SampleRate,
        layout: ChannelLayout,
        max_input_frames: usize,
    ) -> Result<Self> {
        if max_input_frames == 0 {
            return Err(Error::EmptyBlock);
        }
        let bank = FilterBank::new(from, to)?;
        let window_len = bank.taps() + max_input_frames;
        let mut resampler = Self {
            from,
            to,
            layout,
            max_input: max_input_frames,
            windows: vec![0.0; window_len * layout.channels()],
            window_len,
            held: 0,
            phase: 0,
            start: None,
            consumed: 0,
            produced: 0,
            discontinuities: 0,
            bank,
        };
        resampler.reset();
        Ok(resampler)
    }

    /// The input rate.
    pub const fn from(&self) -> SampleRate {
        self.from
    }

    /// The output rate.
    pub const fn to(&self) -> SampleRate {
        self.to
    }

    /// The most output samples a block of `input_frames` can produce: the
    /// capacity the output block of [`process`](Self::process) needs.
    pub const fn max_output_frames(&self, input_frames: usize) -> usize {
        (input_frames * self.bank.up()).div_ceil(self.bank.down()) + 1
    }

    /// How far the output lags the input: the filter's look-ahead.
    pub fn latency(&self) -> Duration {
        let ahead = self.bank.taps() - 1 - self.bank.center();
        self.from
            .duration_of(u64::try_from(ahead).unwrap_or(u64::MAX))
    }

    /// How many times the input jumped and the stream restarted.
    pub const fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    /// Forget the stream: the next block starts a new one at its own timestamp.
    pub fn reset(&mut self) {
        // The input before the start is silence: `center` zeros put output 0
        // exactly on input 0.
        self.windows.fill(0.0);
        self.held = self.bank.center();
        self.phase = 0;
        self.start = None;
        self.consumed = 0;
        self.produced = 0;
    }

    /// Resample one block into `output` and return how many samples it got.
    ///
    /// `output` must be at the output rate in the same layout, with capacity
    /// for [`max_output_frames`](Self::max_output_frames) of this block. It is
    /// reshaped to exactly the samples produced and stamped with the time of
    /// the first. When none are produced — a tiny first block still filling
    /// the look-ahead — it returns 0 and leaves `output` untouched.
    ///
    /// Never allocates, never blocks.
    ///
    /// # Errors
    ///
    /// [`Error::FormatMismatch`] for an input or output in the wrong rate or
    /// layout, [`Error::BlockTooLarge`] for more than `max_input_frames`, and
    /// [`Error::OutputTooSmall`]. On error nothing is consumed or written.
    pub fn process(&mut self, input: &AudioBuffer, output: &mut AudioBuffer) -> Result<usize> {
        self.check(input, self.from)?;
        self.check(output, self.to)?;
        let frames = input.frames();
        if frames > self.max_input {
            return Err(Error::BlockTooLarge {
                frames,
                max: self.max_input,
            });
        }
        let needed = self.max_output_frames(frames);
        if output.capacity_frames() < needed {
            return Err(Error::OutputTooSmall {
                needed,
                capacity: output.capacity_frames(),
            });
        }

        let start = self.continue_from(input.pts());

        // Append the block to every channel's window.
        let available = self.held + frames;
        for (window, samples) in self
            .windows
            .chunks_exact_mut(self.window_len)
            .zip(input.channels())
        {
            if let Some(tail) = window.get_mut(self.held..available) {
                tail.copy_from_slice(samples);
            }
        }

        // Count the outputs this window affords, and where the next one starts.
        let (count, next_base, next_phase) = self.step(available);
        self.consumed += frames as u64;
        if count == 0 {
            self.held = available;
            return Ok(0);
        }

        output
            .reshape(self.layout, count)
            .map_err(|_| Error::OutputTooSmall {
                needed: count,
                capacity: output.capacity_frames(),
            })?;
        output.set_pts(Timestamp::from_nanos(
            start
                .as_nanos()
                .saturating_add(self.to.pts(self.produced).as_nanos()),
        ));

        for (window, out) in self
            .windows
            .chunks_exact_mut(self.window_len)
            .zip(output.channels_mut())
        {
            interpolate(&self.bank, window, out, self.phase);
            // Carry what the next outputs still need to the front.
            window.copy_within(next_base..available, 0);
        }

        self.held = available - next_base;
        self.phase = next_phase;
        self.produced += count as u64;
        Ok(count)
    }

    fn check(&self, block: &AudioBuffer, rate: SampleRate) -> Result<()> {
        if block.rate() == rate && block.layout() == self.layout {
            Ok(())
        } else {
            Err(Error::FormatMismatch {
                expected_rate: rate,
                expected_layout: self.layout,
                found_rate: block.rate(),
                found_layout: block.layout(),
            })
        }
    }

    /// The start of the run this block continues, restarting if it does not.
    fn continue_from(&mut self, pts: Timestamp) -> Timestamp {
        if let Some(start) = self.start {
            let expected = start
                .as_nanos()
                .saturating_add(self.from.pts(self.consumed).as_nanos());
            // Half an input sample: timestamps are whole nanoseconds.
            let tolerance = 500_000_000 / u64::from(self.from.hz());
            if pts.as_nanos().abs_diff(expected) <= tolerance {
                return start;
            }
            self.reset();
            self.discontinuities += 1;
        }
        self.start = Some(pts);
        pts
    }

    /// Walk the exact position over a window of `available` samples: how many
    /// outputs fit, and the window index and phase of the next one. Integer
    /// arithmetic only, so it never drifts.
    fn step(&self, available: usize) -> (usize, usize, usize) {
        let (up, down, taps) = (self.bank.up(), self.bank.down(), self.bank.taps());
        let (mut count, mut base, mut phase) = (0, 0, self.phase);
        while base + taps <= available {
            count += 1;
            phase += down;
            base += phase / up;
            phase %= up;
        }
        (count, base, phase)
    }
}

/// Fill `out` from `window`, starting at window index 0 in `phase`.
fn interpolate(bank: &FilterBank, window: &[f32], out: &mut [f32], mut phase: usize) {
    let (up, down, taps) = (bank.up(), bank.down(), bank.taps());
    let mut base = 0;
    for sample in out {
        if let Some(span) = window.get(base..base + taps) {
            *sample = dot(bank.phase(phase), span);
        }
        phase += down;
        base += phase / up;
        phase %= up;
    }
}

/// A dot product in eight lanes; both sides are a multiple of eight long.
fn dot(coefficients: &[f32], samples: &[f32]) -> f32 {
    let mut lanes = [0.0f32; LANES];
    for (c, s) in coefficients
        .chunks_exact(LANES)
        .zip(samples.chunks_exact(LANES))
    {
        for ((lane, &a), &b) in lanes.iter_mut().zip(c).zip(s) {
            *lane += a * b;
        }
    }
    lanes.iter().sum()
}

#[cfg(test)]
mod tests;
