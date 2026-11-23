//! The source's half of an input: smooth, resample, place.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::ring::Ring;
use super::spec::{InputSpec, index_of};
use super::stats::{InputStats, StatsCells};
use crate::{Error, Resampler, Result};

/// The writing half of an input, for the source's thread.
///
/// [`push`](Self::push) never allocates and never waits for the mix.
#[derive(Debug)]
pub struct InputWriter {
    source_rate: SampleRate,
    layout: ChannelLayout,
    max_block: usize,
    smoothing_ns: u64,
    /// Where the current continuous run began, and how many source samples it
    /// has had since: the next block is expected at `start + rate.pts(count)`.
    run: Option<(Timestamp, u64)>,
    resampling: Option<(Resampler, AudioBuffer)>,
    placer: Placer,
}

/// Writes blocks into the ring at the index of their timestamp.
#[derive(Debug)]
struct Placer {
    ring: Arc<Ring>,
    stats: Arc<StatsCells>,
    mix_rate: SampleRate,
    /// One past the last index written, mirrored from the ring.
    written: u64,
    started: bool,
}

impl InputWriter {
    pub(crate) fn new(
        spec: InputSpec,
        ring: Arc<Ring>,
        stats: Arc<StatsCells>,
        resampling: Option<(Resampler, AudioBuffer)>,
    ) -> Self {
        Self {
            source_rate: spec.source_rate,
            layout: spec.layout,
            max_block: spec.max_block_frames,
            // Never below half a source sample: under that, a difference is
            // nanosecond rounding, not a gap.
            smoothing_ns: u64::try_from(spec.smoothing.as_nanos())
                .unwrap_or(u64::MAX)
                .max(500_000_000 / u64::from(spec.source_rate.hz())),
            run: None,
            resampling,
            placer: Placer {
                ring,
                stats,
                mix_rate: spec.mix.rate,
                written: 0,
                started: false,
            },
        }
    }

    /// Hand over one block from the source.
    ///
    /// Its timestamp is smoothed if it lands within the smoothing threshold of
    /// where the previous block ended; it is resampled to the mix rate if the
    /// source runs at another; and it is placed at the index of its time:
    /// after the previous one, after a gap of silence, trimmed where it
    /// overlaps what is written or where its moment has already been mixed —
    /// or dropped whole if the ring has no room. All of it is counted in
    /// [`stats`](Self::stats).
    ///
    /// # Errors
    ///
    /// [`Error::FormatMismatch`] for a block in the wrong rate or layout and
    /// [`Error::BlockTooLarge`] for one longer than the spec allows. Nothing is
    /// consumed when they occur.
    pub fn push(&mut self, block: &AudioBuffer) -> Result<()> {
        if block.rate() != self.source_rate || block.layout() != self.layout {
            return Err(Error::FormatMismatch {
                expected_rate: self.source_rate,
                expected_layout: self.layout,
                found_rate: block.rate(),
                found_layout: block.layout(),
            });
        }
        if block.frames() > self.max_block {
            return Err(Error::BlockTooLarge {
                frames: block.frames(),
                max: self.max_block,
            });
        }
        StatsCells::add(&self.placer.stats.blocks, 1);
        let pts = self.smooth(block);

        match &mut self.resampling {
            None => self.placer.place(block, pts),
            Some((resampler, scratch)) => {
                // The scratch block is sized for this; anything else is a bug
                // in this crate, and the block is dropped rather than panic.
                if matches!(resampler.process_at(block, pts, scratch), Ok(produced) if produced > 0)
                {
                    let at = scratch.pts();
                    self.placer.place(scratch, at);
                }
            }
        }
        Ok(())
    }

    /// What this input has been through.
    pub fn stats(&self) -> InputStats {
        self.placer.stats.snapshot()
    }

    /// The timestamp this block will be taken as: where the previous one ended,
    /// if it is within the smoothing threshold of it; its own otherwise.
    fn smooth(&mut self, block: &AudioBuffer) -> Timestamp {
        let frames = block.frames() as u64;
        let pts = block.pts();
        if let Some((start, count)) = &mut self.run {
            let expected = Timestamp::from_nanos(
                start
                    .as_nanos()
                    .saturating_add(self.source_rate.pts(*count).as_nanos()),
            );
            let jitter = pts.as_nanos().abs_diff(expected.as_nanos());
            if jitter <= self.smoothing_ns {
                self.placer
                    .stats
                    .max_jitter_ns
                    .fetch_max(jitter, Ordering::Relaxed);
                *count += frames;
                return expected;
            }
            StatsCells::add(&self.placer.stats.discontinuities, 1);
        }
        self.run = Some((pts, frames));
        pts
    }
}

impl Placer {
    /// Write `block` into the ring at the mix-rate index of `pts`.
    fn place(&mut self, block: &AudioBuffer, pts: Timestamp) {
        let stats = &self.stats;
        let frames = block.frames() as u64;
        let mut start = index_of(self.mix_rate, pts);
        if self.started && start.abs_diff(self.written) <= 1 {
            // Nanosecond rounding, not a gap: continue exactly.
            start = self.written;
        }
        let mut read = self.ring.read_index();
        let mut skip = 0u64;

        // What is already written stays: the mix may be reading it.
        if self.started && start < self.written {
            let overlap = frames.min(self.written - start);
            StatsCells::add(&stats.overlap_samples, overlap);
            skip += overlap;
        }
        // What the mix has already passed is late.
        if start + skip < read {
            let late = (frames - skip).min(read - (start + skip));
            StatsCells::add(&stats.late_samples, late);
            skip += late;
        }
        if skip >= frames {
            return;
        }
        let begin = start + skip;
        let end = start + frames;
        let capacity = self.ring.capacity() as u64;

        // Invariant: every index in `read .. written` holds a written sample,
        // so the mix never takes an unwritten slot for data.
        let empty = self.written <= read;
        if self.started && begin > self.written.max(read) {
            StatsCells::add(&stats.gaps, 1);
            StatsCells::add(&stats.gap_samples, begin - self.written.max(read));
        }
        if empty && begin > read {
            // Nothing unread: start the ring at this block. The mix will find
            // nothing before it, which is the truth.
            self.ring.advance_read(begin);
            read = begin;
        }
        if end - read > capacity {
            StatsCells::add(&stats.overflows, 1);
            return;
        }
        // A real gap inside live data: silence in its place, so later samples
        // land exactly where their time says.
        let fill_from = if empty { begin } else { self.written };
        let offset = usize::try_from(skip).unwrap_or(usize::MAX);
        for (channel, samples) in block.channels().enumerate() {
            if begin > fill_from {
                let gap = usize::try_from(begin - fill_from).unwrap_or(0);
                self.ring.silence(channel, fill_from, gap);
            }
            if let Some(rest) = samples.get(offset..) {
                self.ring.write(channel, begin, rest);
            }
        }
        self.written = end;
        self.started = true;
        self.ring.publish(end);
    }
}
