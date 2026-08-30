//! A solid colour, the simplest source there is.

use voltra_core::{
    FrameSize, PixelFormat, Result, Source, SourceCapabilities, SourceType, TickContext, Timestamp,
    VideoFrame, VideoSource,
};

/// A source that fills its frame with one colour.
///
/// The counterpart of the "Color Source" that ships with OBS: a backdrop, a
/// placeholder, and the material the compositor's tests are built from.
///
/// The frame is allocated once and rewritten in place, never reallocated per
/// tick — the pool measurements in plan 005 are the reason.
///
/// # Examples
///
/// ```
/// use voltra_core::{Fps, FrameSize, Source, TickContext, VideoSource};
/// use voltra_sources::ColorSource;
///
/// let size = FrameSize::new(320, 180)?;
/// let mut source = ColorSource::new(size, [255, 0, 0, 255])?;
/// source.tick(&TickContext::new(0, Fps::FPS_60))?;
///
/// let frame = source.frame().expect("a frame after the first tick");
/// // Stored as BGRA, so red sits in the third byte.
/// assert_eq!(&frame.plane(0).expect("pixels")[..4], &[0, 0, 255, 255]);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct ColorSource {
    frame: VideoFrame,
    /// Red, green, blue, alpha, in that order regardless of storage.
    color: [u8; 4],
    /// Set when the colour or size changed and the frame needs repainting.
    dirty: bool,
    /// Whether the first tick has happened.
    painted: bool,
}

impl ColorSource {
    /// The stable type identifier.
    pub const KIND: &'static str = "color";

    /// Build a source of `size` filled with `color` as `[r, g, b, a]`.
    ///
    /// # Errors
    ///
    /// Propagates from [`VideoFrame::new`].
    pub fn new(size: FrameSize, color: [u8; 4]) -> Result<Self> {
        Ok(Self {
            frame: VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO)?,
            color,
            dirty: true,
            painted: false,
        })
    }

    /// The current colour as `[r, g, b, a]`.
    #[must_use]
    pub const fn color(&self) -> [u8; 4] {
        self.color
    }

    /// Change the colour.
    ///
    /// Takes effect on the next tick, not immediately: the tick is the point
    /// where state changes become visible, so a frame is never composited half
    /// from the old settings and half from the new.
    pub const fn set_color(&mut self, color: [u8; 4]) {
        self.color = color;
        self.dirty = true;
    }

    /// Change the frame size, reallocating only when it actually changed.
    ///
    /// # Errors
    ///
    /// Propagates from [`VideoFrame::new`].
    pub fn set_size(&mut self, size: FrameSize) -> Result<()> {
        if size == self.frame.size() {
            return Ok(());
        }
        self.frame = VideoFrame::new(PixelFormat::Bgra8, size, self.frame.pts())?;
        self.dirty = true;
        Ok(())
    }

    /// Paint the frame, if anything changed since the last time.
    fn repaint(&mut self) {
        if !self.dirty {
            return;
        }
        let [red, green, blue, alpha] = self.color;
        let pixel = [blue, green, red, alpha];
        if let Some(rows) = self.frame.rows_mut(0) {
            for row in rows {
                for target in row.chunks_exact_mut(4) {
                    target.copy_from_slice(&pixel);
                }
            }
        }
        self.dirty = false;
    }
}

impl Source for ColorSource {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn source_type(&self) -> SourceType {
        SourceType::Input
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::VIDEO
    }

    fn tick(&mut self, context: &TickContext) -> Result<()> {
        self.repaint();
        self.frame.set_pts(context.pts());
        self.painted = true;
        Ok(())
    }
}

impl VideoSource for ColorSource {
    fn size(&self) -> Option<FrameSize> {
        Some(self.frame.size())
    }

    fn frame(&self) -> Option<&VideoFrame> {
        self.painted.then_some(&self.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::ColorSource;
    use voltra_core::{Fps, FrameSize, Source, TickContext, Timestamp, VideoSource};

    fn size(width: u32, height: u32) -> FrameSize {
        FrameSize::new(width, height).expect("valid size")
    }

    fn tick(source: &mut ColorSource, index: u64) {
        source
            .tick(&TickContext::new(index, Fps::FPS_60))
            .expect("tick");
    }

    #[test]
    fn produces_the_requested_colour_in_bgra_order() {
        let mut source = ColorSource::new(size(4, 4), [10, 20, 30, 255]).expect("source");
        tick(&mut source, 0);

        let frame = source.frame().expect("frame");
        let pixels = frame.plane(0).expect("pixels");
        assert!(
            pixels.chunks_exact(4).all(|px| px == [30, 20, 10, 255]),
            "every pixel should be the requested colour, stored as BGRA"
        );
    }

    #[test]
    fn reports_no_frame_before_the_first_tick() {
        let source = ColorSource::new(size(4, 4), [0, 0, 0, 255]).expect("source");
        assert!(source.frame().is_none());
        // The size is known up front: this is not an asynchronous device.
        assert_eq!(source.size(), Some(size(4, 4)));
    }

    /// The rule from CLAUDE.md §4.1: nothing allocates per frame.
    #[test]
    fn never_reallocates_between_ticks() {
        let mut source = ColorSource::new(size(64, 64), [1, 2, 3, 255]).expect("source");
        tick(&mut source, 0);
        let address = source.frame().expect("frame").as_bytes().as_ptr();

        for index in 1..100 {
            tick(&mut source, index);
        }
        assert_eq!(
            source.frame().expect("frame").as_bytes().as_ptr(),
            address,
            "the source allocated a new frame"
        );
    }

    #[test]
    fn colour_changes_land_on_the_next_tick() {
        let mut source = ColorSource::new(size(4, 4), [0, 0, 0, 255]).expect("source");
        tick(&mut source, 0);

        source.set_color([255, 255, 255, 255]);
        // Still the old colour: the change is pending until the tick.
        let before = source.frame().expect("frame").plane(0).expect("pixels")[0];
        assert_eq!(before, 0);

        tick(&mut source, 1);
        let after = source.frame().expect("frame").plane(0).expect("pixels")[0];
        assert_eq!(after, 255);
        assert_eq!(source.color(), [255, 255, 255, 255]);
    }

    #[test]
    fn the_timestamp_follows_the_tick() {
        let mut source = ColorSource::new(size(4, 4), [0, 0, 0, 255]).expect("source");
        tick(&mut source, 60);
        assert_eq!(
            source.frame().expect("frame").pts(),
            Timestamp::from_nanos(1_000_000_000)
        );
    }

    #[test]
    fn resizing_reallocates_only_when_the_size_changes() {
        let mut source = ColorSource::new(size(8, 8), [7, 7, 7, 255]).expect("source");
        tick(&mut source, 0);
        let address = source.frame().expect("frame").as_bytes().as_ptr();

        source.set_size(size(8, 8)).expect("same size");
        tick(&mut source, 1);
        assert_eq!(source.frame().expect("frame").as_bytes().as_ptr(), address);

        source.set_size(size(16, 16)).expect("new size");
        tick(&mut source, 2);
        assert_eq!(source.size(), Some(size(16, 16)));
        let pixels = source.frame().expect("frame").plane(0).expect("pixels");
        assert!(
            pixels.chunks_exact(4).all(|px| px == [7, 7, 7, 255]),
            "the new frame should be repainted, not left uninitialised"
        );
    }

    #[test]
    fn declares_itself_correctly() {
        let source = ColorSource::new(size(4, 4), [0, 0, 0, 255]).expect("source");
        assert_eq!(source.kind(), "color");
        assert!(source.capabilities().has_video());
        assert!(!source.capabilities().has_audio());
    }
}
