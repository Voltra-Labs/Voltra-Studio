//! A block of audio in system memory.
//!
//! One allocation holds every channel, contiguously, exactly as
//! [`VideoFrame`](crate::VideoFrame) holds every plane. A buffer is one
//! `malloc`, not eight.
//!
//! Unlike video there is **no stride and no padding between channels**. No audio
//! API hands over channel planes aligned to 32 bytes, an `f32` slice is already
//! aligned to 4, and inventing padding here would copy a video complication that
//! pays for nothing.

use crate::audio::format::{ChannelLayout, MAX_CHANNELS, SampleRate};
use crate::time::Timestamp;
use crate::{Error, Result};

/// A block of 32-bit float samples, one contiguous run per channel.
///
/// # Examples
///
/// ```
/// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
///
/// let mut block = AudioBuffer::new(
///     SampleRate::HZ_48000,
///     ChannelLayout::Stereo,
///     1024,
///     Timestamp::ZERO,
/// )?;
///
/// assert_eq!(block.channel_count(), 2);
/// assert_eq!(block.frames(), 1024);
/// // A fresh buffer is silence, not whatever was in memory.
/// assert!(block.channel(0).is_some_and(|left| left.iter().all(|&s| s == 0.0)));
///
/// // One block at 48 kHz is 21.33 ms, and the next one starts exactly there.
/// assert_eq!(block.end_pts(), SampleRate::HZ_48000.pts(1024));
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    rate: SampleRate,
    layout: ChannelLayout,
    /// Samples per channel, not samples in total.
    frames: usize,
    pts: Timestamp,
    /// Channel `c` occupies `[c * frames .. (c + 1) * frames]`.
    data: Vec<f32>,
}

impl AudioBuffer {
    /// Allocate a silent buffer.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `frames` is zero — an empty block has no meaning
    /// and every consumer would have to special-case it — or when the total
    /// sample count overflows.
    pub fn new(
        rate: SampleRate,
        layout: ChannelLayout,
        frames: usize,
        pts: Timestamp,
    ) -> Result<Self> {
        if frames == 0 {
            return Err(Error::config("an audio buffer needs at least one frame"));
        }
        let channels = layout.channels();
        debug_assert!(channels <= MAX_CHANNELS, "layout wider than MAX_CHANNELS");

        let samples = frames
            .checked_mul(channels)
            .ok_or_else(|| Error::config(format!("{frames} frames of {layout} is too large")))?;

        Ok(Self {
            rate,
            layout,
            frames,
            pts,
            data: vec![0.0; samples],
        })
    }

    /// The sample rate.
    pub const fn rate(&self) -> SampleRate {
        self.rate
    }

    /// The channel layout.
    pub const fn layout(&self) -> ChannelLayout {
        self.layout
    }

    /// How many channels there are.
    pub const fn channel_count(&self) -> usize {
        self.layout.channels()
    }

    /// Samples per channel.
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// The timestamp of the first sample.
    pub const fn pts(&self) -> Timestamp {
        self.pts
    }

    /// Set the timestamp of the first sample.
    pub const fn set_pts(&mut self, pts: Timestamp) {
        self.pts = pts;
    }

    /// Where the next block starts, if it follows this one exactly.
    ///
    /// Derived from the rate rather than by adding a rounded block duration:
    /// summing 21.333 ms a thousand times is not the same number as dividing
    /// once, and that difference is what A/V drift is made of.
    pub fn end_pts(&self) -> Timestamp {
        let elapsed = u64::try_from(self.duration().as_nanos()).unwrap_or(u64::MAX);
        Timestamp::from_nanos(self.pts.as_nanos().saturating_add(elapsed))
    }

    /// How long this block lasts.
    pub fn duration(&self) -> std::time::Duration {
        self.rate
            .duration_of(u64::try_from(self.frames).unwrap_or(u64::MAX))
    }

    /// One channel's samples.
    pub fn channel(&self, index: usize) -> Option<&[f32]> {
        if index >= self.channel_count() {
            return None;
        }
        let start = index * self.frames;
        self.data.get(start..start + self.frames)
    }

    /// One channel's samples, mutably.
    pub fn channel_mut(&mut self, index: usize) -> Option<&mut [f32]> {
        if index >= self.channel_count() {
            return None;
        }
        let start = index * self.frames;
        self.data.get_mut(start..start + self.frames)
    }

    /// Every channel, in order.
    pub fn channels(&self) -> impl Iterator<Item = &[f32]> {
        self.data.chunks_exact(self.frames)
    }

    /// Every channel, in order, mutably.
    ///
    /// The way to touch several channels at once — a mixer, a pan — without
    /// fighting the borrow checker one `channel_mut` at a time.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        self.data.chunks_exact_mut(self.frames)
    }

    /// Every sample, channel by channel.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Every sample, channel by channel, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Zero every sample, keeping the allocation.
    ///
    /// What a mixer does at the start of a block. Reallocating instead would put
    /// a `malloc` in the audio callback, which CLAUDE.md §4.3 forbids outright.
    pub fn silence(&mut self) {
        self.data.fill(0.0);
    }

    /// Reshape in place, reallocating only when the sample count actually grows.
    ///
    /// The buffer is left silent. This is what lets a source that changes layout
    /// mid-session avoid allocating on the audio thread more than once.
    ///
    /// # Errors
    ///
    /// As [`AudioBuffer::new`].
    pub fn reshape(&mut self, layout: ChannelLayout, frames: usize) -> Result<()> {
        if frames == 0 {
            return Err(Error::config("an audio buffer needs at least one frame"));
        }
        let samples = frames
            .checked_mul(layout.channels())
            .ok_or_else(|| Error::config(format!("{frames} frames of {layout} is too large")))?;

        self.layout = layout;
        self.frames = frames;
        if self.data.len() < samples {
            self.data.resize(samples, 0.0);
        } else {
            self.data.truncate(samples);
        }
        self.silence();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AudioBuffer;
    use crate::audio::format::{ChannelLayout, SampleRate};
    use crate::time::Timestamp;

    fn block(layout: ChannelLayout, frames: usize) -> AudioBuffer {
        AudioBuffer::new(SampleRate::HZ_48000, layout, frames, Timestamp::ZERO).unwrap()
    }

    #[test]
    fn a_buffer_allocates_exactly_frames_times_channels() {
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::FivePoint1,
            ChannelLayout::SevenPoint1,
        ] {
            let buffer = block(layout, 1024);
            assert_eq!(
                buffer.as_slice().len(),
                1024 * layout.channels(),
                "{layout}"
            );
            assert_eq!(buffer.channels().count(), layout.channels());
        }
    }

    #[test]
    fn a_fresh_buffer_is_silent() {
        let buffer = block(ChannelLayout::Stereo, 256);
        assert!(buffer.as_slice().iter().all(|&sample| sample == 0.0));
    }

    /// Channels must not overlap: writing one has to leave the others alone.
    #[test]
    fn channels_are_independent_runs() {
        // A distinct, exactly representable marker per channel, so an overlap
        // between channels shows up as the wrong number rather than as noise.
        let marker = |index: usize| f32::from(u8::try_from(index).unwrap());

        let mut buffer = block(ChannelLayout::FivePoint1, 64);
        for (index, channel) in buffer.channels_mut().enumerate() {
            channel.fill(marker(index));
        }
        for index in 0..6 {
            let channel = buffer.channel(index).unwrap();
            assert_eq!(channel.len(), 64);
            assert!(
                channel
                    .iter()
                    .all(|sample| (sample - marker(index)).abs() < f32::EPSILON),
                "channel {index}"
            );
        }
    }

    #[test]
    fn an_out_of_range_channel_is_none() {
        let mut buffer = block(ChannelLayout::Stereo, 16);
        assert!(buffer.channel(1).is_some());
        assert!(buffer.channel(2).is_none());
        assert!(buffer.channel_mut(2).is_none());
    }

    #[test]
    fn an_empty_buffer_is_refused() {
        assert!(
            AudioBuffer::new(
                SampleRate::HZ_48000,
                ChannelLayout::Stereo,
                0,
                Timestamp::ZERO
            )
            .is_err()
        );
    }

    /// A block's end is the next block's start, computed from the rate. Adding
    /// rounded durations would drift; this is the test that says so.
    #[test]
    fn the_end_of_a_block_is_where_the_next_one_starts() {
        let buffer = block(ChannelLayout::Stereo, 1024);
        assert_eq!(buffer.end_pts(), SampleRate::HZ_48000.pts(1024));
    }

    #[test]
    fn silencing_keeps_the_allocation() {
        let mut buffer = block(ChannelLayout::Stereo, 128);
        buffer.as_mut_slice().fill(0.5);
        let capacity = buffer.as_slice().len();
        buffer.silence();
        assert_eq!(buffer.as_slice().len(), capacity);
        assert!(buffer.as_slice().iter().all(|&s| s == 0.0));
    }

    /// Growing reallocates once; shrinking must not, because the audio thread
    /// cannot afford a `malloc` (CLAUDE.md §4.3).
    #[test]
    fn reshaping_reuses_the_allocation_when_it_can() {
        let mut buffer = block(ChannelLayout::SevenPoint1, 1024);
        let before = buffer.as_slice().as_ptr();

        buffer.reshape(ChannelLayout::Stereo, 512).unwrap();
        assert_eq!(buffer.channel_count(), 2);
        assert_eq!(buffer.frames(), 512);
        assert_eq!(buffer.as_slice().len(), 1024);
        assert_eq!(buffer.as_slice().as_ptr(), before, "shrinking reallocated");

        assert!(buffer.reshape(ChannelLayout::Stereo, 0).is_err());
    }
}
