//! Video capture from a PipeWire node (feature `pipewire`, ADR 0007).
//!
//! A screen shared through the `ScreenCast` portal, a camera, or anything else
//! that publishes video on PipeWire. The backend runs its own PipeWire loop on
//! its own thread, as OBS runs a `pw_thread_loop`; nothing of PipeWire reaches
//! the render thread, which only sees the frame channel (ADR 0006).
//!
//! On each `process` the backend drains PipeWire's queue to the newest buffer,
//! as OBS does, counting what it drained; skips buffers flagged corrupted or
//! empty; copies the newest into its frame, converting to BGRA; and publishes
//! it with its own timestamp, brought onto the process's clock.

mod stream;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use voltra_core::{FrameSize, PixelFormat, Result};

use crate::delivery::frame_channel;
use crate::source::{BackendStats, Capture, CaptureSource, CaptureStatus, StatusCell};

/// Which PipeWire node to capture, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeWireTarget {
    /// The node to connect to, by id; `None` lets the session manager pick.
    pub node: Option<u32>,
    /// Whether the session manager links the stream. Off, something else must
    /// link its ports — `pw-link`, as the tests do.
    pub autoconnect: bool,
    /// The capture's node name in the PipeWire graph.
    pub name: String,
    /// The server to connect to: a socket name in the runtime directory, or an
    /// absolute path to one. `None` is the default server.
    pub remote: Option<String>,
}

impl PipeWireTarget {
    /// Connect to `node`, or to whatever the session manager offers, linked by
    /// the session manager.
    #[must_use]
    pub fn node(node: Option<u32>) -> Self {
        Self {
            node,
            autoconnect: true,
            name: "voltra-capture".to_owned(),
            remote: None,
        }
    }
}

/// A capture backend reading video from PipeWire.
pub struct PipeWireCapture {
    quit: ::pipewire::channel::Sender<()>,
    status: Arc<StatusCell>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireCapture {
    /// Start capturing `target`, and return the scene source that shows it.
    ///
    /// The source has no frame until PipeWire has negotiated a format and
    /// delivered the first buffer; until then its status is
    /// [`CaptureStatus::Starting`]. A failure after this returns — the server
    /// gone, the node removed — turns the status to
    /// [`CaptureStatus::Failed`] and is logged.
    ///
    /// # Errors
    ///
    /// When the frames cannot be allocated or the thread cannot be spawned.
    pub fn start(target: PipeWireTarget) -> Result<CaptureSource> {
        // The frames are reallocated to the negotiated size on the first
        // buffer; this one is a placeholder.
        let placeholder = FrameSize::new(16, 16)?;
        let (writer, reader) = frame_channel(PixelFormat::Bgra8, placeholder)?;
        let (quit, quit_receiver) = ::pipewire::channel::channel();
        let status = Arc::new(StatusCell::new(CaptureStatus::Starting));
        let counters = Arc::new(Counters::default());
        let thread = {
            let status = Arc::clone(&status);
            let counters = Arc::clone(&counters);
            std::thread::Builder::new()
                .name("voltra-pipewire".to_owned())
                .spawn(move || {
                    let outcome = stream::run(&target, writer, quit_receiver, &status, &counters);
                    match outcome {
                        Ok(()) => {
                            if status.get() != CaptureStatus::Failed {
                                status.set(CaptureStatus::Stopped);
                            }
                        }
                        Err(error) => {
                            tracing::error!("PipeWire capture failed: {error}");
                            status.set(CaptureStatus::Failed);
                        }
                    }
                })?
        };
        let backend = Self {
            quit,
            status,
            counters,
            thread: Some(thread),
        };
        Ok(CaptureSource::new(reader, Box::new(backend)))
    }
}

impl std::fmt::Debug for PipeWireCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The quit channel has nothing to show.
        f.debug_struct("PipeWireCapture")
            .field("status", &self.status.get())
            .field("counters", &self.counters)
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

impl Capture for PipeWireCapture {
    fn kind(&self) -> &'static str {
        "pipewire_capture"
    }

    fn status(&self) -> CaptureStatus {
        self.status.get()
    }

    fn backend_stats(&self) -> BackendStats {
        self.counters.snapshot()
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // If the loop already ended, nobody is listening; that is fine.
            let _ = self.quit.send(());
            let _ = thread.join();
        }
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the backend counts beyond the frame channel.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    source_dropped: AtomicU64,
    skipped: AtomicU64,
    anchors: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> BackendStats {
        BackendStats {
            source_dropped: self.source_dropped.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            clock_anchors: self.anchors.load(Ordering::Relaxed),
        }
    }
}
