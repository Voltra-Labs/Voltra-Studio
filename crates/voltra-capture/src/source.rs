//! A capture as the scene sees it: a [`VideoSource`] like any other, fed by a
//! backend on a thread of its own.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

use voltra_core::{
    FrameSize, Source, SourceCapabilities, SourceType, TickContext, VideoFrame, VideoSource,
};

use crate::delivery::{CaptureStats, FrameReader};

/// Where a capture backend is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureStatus {
    /// Started, with no frame published yet.
    Starting,
    /// Publishing frames.
    Running,
    /// Stopped on request.
    Stopped,
    /// Stopped by an error, which the backend logged. The source keeps showing
    /// its last frame; nothing else in the pipeline notices (CLAUDE.md §5).
    Failed,
}

/// A capture backend: whatever owns the thread that publishes frames into a
/// [`FrameWriter`](crate::FrameWriter).
///
/// The platform-specific part lives behind this, and it is small on purpose:
/// on every platform, frames arrive on a system thread from a small pool of
/// buffers that must be handed back soon (`docs/references/capture.md` §5), so
/// what differs is how the backend gets them, not how the scene receives them.
pub trait Capture: Send + fmt::Debug {
    /// The stable type identifier of this capture, as [`Source::kind`].
    fn kind(&self) -> &'static str;

    /// Where the backend is.
    fn status(&self) -> CaptureStatus;

    /// What the backend lost before a frame reached the frame channel. A
    /// backend that cannot lose anything there keeps the default, all zero.
    fn backend_stats(&self) -> BackendStats {
        BackendStats::default()
    }

    /// Stop capturing and wait for the backend's thread to finish. Calling it
    /// again does nothing.
    fn stop(&mut self);
}

/// What a backend counts before a frame reaches the frame channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendStats {
    /// Frames the system delivered while a newer one was already waiting, and
    /// that were handed back unused: drained to the newest, as OBS does with
    /// PipeWire, and counted.
    pub source_dropped: u64,
    /// Buffers skipped because they were flagged corrupted, were empty, or
    /// could not be read.
    pub skipped: u64,
    /// How many times the capture's clock was mapped onto the process's: once,
    /// plus once per jump of more than 2 s.
    pub clock_anchors: u64,
}

/// A capture as a scene source: on each tick it takes the latest frame the
/// backend published, if there is one, and shows it until the next.
#[derive(Debug)]
pub struct CaptureSource {
    reader: FrameReader,
    backend: Box<dyn Capture>,
}

impl CaptureSource {
    /// A source that shows what `backend` publishes into the other end of
    /// `reader`'s channel.
    #[must_use]
    pub fn new(reader: FrameReader, backend: Box<dyn Capture>) -> Self {
        Self { reader, backend }
    }

    /// What the capture has delivered so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.reader.stats()
    }

    /// What the backend lost before the frame channel.
    #[must_use]
    pub fn backend_stats(&self) -> BackendStats {
        self.backend.backend_stats()
    }

    /// Where the backend is.
    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        self.backend.status()
    }

    /// Stop the backend and wait for it. The last frame stays.
    pub fn stop(&mut self) {
        self.backend.stop();
    }
}

impl Source for CaptureSource {
    fn kind(&self) -> &'static str {
        self.backend.kind()
    }

    fn source_type(&self) -> SourceType {
        SourceType::Input
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::VIDEO
    }

    /// Take the latest frame. Never waits for the capture thread, and never
    /// fails: a backend that failed leaves its last frame on show.
    fn tick(&mut self, _context: &TickContext) -> voltra_core::Result<()> {
        self.reader.update();
        Ok(())
    }
}

impl VideoSource for CaptureSource {
    fn size(&self) -> Option<FrameSize> {
        self.reader.frame().map(VideoFrame::size)
    }

    fn frame(&self) -> Option<&VideoFrame> {
        self.reader.frame()
    }
}

/// A [`CaptureStatus`] shared between a backend's thread and whoever asks.
#[derive(Debug)]
pub(crate) struct StatusCell(AtomicU8);

impl StatusCell {
    pub(crate) const fn new(status: CaptureStatus) -> Self {
        Self(AtomicU8::new(status as u8))
    }

    pub(crate) fn set(&self, status: CaptureStatus) {
        self.0.store(status as u8, Ordering::Release);
    }

    pub(crate) fn get(&self) -> CaptureStatus {
        match self.0.load(Ordering::Acquire) {
            0 => CaptureStatus::Starting,
            1 => CaptureStatus::Running,
            2 => CaptureStatus::Stopped,
            _ => CaptureStatus::Failed,
        }
    }
}
