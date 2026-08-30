//! The CPU-side video frame.
//!
//! One allocation holds every plane, contiguously, with each plane starting at
//! an aligned offset — the layout `video_frame_init` builds in libobs. A frame
//! is one `malloc`, not one per plane, which matters when frames are recycled
//! sixty times a second.
//!
//! # This is the CPU frame, not the only frame
//!
//! libobs keeps two distinct types: `obs_source_frame` for pixels in system
//! memory (cameras, media files, capture APIs that hand over buffers) and
//! `gs_texture_t` for pixels that live on the GPU. Voltra keeps the same split.
//! This type is the former. The GPU texture belongs to `voltra-render`, which
//! owns the graphics backend, and the encoder input will accept either — that
//! is what keeps the zero-copy path of ADR 0003 open.

use crate::pixel::{MAX_PLANES, PixelFormat};
use crate::time::Timestamp;
use crate::{Error, Result};

/// Byte alignment applied to the start of every plane.
///
/// 32 bytes, the width of an AVX2 register, so a vectorised loop over a plane
/// needs no scalar prologue. libobs makes the same choice via
/// `base_get_alignment()`. AVX-512 would want 64; this is a single constant.
pub const PLANE_ALIGNMENT: usize = 32;

/// Validated frame dimensions in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSize {
    width: u32,
    height: u32,
}

impl FrameSize {
    /// Build a frame size.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when either dimension is zero. A zero-sized
    /// frame has no meaning and every consumer would have to special-case it.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::config(format!(
                "frame size {width}x{height} must have non-zero dimensions"
            )));
        }
        Ok(Self { width, height })
    }

    /// Width in pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

impl std::fmt::Display for FrameSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Where one plane lives inside a frame's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    offset: usize,
    stride: usize,
    row_bytes: usize,
    height: u32,
}

impl Plane {
    /// Byte offset of the plane within the frame buffer. Always aligned to
    /// [`PLANE_ALIGNMENT`].
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Bytes from the start of one row to the start of the next.
    ///
    /// This is libobs's `linesize`. It is at least [`row_bytes`](Plane::row_bytes)
    /// and may exceed it for frames adopted from a capture API that pads rows.
    pub const fn stride(self) -> usize {
        self.stride
    }

    /// Meaningful bytes per row, excluding any stride padding.
    pub const fn row_bytes(self) -> usize {
        self.row_bytes
    }

    /// Number of rows in this plane.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Total bytes occupied, padding included.
    pub const fn byte_len(self) -> usize {
        self.stride * self.height as usize
    }
}

const EMPTY_PLANE: Plane = Plane {
    offset: 0,
    stride: 0,
    row_bytes: 0,
    height: 0,
};

/// A frame of video in system memory.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    format: PixelFormat,
    size: FrameSize,
    pts: Timestamp,
    planes: [Plane; MAX_PLANES],
    plane_count: usize,
    data: Vec<u8>,
}

impl VideoFrame {
    /// Allocate a zeroed frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the format needs even dimensions and the
    /// size is odd — 4:2:0 chroma cannot describe half a row — or when the
    /// buffer size overflows `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use voltra_core::{FrameSize, PixelFormat, Timestamp, VideoFrame};
    ///
    /// let size = FrameSize::new(1920, 1080)?;
    /// let frame = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO)?;
    ///
    /// assert_eq!(frame.plane_count(), 3);
    /// assert_eq!(frame.plane(1).map(<[u8]>::len), Some(960 * 540));
    /// # Ok::<(), voltra_core::Error>(())
    /// ```
    pub fn new(format: PixelFormat, size: FrameSize, pts: Timestamp) -> Result<Self> {
        if format.requires_even_size() && (size.width() % 2 != 0 || size.height() % 2 != 0) {
            return Err(Error::config(format!(
                "{format} needs even dimensions, got {size}"
            )));
        }

        let mut planes = [EMPTY_PLANE; MAX_PLANES];
        let mut offset = 0usize;

        for (index, plane) in planes.iter_mut().take(format.plane_count()).enumerate() {
            // Both are `Some` for every plane below `plane_count`; the format
            // is the single source of truth for its own geometry.
            let (Some(stride), Some(height)) = (
                format.plane_stride(index, size.width()),
                format.plane_height(index, size.height()),
            ) else {
                return Err(Error::config(format!(
                    "{format} does not describe plane {index}"
                )));
            };

            let plane_bytes = stride
                .checked_mul(height as usize)
                .ok_or_else(|| Error::config(format!("frame {size} is too large to allocate")))?;

            *plane = Plane {
                offset,
                stride,
                row_bytes: stride,
                height,
            };

            offset = offset
                .checked_add(plane_bytes)
                .map(|end| end.next_multiple_of(PLANE_ALIGNMENT))
                .ok_or_else(|| Error::config(format!("frame {size} is too large to allocate")))?;
        }

        Ok(Self {
            format,
            size,
            pts,
            planes,
            plane_count: format.plane_count(),
            data: vec![0; offset],
        })
    }

    /// The pixel format.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// The frame dimensions.
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// Width in pixels.
    pub const fn width(&self) -> u32 {
        self.size.width()
    }

    /// Height in pixels.
    pub const fn height(&self) -> u32 {
        self.size.height()
    }

    /// The presentation timestamp.
    pub const fn pts(&self) -> Timestamp {
        self.pts
    }

    /// Set the presentation timestamp.
    ///
    /// Recycled frames get a new timestamp rather than a new allocation.
    pub const fn set_pts(&mut self, pts: Timestamp) {
        self.pts = pts;
    }

    /// How many planes this frame has.
    pub const fn plane_count(&self) -> usize {
        self.plane_count
    }

    /// Layout of `index`, or `None` when the plane does not exist.
    pub fn plane_layout(&self, index: usize) -> Option<Plane> {
        (index < self.plane_count).then(|| self.planes[index])
    }

    /// The bytes of `index`, padding included, or `None` when absent.
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        let plane = self.plane_layout(index)?;
        self.data
            .get(plane.offset()..plane.offset() + plane.byte_len())
    }

    /// The bytes of `index` for writing, or `None` when absent.
    pub fn plane_mut(&mut self, index: usize) -> Option<&mut [u8]> {
        let plane = self.plane_layout(index)?;
        self.data
            .get_mut(plane.offset()..plane.offset() + plane.byte_len())
    }

    /// Iterate the rows of `index`, each trimmed to its meaningful bytes.
    ///
    /// Rows come back without stride padding so per-pixel loops can consume
    /// them with `chunks_exact` and stay autovectorisable.
    pub fn rows(&self, index: usize) -> Option<impl Iterator<Item = &[u8]>> {
        let plane = self.plane_layout(index)?;
        let bytes = self.plane(index)?;
        Some(
            bytes
                .chunks_exact(plane.stride())
                .map(move |row| &row[..plane.row_bytes()]),
        )
    }

    /// Iterate the rows of `index` for writing.
    pub fn rows_mut(&mut self, index: usize) -> Option<impl Iterator<Item = &mut [u8]>> {
        let plane = self.plane_layout(index)?;
        let bytes = self.plane_mut(index)?;
        Some(
            bytes
                .chunks_exact_mut(plane.stride())
                .map(move |row| &mut row[..plane.row_bytes()]),
        )
    }

    /// Mutable access to every plane at once.
    ///
    /// Absent planes come back as empty slices. Converting to a planar format
    /// needs to write luma and chroma in the same pass — without this the source
    /// would have to be walked twice, which at 1080p60 costs milliseconds per
    /// second for nothing.
    pub fn planes_mut(&mut self) -> [&mut [u8]; MAX_PLANES] {
        let layouts = self.planes;
        let count = self.plane_count;
        let mut out: [&mut [u8]; MAX_PLANES] = [&mut [], &mut [], &mut []];
        let mut rest: &mut [u8] = &mut self.data;
        let mut consumed = 0usize;

        for (slot, plane) in out.iter_mut().zip(layouts).take(count) {
            // Planes are laid out in ascending order with alignment padding in
            // between, so each split is disjoint from the last.
            let (_padding, tail) = rest.split_at_mut(plane.offset() - consumed);
            let (body, tail) = tail.split_at_mut(plane.byte_len());
            *slot = body;
            rest = tail;
            consumed = plane.offset() + plane.byte_len();
        }
        out
    }

    /// The whole buffer, every plane contiguous.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The whole buffer for writing.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Total bytes allocated, alignment padding included.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameSize, PLANE_ALIGNMENT, VideoFrame};
    use crate::pixel::PixelFormat;
    use crate::time::Timestamp;

    fn frame(format: PixelFormat, width: u32, height: u32) -> VideoFrame {
        let size = FrameSize::new(width, height).expect("valid size");
        VideoFrame::new(format, size, Timestamp::ZERO).expect("valid frame")
    }

    #[test]
    fn packed_frames_have_one_full_size_plane() {
        let frame = frame(PixelFormat::Bgra8, 1920, 1080);
        assert_eq!(frame.plane_count(), 1);
        let plane = frame.plane_layout(0).expect("plane 0");
        assert_eq!(plane.stride(), 7680);
        assert_eq!(plane.height(), 1080);
        assert_eq!(frame.plane(0).map(<[u8]>::len), Some(7680 * 1080));
    }

    #[test]
    fn planar_frames_lay_chroma_out_after_luma() {
        let frame = frame(PixelFormat::I420, 1920, 1080);
        assert_eq!(frame.plane_count(), 3);

        let luma = frame.plane_layout(0).expect("luma");
        let blue = frame.plane_layout(1).expect("chroma u");
        let red = frame.plane_layout(2).expect("chroma v");

        assert_eq!((luma.stride(), luma.height()), (1920, 1080));
        assert_eq!((blue.stride(), blue.height()), (960, 540));
        assert_eq!((red.stride(), red.height()), (960, 540));
        assert!(blue.offset() >= luma.byte_len());
        assert!(red.offset() >= blue.offset() + blue.byte_len());
    }

    #[test]
    fn nv12_chroma_is_full_width_and_half_height() {
        let frame = frame(PixelFormat::Nv12, 1280, 720);
        assert_eq!(frame.plane_count(), 2);
        let chroma = frame.plane_layout(1).expect("chroma");
        assert_eq!((chroma.stride(), chroma.height()), (1280, 360));
    }

    /// The reason the alignment constant exists: a vectorised loop over any
    /// plane must be able to start at a 32-byte boundary.
    #[test]
    fn every_plane_starts_aligned() {
        for format in [
            PixelFormat::Bgra8,
            PixelFormat::Rgba8,
            PixelFormat::Nv12,
            PixelFormat::I420,
            PixelFormat::Y8,
        ] {
            // 1918 is even but not a multiple of the alignment, so any plane
            // that happens to line up does so by construction, not by luck.
            let frame = frame(format, 1918, 1080);
            for index in 0..frame.plane_count() {
                let offset = frame.plane_layout(index).expect("plane").offset();
                assert_eq!(
                    offset % PLANE_ALIGNMENT,
                    0,
                    "{format} plane {index} starts at {offset}"
                );
            }
        }
    }

    #[test]
    fn buffer_length_matches_the_sum_of_aligned_planes() {
        let frame = frame(PixelFormat::I420, 1918, 1080);
        let expected: usize = (0..frame.plane_count())
            .map(|index| {
                frame
                    .plane_layout(index)
                    .expect("plane")
                    .byte_len()
                    .next_multiple_of(PLANE_ALIGNMENT)
            })
            .sum();
        assert_eq!(frame.byte_len(), expected);
    }

    #[test]
    fn subsampled_formats_reject_odd_sizes() {
        let odd = FrameSize::new(1921, 1080).expect("valid size");
        assert!(VideoFrame::new(PixelFormat::I420, odd, Timestamp::ZERO).is_err());
        assert!(VideoFrame::new(PixelFormat::Nv12, odd, Timestamp::ZERO).is_err());
        // Packed formats have no such constraint.
        assert!(VideoFrame::new(PixelFormat::Bgra8, odd, Timestamp::ZERO).is_ok());
        assert!(VideoFrame::new(PixelFormat::Y8, odd, Timestamp::ZERO).is_ok());
    }

    #[test]
    fn zero_sized_frames_are_rejected() {
        assert!(FrameSize::new(0, 1080).is_err());
        assert!(FrameSize::new(1920, 0).is_err());
    }

    #[test]
    fn absent_planes_are_none_not_a_panic() {
        let frame = frame(PixelFormat::Bgra8, 64, 64);
        assert!(frame.plane(1).is_none());
        assert!(frame.plane_layout(9).is_none());
        assert!(frame.rows(1).is_none());
    }

    #[test]
    fn rows_cover_the_plane_exactly() {
        let frame = frame(PixelFormat::I420, 64, 32);
        let rows: Vec<&[u8]> = frame.rows(0).expect("luma rows").collect();
        assert_eq!(rows.len(), 32);
        assert!(rows.iter().all(|row| row.len() == 64));

        let chroma: Vec<&[u8]> = frame.rows(1).expect("chroma rows").collect();
        assert_eq!(chroma.len(), 16);
        assert!(chroma.iter().all(|row| row.len() == 32));
    }

    #[test]
    fn frames_start_zeroed_and_writes_land_in_the_right_plane() {
        let mut frame = frame(PixelFormat::I420, 64, 32);
        assert!(frame.as_bytes().iter().all(|&byte| byte == 0));

        for row in frame.rows_mut(1).expect("chroma rows") {
            row.fill(0x80);
        }

        assert!(frame.plane(0).expect("luma").iter().all(|&b| b == 0));
        assert!(frame.plane(1).expect("chroma u").iter().all(|&b| b == 0x80));
        assert!(frame.plane(2).expect("chroma v").iter().all(|&b| b == 0));
    }

    #[test]
    fn timestamps_can_be_reset_without_reallocating() {
        let mut frame = frame(PixelFormat::Bgra8, 64, 64);
        let address = frame.as_bytes().as_ptr();
        frame.set_pts(Timestamp::from_millis(500));
        assert_eq!(frame.pts(), Timestamp::from_millis(500));
        assert_eq!(frame.as_bytes().as_ptr(), address);
    }
}
