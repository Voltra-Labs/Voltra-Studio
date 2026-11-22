//! The mixer: tracks in, one block out.
//!
//! Structure changes — adding and removing tracks — take `&mut Mixer`, so they
//! happen on the thread that owns it and allocate there, never inside
//! [`Mixer::mix`]. Parameter changes come from any thread through
//! [`TrackControl`]. See the crate documentation for the invariants.

use std::sync::Arc;

use voltra_core::{AudioBuffer, ChannelLayout, MAX_CHANNELS, SampleRate, Timestamp};

use crate::gain::{ChannelGains, Routing};
use crate::kernel::{accumulate, accumulate_ramp, all_finite};
use crate::track::{TrackControl, TrackId, TrackParams};
use crate::{Error, Result};

/// The format of a mix: what every block in and out has to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixSpec {
    /// Sample rate of every track and of the output.
    pub rate: SampleRate,
    /// Layout of the output.
    pub layout: ChannelLayout,
    /// Samples per channel per block. libobs uses 1024.
    pub frames: usize,
}

impl MixSpec {
    /// A spec, rejecting an empty block.
    ///
    /// # Errors
    ///
    /// [`Error::EmptyBlock`] when `frames` is zero.
    pub const fn new(rate: SampleRate, layout: ChannelLayout, frames: usize) -> Result<Self> {
        if frames == 0 {
            return Err(Error::EmptyBlock);
        }
        Ok(Self {
            rate,
            layout,
            frames,
        })
    }

    /// Allocate a silent output block in this format.
    ///
    /// # Errors
    ///
    /// Whatever [`AudioBuffer::new`] reports, such as a size that overflows.
    pub fn buffer(&self, pts: Timestamp) -> voltra_core::Result<AudioBuffer> {
        AudioBuffer::new(self.rate, self.layout, self.frames, pts)
    }

    fn of(buffer: &AudioBuffer) -> Self {
        Self {
            rate: buffer.rate(),
            layout: buffer.layout(),
            frames: buffer.frames(),
        }
    }
}

/// What happened to the tracks in one block.
///
/// Returned by value and allocation-free, so the audio thread can hand it to
/// its metrics without formatting anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MixReport {
    /// Tracks that contributed to the block.
    pub mixed: u32,
    /// Tracks that delivered no block. They fade back in when they return.
    pub missing: u32,
    /// Inputs dropped for a wrong rate, length, layout or timestamp, an unknown
    /// track, or a track given twice.
    pub rejected: u32,
    /// Inputs dropped because a sample was NaN or infinite.
    pub non_finite: u32,
}

/// [`MixReport`] accumulated over the mixer's life: the always-on counters of
/// CLAUDE.md §4.10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MixTotals {
    /// Blocks mixed.
    pub blocks: u64,
    /// Track contributions mixed.
    pub mixed: u64,
    /// Track blocks that never arrived.
    pub missing: u64,
    /// Inputs rejected.
    pub rejected: u64,
    /// Inputs dropped for non-finite samples.
    pub non_finite: u64,
}

impl MixTotals {
    fn add(&mut self, report: MixReport) {
        self.blocks += 1;
        self.mixed += u64::from(report.mixed);
        self.missing += u64::from(report.missing);
        self.rejected += u64::from(report.rejected);
        self.non_finite += u64::from(report.non_finite);
    }
}

/// The mixer's private view of one track.
#[derive(Debug)]
struct Track {
    id: TrackId,
    layout: ChannelLayout,
    routing: Routing,
    params: Arc<TrackParams>,
    /// Per-channel gains reached at the end of the last block. Zero for a new
    /// track or one that did not sound, so it always enters with a fade.
    applied: ChannelGains,
    /// The block number this track last delivered in, to catch duplicates.
    seen: u64,
}

/// Sums tracks into one output block with gain, balance and mute.
///
/// # Examples
///
/// Two stereo tracks at unity sum sample by sample, and the sum is not clipped:
///
/// ```
/// use voltra_audio::{MixSpec, Mixer};
/// use voltra_core::{ChannelLayout, SampleRate, Timestamp};
///
/// let spec = MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 4)?;
/// let mut mixer = Mixer::new(spec)?;
/// let a = mixer.add_track(ChannelLayout::Stereo)?;
/// let b = mixer.add_track(ChannelLayout::Stereo)?;
///
/// let mut loud = spec.buffer(Timestamp::ZERO)?;
/// loud.as_mut_slice().fill(0.75);
/// let mut out = spec.buffer(Timestamp::ZERO)?;
///
/// // The first block fades each new track in; the second is steady.
/// mixer.mix([(a.id(), &loud), (b.id(), &loud)], &mut out)?;
/// mixer.mix([(a.id(), &loud), (b.id(), &loud)], &mut out)?;
/// assert!(out.as_slice().iter().all(|&sample| sample == 1.5));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Mixer {
    spec: MixSpec,
    /// Sorted by id; ids only grow, so pushing keeps the order.
    tracks: Vec<Track>,
    /// `(k + 1) / frames`, the shape of every gain transition.
    ramp: Vec<f32>,
    next_id: u32,
    block: u64,
    totals: MixTotals,
}

impl Mixer {
    /// A mixer with no tracks.
    ///
    /// # Errors
    ///
    /// [`Error::EmptyBlock`] when `spec.frames` is zero. [`MixSpec::new`]
    /// already refuses that, but its fields are public and can be set directly.
    pub fn new(spec: MixSpec) -> Result<Self> {
        if spec.frames == 0 {
            return Err(Error::EmptyBlock);
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "block lengths are far below 2^24, where f32 stays exact"
        )]
        let ramp = {
            let frames = spec.frames as f32;
            (1..=spec.frames).map(|k| k as f32 / frames).collect()
        };
        Ok(Self {
            spec,
            tracks: Vec::new(),
            ramp,
            next_id: 0,
            block: 0,
            totals: MixTotals::default(),
        })
    }

    /// The format this mixer produces.
    pub const fn spec(&self) -> MixSpec {
        self.spec
    }

    /// How many tracks there are.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Counters accumulated since the mixer was built.
    pub const fn totals(&self) -> MixTotals {
        self.totals
    }

    /// Add a track and return its control.
    ///
    /// The track starts at unity, centred and unmuted, and its first block
    /// fades in. Allocates: call it from the owning thread, not the callback.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedLayout`] unless the track has the mix's layout or is
    /// mono into a stereo mix. [`Error::TooManyTracks`] after `u32::MAX` tracks.
    pub fn add_track(&mut self, layout: ChannelLayout) -> Result<TrackControl> {
        let routing =
            Routing::between(layout, self.spec.layout).ok_or(Error::UnsupportedLayout {
                track: layout,
                mix: self.spec.layout,
            })?;
        let id = TrackId::new(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(Error::TooManyTracks)?;

        let (control, params) = TrackControl::new(id);
        self.tracks.push(Track {
            id,
            layout,
            routing,
            params,
            applied: [0.0; MAX_CHANNELS],
            seen: 0,
        });
        Ok(control)
    }

    /// Remove a track. Returns whether it existed.
    ///
    /// The track stops at once, without a fade. Its [`TrackControl`] keeps
    /// working but no longer affects anything.
    pub fn remove_track(&mut self, id: TrackId) -> bool {
        match self.tracks.binary_search_by_key(&id, |track| track.id) {
            Ok(index) => {
                self.tracks.remove(index);
                true
            }
            Err(_) => false,
        }
    }

    /// Mix one block.
    ///
    /// `inputs` pairs each track with its block for this tick; an array on the
    /// stack is enough, so the caller needs no `Vec`. Every input must match the
    /// mix's rate and length, its track's layout, and `out`'s timestamp exactly.
    /// An input that does not is dropped and counted, and the rest still mix.
    ///
    /// `out` is overwritten, not added to. It is not clipped.
    ///
    /// Never allocates, never blocks, never logs.
    ///
    /// # Errors
    ///
    /// [`Error::OutputMismatch`] when `out` is not in this mixer's format. That
    /// is a bug in the caller, not a track failure, and `out` is left untouched.
    pub fn mix<'a, I>(&mut self, inputs: I, out: &mut AudioBuffer) -> Result<MixReport>
    where
        I: IntoIterator<Item = (TrackId, &'a AudioBuffer)>,
    {
        let found = MixSpec::of(out);
        if found != self.spec {
            return Err(Error::OutputMismatch {
                expected: self.spec,
                found,
            });
        }

        self.block += 1;
        out.silence();
        let mut report = MixReport::default();

        for (id, input) in inputs {
            let Ok(index) = self.tracks.binary_search_by_key(&id, |track| track.id) else {
                report.rejected += 1;
                continue;
            };
            let track = &mut self.tracks[index];
            if track.seen == self.block {
                // Same track twice in one block: keep the first, drop this one.
                report.rejected += 1;
                continue;
            }
            track.seen = self.block;

            if !fits(input, track.layout, self.spec, out.pts()) {
                report.rejected += 1;
                track.fail();
                continue;
            }
            if !all_finite(input.as_slice()) {
                report.non_finite += 1;
                track.fail();
                continue;
            }

            track.sum_into(input, out, &self.ramp);
            report.mixed += 1;
        }

        for track in &mut self.tracks {
            if track.seen != self.block {
                report.missing += 1;
                track.applied = [0.0; MAX_CHANNELS];
            }
        }

        self.totals.add(report);
        Ok(report)
    }
}

/// Whether an input block can be mixed as it is.
fn fits(input: &AudioBuffer, layout: ChannelLayout, spec: MixSpec, pts: Timestamp) -> bool {
    input.rate() == spec.rate
        && input.frames() == spec.frames
        && input.layout() == layout
        && input.pts() == pts
}

impl Track {
    /// Drop this block: count it against the track and fade in next time.
    fn fail(&mut self) {
        self.params.fault();
        self.applied = [0.0; MAX_CHANNELS];
    }

    /// Add this track's block into `out` at its current gains.
    fn sum_into(&mut self, input: &AudioBuffer, out: &mut AudioBuffer, ramp: &[f32]) {
        let target = self.routing.channel_gains(
            out.channel_count(),
            self.params.effective_gain(),
            self.params.balance(),
        );
        for ((channel, sum), (&from, &to)) in out
            .channels_mut()
            .enumerate()
            .zip(self.applied.iter().zip(&target))
        {
            let Some(source) = input.channel(self.routing.source_channel(channel)) else {
                continue;
            };
            #[allow(
                clippy::float_cmp,
                reason = "exact on purpose: equal means no change since last block"
            )]
            let steady = from == to;
            if steady {
                if to != 0.0 {
                    accumulate(sum, source, to);
                }
            } else {
                accumulate_ramp(sum, source, from, to, ramp);
            }
        }
        self.applied = target;
    }
}

#[cfg(test)]
mod tests;
