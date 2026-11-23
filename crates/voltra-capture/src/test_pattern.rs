//! A capture with no device: a thread that paints a moving bar at a fixed
//! rate. What exercises the delivery on a machine with nothing to capture, the
//! way a real backend's system thread would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use voltra_core::{Fps, FrameSize, PixelFormat, Result, VideoFrame};

use crate::delivery::{FrameWriter, frame_channel};
use crate::source::{Capture, CaptureSource, CaptureStatus, StatusCell};

/// The background, a dark blue-grey, and the bar, white. BGRA.
const BACKGROUND: [u8; 4] = [0x30, 0x20, 0x18, 0xFF];
const BAR: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
/// How far the bar moves per frame, and how wide it is, in pixels.
const STEP: u64 = 8;
const BAR_WIDTH: u64 = 16;

/// A capture thread publishing a moving bar, `size` in BGRA, at `fps`.
///
/// Paced by absolute deadlines from its start, as a real capture is paced by
/// the display: a late frame does not push the next ones later. Frames are
/// stamped on the capture's own timeline, frame `n` at `fps.pts(n)`.
#[derive(Debug)]
pub struct TestPatternCapture {
    stop: Arc<AtomicBool>,
    status: Arc<StatusCell>,
    thread: Option<JoinHandle<()>>,
}

impl TestPatternCapture {
    /// Start capturing, and return the scene source that shows it.
    ///
    /// # Errors
    ///
    /// When the frames cannot be allocated or the thread cannot be spawned.
    ///
    /// # Examples
    ///
    /// ```
    /// use voltra_capture::TestPatternCapture;
    /// use voltra_core::{Fps, FrameSize, Source, TickContext, VideoSource};
    ///
    /// let mut source = TestPatternCapture::start(FrameSize::new(64, 36)?, Fps::FPS_60)?;
    /// // A render loop ticks it once per output frame and draws its frame.
    /// std::thread::sleep(std::time::Duration::from_millis(50));
    /// source.tick(&TickContext::new(0, Fps::FPS_60))?;
    /// assert!(source.frame().is_some());
    /// source.stop();
    /// # Ok::<(), voltra_core::Error>(())
    /// ```
    pub fn start(size: FrameSize, fps: Fps) -> Result<CaptureSource> {
        let (writer, reader) = frame_channel(PixelFormat::Bgra8, size)?;
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(StatusCell::new(CaptureStatus::Starting));
        let thread = {
            let stop = Arc::clone(&stop);
            let status = Arc::clone(&status);
            std::thread::Builder::new()
                .name("voltra-test-capture".to_owned())
                .spawn(move || run(writer, size, fps, &stop, &status))?
        };
        let backend = Self {
            stop,
            status,
            thread: Some(thread),
        };
        Ok(CaptureSource::new(reader, Box::new(backend)))
    }
}

impl Capture for TestPatternCapture {
    fn kind(&self) -> &'static str {
        "test_pattern_capture"
    }

    fn status(&self) -> CaptureStatus {
        self.status.get()
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // The thread never panics; if it did, there is nothing to recover.
            let _ = thread.join();
        }
    }
}

impl Drop for TestPatternCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The capture thread.
fn run(mut writer: FrameWriter, size: FrameSize, fps: Fps, stop: &AtomicBool, status: &StatusCell) {
    let start = Instant::now();
    let mut n = 0u64;
    while !stop.load(Ordering::Acquire) {
        let Ok(frame) = writer.frame_mut(PixelFormat::Bgra8, size) else {
            status.set(CaptureStatus::Failed);
            return;
        };
        paint(frame, n);
        writer.publish(fps.pts(n));
        if n == 0 {
            status.set(CaptureStatus::Running);
        }
        n += 1;
        let deadline = start + Duration::from_nanos(fps.pts(n).as_nanos());
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    status.set(CaptureStatus::Stopped);
}

/// Frame `n`: the background with the bar `STEP × n` pixels in, wrapping.
fn paint(frame: &mut VideoFrame, n: u64) {
    let width = u64::from(frame.width());
    let left = (n * STEP) % width.max(1);
    let Some(rows) = frame.rows_mut(0) else {
        return;
    };
    for row in rows {
        for (x, pixel) in (0u64..).zip(row.chunks_exact_mut(4)) {
            let on_bar = x >= left && x < left + BAR_WIDTH;
            pixel.copy_from_slice(if on_bar { &BAR } else { &BACKGROUND });
        }
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::{Fps, FrameSize, PixelFormat, Timestamp, VideoFrame};

    use super::{BACKGROUND, BAR, paint};

    #[test]
    fn the_bar_moves_eight_pixels_a_frame() {
        let size = FrameSize::new(64, 4).unwrap();
        let mut frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).unwrap();
        for n in [0u64, 1, 7] {
            paint(&mut frame, n);
            let row: Vec<&[u8]> = frame.plane(0).unwrap()[..64 * 4].chunks_exact(4).collect();
            let left = usize::try_from(n * 8 % 64).unwrap();
            for (x, pixel) in row.iter().enumerate() {
                let expected = if (left..left + 16).contains(&x) {
                    BAR
                } else {
                    BACKGROUND
                };
                assert_eq!(*pixel, expected, "frame {n}, pixel {x}");
            }
        }
        let _ = Fps::FPS_60;
    }
}
