//! The capture-to-render edge: a triple buffer of frames, the latest wins.
//!
//! Three [`VideoFrame`]s: the producer writes into one, the consumer reads
//! another, and the third is the one most recently published, swapped with
//! either side by a single atomic exchange. The implementation is the
//! `triple_buffer` crate (ADR 0006); this module adds the frames' shape and the
//! counting.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use triple_buffer::{Input, Output, TripleBuffer};
use voltra_core::{FrameSize, PixelFormat, Result, Timestamp, VideoFrame};

/// A frame channel for frames of `format` and `size`: the writing end for the
/// capture thread, the reading end for the render thread.
///
/// The three frames are allocated here, once.
///
/// # Errors
///
/// When a frame of that format and size cannot be allocated.
///
/// # Examples
///
/// ```
/// use voltra_capture::frame_channel;
/// use voltra_core::{FrameSize, PixelFormat, Timestamp};
///
/// let size = FrameSize::new(64, 36)?;
/// let (mut writer, mut reader) = frame_channel(PixelFormat::Bgra8, size)?;
///
/// // The capture thread writes in place and publishes.
/// writer.frame_mut(PixelFormat::Bgra8, size)?.as_bytes_mut().fill(0xFF);
/// writer.publish(Timestamp::from_millis(16));
///
/// // The render thread takes the latest.
/// assert!(reader.update());
/// assert_eq!(reader.frame().unwrap().pts(), Timestamp::from_millis(16));
/// # Ok::<(), voltra_core::Error>(())
/// ```
pub fn frame_channel(format: PixelFormat, size: FrameSize) -> Result<(FrameWriter, FrameReader)> {
    let initial = VideoFrame::new(format, size, Timestamp::ZERO)?;
    let (input, output) = TripleBuffer::new(&initial).split();
    let counters = Arc::new(Counters::default());
    Ok((
        FrameWriter {
            input,
            counters: Arc::clone(&counters),
        },
        FrameReader {
            output,
            counters,
            has_frame: false,
        },
    ))
}

/// The capture thread's end of a frame channel.
#[derive(Debug)]
pub struct FrameWriter {
    input: Input<VideoFrame>,
    counters: Arc<Counters>,
}

impl FrameWriter {
    /// The frame to write the next capture into, of `format` and `size`.
    ///
    /// Its contents are whatever a previous frame left: write all of it. If it
    /// is not of `format` and `size`, it is allocated again first — the only
    /// allocation on this path, once per buffer after a change of size, so a
    /// change costs three.
    ///
    /// # Errors
    ///
    /// When a frame of the new format and size cannot be allocated.
    pub fn frame_mut(&mut self, format: PixelFormat, size: FrameSize) -> Result<&mut VideoFrame> {
        let frame = self.input.input_buffer_mut();
        if frame.format() != format || frame.size() != size {
            *frame = VideoFrame::new(format, size, frame.pts())?;
            self.counters.reallocated.fetch_add(1, Ordering::Relaxed);
        }
        Ok(frame)
    }

    /// Stamp the frame written through [`frame_mut`](Self::frame_mut) with
    /// `pts` and hand it over. If the previous one had not been read, it is
    /// gone, and counted as dropped.
    pub fn publish(&mut self, pts: Timestamp) {
        self.input.input_buffer_mut().set_pts(pts);
        // Counted before the frame becomes visible, so a reader that has taken
        // it already sees it counted as captured.
        self.counters.captured.fetch_add(1, Ordering::Relaxed);
        if self.input.publish() {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// What this channel has carried so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.counters.snapshot()
    }
}

/// The render thread's end of a frame channel.
#[derive(Debug)]
pub struct FrameReader {
    output: Output<VideoFrame>,
    counters: Arc<Counters>,
    has_frame: bool,
}

impl FrameReader {
    /// Take the latest published frame, if there is one newer than the frame
    /// held: `true` if so. Once a frame has been shown, a call with nothing new
    /// counts as a repeat; before the first one, it counts as nothing.
    pub fn update(&mut self) -> bool {
        if self.output.update() {
            self.has_frame = true;
            self.counters.shown.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            if self.has_frame {
                self.counters.repeated.fetch_add(1, Ordering::Relaxed);
            }
            false
        }
    }

    /// The frame held: the latest taken by [`update`](Self::update). `None`
    /// until the first.
    #[must_use]
    pub fn frame(&self) -> Option<&VideoFrame> {
        self.has_frame.then(|| self.output.peek_output_buffer())
    }

    /// What this channel has carried so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.counters.snapshot()
    }
}

/// What a frame channel has carried.
///
/// Read while both ends are busy, the counts are each exact but not taken at
/// the same instant; once they are quiet, `captured == shown + dropped +
/// pending` with [`pending`](Self::pending) at most 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureStats {
    /// Frames published by the capture thread.
    pub captured: u64,
    /// Frames overwritten before the render thread read them.
    pub dropped: u64,
    /// Frames the render thread took.
    pub shown: u64,
    /// Render ticks, after the first frame, that found nothing new.
    pub repeated: u64,
    /// Frame buffers allocated again because the format or size changed.
    pub reallocated: u64,
}

impl CaptureStats {
    /// Frames captured and neither shown nor dropped: the one published and
    /// not read yet, if any.
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.captured
            .saturating_sub(self.shown)
            .saturating_sub(self.dropped)
    }
}

/// Tallies shared by both ends. Relaxed: the frames themselves are published
/// by the triple buffer's own acquire/release exchange.
#[derive(Debug, Default)]
struct Counters {
    captured: AtomicU64,
    dropped: AtomicU64,
    shown: AtomicU64,
    repeated: AtomicU64,
    reallocated: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> CaptureStats {
        CaptureStats {
            captured: self.captured.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            repeated: self.repeated.load(Ordering::Relaxed),
            reallocated: self.reallocated.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests;
