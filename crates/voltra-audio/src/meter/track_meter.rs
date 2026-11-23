//! The two meters every mixer track carries: before and after its fader.

use std::sync::Arc;

use voltra_core::ChannelLayout;

use super::cell::LevelCells;
use super::levels::Levels;

/// The accumulators of one track, written by the mixer.
#[derive(Debug)]
pub(crate) struct TrackCells {
    input: LevelCells,
    output: LevelCells,
}

impl TrackCells {
    pub(crate) const fn new() -> Self {
        Self {
            input: LevelCells::new(),
            output: LevelCells::new(),
        }
    }

    /// One channel of the track's block, before gain.
    pub(crate) fn input(&self, channel: usize, peak: f32, energy: f32, frames: u32) {
        self.input.add(channel, peak, None, energy, frames);
    }

    /// One channel of the mix, as this track contributes to it.
    pub(crate) fn output(&self, channel: usize, peak: f32, energy: f32, frames: u32) {
        self.output.add(channel, peak, None, energy, frames);
    }

    /// Close a block: count its duration on both meters.
    pub(crate) fn close(&self, frames: u32) {
        self.input.add_frames(frames);
        self.output.add_frames(frames);
    }

    /// A block in which the track did not sound: silence on every channel, so
    /// its meters fall instead of freezing at the last level.
    pub(crate) fn silent(&self, input_channels: usize, output_channels: usize, frames: u32) {
        for channel in 0..input_channels {
            self.input(channel, 0.0, 0.0, frames);
        }
        for channel in 0..output_channels {
            self.output(channel, 0.0, 0.0, frames);
        }
        self.close(frames);
    }
}

/// What a track's meters read since the previous take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackLevels {
    /// The track's own channels, before gain, balance and mute: what the
    /// source delivers, for setting its input level.
    pub input: Levels,
    /// The mix's channels, after gain, balance or pan and mute: what this track
    /// puts into the mix.
    ///
    /// Derived from the input and the per-channel gains rather than measured,
    /// because tracks sum into one buffer. Exact while the gain holds; during
    /// the one-block ramp of a change it uses the larger gain, so it never
    /// under-reads.
    pub output: Levels,
}

/// The reading half of a track's meters. One per track, from
/// [`Mixer::take_track_meter`](crate::Mixer::take_track_meter): `Send`, not
/// `Clone`, because two readers would each take half the data.
#[derive(Debug)]
pub struct TrackMeter {
    cells: Arc<TrackCells>,
    input_channels: usize,
    output_channels: usize,
}

impl TrackMeter {
    pub(crate) fn new(cells: Arc<TrackCells>, track: ChannelLayout, mix: ChannelLayout) -> Self {
        Self {
            cells,
            input_channels: track.channels(),
            output_channels: mix.channels(),
        }
    }

    /// Everything measured since the previous call, and reset.
    pub fn take(&mut self) -> TrackLevels {
        TrackLevels {
            input: Levels::take(&self.cells.input, self.input_channels, false),
            output: Levels::take(&self.cells.output, self.output_channels, false),
        }
    }
}
