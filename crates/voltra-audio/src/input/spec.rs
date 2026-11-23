//! How an input is built, and the timeline it lives on.

use std::sync::Arc;
use std::time::Duration;

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::reader::InputReader;
use super::ring::Ring;
use super::stats::StatsCells;
use super::writer::InputWriter;
use crate::{Error, MixSpec, Resampler, Result};

/// The shape of one source's input.
///
/// # Examples
///
/// ```
/// use voltra_audio::{InputSpec, MixSpec, audio_input};
/// use voltra_core::{ChannelLayout, SampleRate};
///
/// let mix = MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 1024)?;
/// // A mono microphone at 44.1 kHz delivering up to 2048 samples at a time.
/// let spec = InputSpec::new(SampleRate::HZ_44100, ChannelLayout::Mono, 2048, mix);
/// let (writer, reader) = audio_input(spec)?;
/// // The writer moves to the source's thread, the reader to the mix's.
/// # drop((writer, reader));
/// # Ok::<(), voltra_audio::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSpec {
    /// The rate the source delivers at. Resampled to the mix's if different.
    pub source_rate: SampleRate,
    /// The source's layout, kept as is: the mixer pans or balances it.
    pub layout: ChannelLayout,
    /// The longest block the source will push.
    pub max_block_frames: usize,
    /// The mix this input feeds.
    pub mix: MixSpec,
    /// How far ahead of the mix the source may run before blocks are dropped.
    /// Rounded up to a power of two of samples.
    pub capacity: Duration,
    /// Timestamp differences below this are jitter, smoothed away; above it,
    /// a real gap or jump. Never less than half a source sample, whatever is
    /// set: below that a difference is rounding.
    pub smoothing: Duration,
}

impl InputSpec {
    /// OBS's smoothing threshold.
    pub const DEFAULT_SMOOTHING: Duration = Duration::from_millis(70);
    /// About as much as OBS's maximum buffering.
    pub const DEFAULT_CAPACITY: Duration = Duration::from_secs(1);

    /// A spec with the default capacity and smoothing.
    pub const fn new(
        source_rate: SampleRate,
        layout: ChannelLayout,
        max_block_frames: usize,
        mix: MixSpec,
    ) -> Self {
        Self {
            source_rate,
            layout,
            max_block_frames,
            mix,
            capacity: Self::DEFAULT_CAPACITY,
            smoothing: Self::DEFAULT_SMOOTHING,
        }
    }
}

/// Build an input: the writer for the source's thread, the reader for the
/// mix's. Allocates everything either will ever need.
///
/// # Errors
///
/// [`Error::EmptyBlock`] when `max_block_frames` or the mix's block is zero,
/// and whatever building the resampler reports when the rates differ.
pub fn audio_input(spec: InputSpec) -> Result<(InputWriter, InputReader)> {
    if spec.max_block_frames == 0 || spec.mix.frames == 0 {
        return Err(Error::EmptyBlock);
    }
    let resampling = if spec.source_rate == spec.mix.rate {
        None
    } else {
        let resampler = Resampler::new(
            spec.source_rate,
            spec.mix.rate,
            spec.layout,
            spec.max_block_frames,
        )?;
        let scratch = AudioBuffer::new(
            spec.mix.rate,
            spec.layout,
            resampler.max_output_frames(spec.max_block_frames),
            Timestamp::ZERO,
        )
        .map_err(|_| Error::EmptyBlock)?;
        Some((resampler, scratch))
    };
    let largest_block = resampling.as_ref().map_or(spec.max_block_frames, |(r, _)| {
        r.max_output_frames(spec.max_block_frames)
    });
    let wanted = usize::try_from(spec.mix.rate.frames_in(spec.capacity)).unwrap_or(usize::MAX);
    let frames = wanted.max(2 * (largest_block + spec.mix.frames));

    let ring = Arc::new(Ring::new(spec.layout.channels(), frames, 0));
    let stats = Arc::new(StatsCells::new());
    Ok((
        InputWriter::new(spec, Arc::clone(&ring), Arc::clone(&stats), resampling),
        InputReader::new(spec, ring, stats),
    ))
}

/// The sample of `rate` at which `time` falls, rounded to the nearest so that
/// `index_of(rate, rate.pts(i)) == i` despite `pts` flooring to nanoseconds.
pub(crate) fn index_of(rate: SampleRate, time: Timestamp) -> u64 {
    let scaled = u128::from(time.as_nanos()) * u128::from(rate.hz()) + 500_000_000;
    u64::try_from(scaled / 1_000_000_000).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use voltra_core::SampleRate;

    use super::index_of;

    #[test]
    fn indices_round_trip_through_timestamps() {
        for rate in [SampleRate::HZ_44100, SampleRate::HZ_48000] {
            for index in [
                0u64,
                1,
                2,
                1023,
                1024,
                48_000,
                1_234_567_891,
                999_999_999_999,
            ] {
                assert_eq!(index_of(rate, rate.pts(index)), index, "{rate} at {index}");
            }
        }
    }
}
