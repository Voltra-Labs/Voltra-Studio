//! Writing YUV4MPEG2 streams.
//!
//! A text header, then every frame as `FRAME\n` followed by its planes in the
//! raw. No container, no entropy coding, no loss: what comes out is exactly
//! what the compositor produced, which is what makes it verifiable byte for
//! byte and what makes it the first output Voltra can show.
//!
//! The format is defined by mjpegtools' `yuv4mpeg(5)`; the tag order and the
//! two `X`-tag conventions (`XYSCSS=`, `XCOLORRANGE=`) follow `FFmpeg`'s
//! `yuv4mpegenc.c`, because that is what every reader in the wild has been
//! tested against. Both are written up in `docs/references/y4m.md`.

use std::io::Write;

use voltra_core::{ColorRange, Error, Fps, FrameSize, PixelFormat, Result, VideoFrame};

/// The frame separator, as a constant so no frame costs a `format!`.
const FRAME_MAGIC: &[u8] = b"FRAME\n";

/// How the stream declares its fields, the `I` tag of the header.
///
/// Voltra composites progressive video, so [`Progressive`](Y4mInterlacing::Progressive)
/// is the default and the only value the pipeline produces today. The rest exist
/// because a reader is entitled to see them and because interlaced capture is a
/// real thing that will arrive with the capture phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Y4mInterlacing {
    /// Whole frames, one after another. The default.
    #[default]
    Progressive,
    /// Interlaced, top field first.
    TopFieldFirst,
    /// Interlaced, bottom field first.
    BottomFieldFirst,
    /// Mixed; each frame says what it is in its own header.
    Mixed,
    /// Unknown, and left for the reader to guess.
    Unknown,
}

impl Y4mInterlacing {
    /// The single character the `I` tag carries.
    const fn tag(self) -> char {
        match self {
            Y4mInterlacing::Progressive => 'p',
            Y4mInterlacing::TopFieldFirst => 't',
            Y4mInterlacing::BottomFieldFirst => 'b',
            Y4mInterlacing::Mixed => 'm',
            Y4mInterlacing::Unknown => '?',
        }
    }
}

/// What goes in the header and cannot be read off a frame.
///
/// [`Default`] is progressive, square pixels, limited range at 60 fps — what a
/// modern player assumes when a stream stays quiet. Build one by overriding
/// what matters and leaving the rest alone:
///
/// ```
/// use voltra_core::{ColorRange, Fps};
/// use voltra_output::Y4mParams;
///
/// let params = Y4mParams {
///     fps: Fps::FPS_29_97,
///     range: ColorRange::Full,
///     ..Y4mParams::default()
/// };
/// ```
///
/// Deliberately *not* `#[non_exhaustive]`: that attribute would forbid exactly
/// the struct expression above from outside this crate, which is the only
/// ergonomic way to build a settings struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Y4mParams {
    /// The frame rate, written as the exact ratio it is.
    ///
    /// This is why the clock is rational: 29.97 has to reach the file as
    /// `F30000:1001`, not as a decimal Y4M cannot express.
    pub fps: Fps,
    /// How the stream declares its fields.
    pub interlacing: Y4mInterlacing,
    /// Pixel aspect ratio as `(numerator, denominator)`. `(1, 1)` is square.
    pub pixel_aspect: (u32, u32),
    /// The range the samples were converted into.
    ///
    /// Y4M has no field for this — the specification says "CCIR-601" and stops
    /// — so it travels in the `XCOLORRANGE` metadata tag `FFmpeg` established.
    /// Without it a full-range stream plays back washed out.
    pub range: ColorRange,
}

impl Default for Y4mParams {
    fn default() -> Self {
        Self {
            fps: Fps::FPS_60,
            interlacing: Y4mInterlacing::Progressive,
            pixel_aspect: (1, 1),
            range: ColorRange::Limited,
        }
    }
}

impl Y4mParams {
    /// Parameters at `fps`, everything else left at its default.
    #[must_use]
    pub fn at(fps: Fps) -> Self {
        Self {
            fps,
            ..Self::default()
        }
    }
}

/// The chroma tag a pixel format maps to, or `None` when Y4M cannot carry it.
///
/// `I420` is written as `420jpeg` rather than `420mpeg2` because that is where
/// our chroma samples actually sit: [`rgb_to_yuv`](voltra_core::rgb_to_yuv)
/// averages the four RGB samples of each 2×2 block, which places the chroma
/// sample at the centre of the block — JPEG/MPEG-1 siting. Claiming MPEG-2
/// siting would shift the colour half a pixel against the luma.
const fn chroma_tag(format: PixelFormat) -> Option<&'static str> {
    match format {
        PixelFormat::I420 => Some("420jpeg"),
        PixelFormat::Y8 => Some("mono"),
        // NV12 interleaves Cb and Cr; Y4M wants them as separate planes.
        // BGRA and RGBA are not YCbCr at all. A format added later is not
        // writable until someone maps it here on purpose, which is the safe
        // default for a `#[non_exhaustive]` enum.
        _ => None,
    }
}

/// The `XCOLORRANGE` value for a range, or `None` when Y4M cannot say it.
///
/// Y4M can only distinguish limited from full. A range added later has to be
/// refused rather than guessed: mislabelling the range is exactly the mistake
/// that makes a file play back washed out.
const fn range_tag(range: ColorRange) -> Option<&'static str> {
    match range {
        ColorRange::Limited => Some("LIMITED"),
        ColorRange::Full => Some("FULL"),
        _ => None,
    }
}

/// Writes a video stream as YUV4MPEG2.
///
/// The header is written when the writer is built, which fixes the format and
/// the size for the rest of the stream: a Y4M whose frames change shape halfway
/// through is a file no reader can follow, so a mismatched frame is refused
/// rather than written.
///
/// Nothing is allocated per frame. The header is formatted once here; the frame
/// separator is a constant; the planes go straight from the frame's buffer to
/// the writer.
///
/// # Examples
///
/// ```
/// use voltra_core::{Fps, FrameSize, PixelFormat, Timestamp, VideoFrame};
/// use voltra_output::{Y4mParams, Y4mWriter};
///
/// let size = FrameSize::new(64, 64)?;
/// let frame = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO)?;
///
/// let mut writer = Y4mWriter::new(
///     Vec::new(),
///     PixelFormat::I420,
///     size,
///     Y4mParams::at(Fps::FPS_30),
/// )?;
/// writer.write_frame(&frame)?;
/// let bytes = writer.finish()?;
///
/// assert!(bytes.starts_with(b"YUV4MPEG2 W64 H64 F30:1 Ip A1:1 C420jpeg"));
/// assert_eq!(writer_len(&bytes), 64 * 64 * 3 / 2 + 6);
/// # fn writer_len(bytes: &[u8]) -> usize {
/// #     bytes.len() - bytes.iter().position(|&b| b == b'\n').expect("header") - 1
/// # }
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct Y4mWriter<W: Write> {
    writer: W,
    format: PixelFormat,
    size: FrameSize,
    frames_written: u64,
}

impl<W: Write> Y4mWriter<W> {
    /// Start a stream, writing its header immediately.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when `format` is not planar YCbCr — Y4M carries
    /// separate `Y`, `Cb` and `Cr` planes, so `NV12` and the packed RGB formats
    /// have no representation. Converting here instead would hide a
    /// milliseconds-per-frame cost inside what looks like a write, so the
    /// caller does it. [`Error::Io`] when the header cannot be written.
    pub fn new(writer: W, format: PixelFormat, size: FrameSize, params: Y4mParams) -> Result<Self> {
        let Some(chroma) = chroma_tag(format) else {
            return Err(Error::unsupported(format!(
                "Y4M carries planar YCbCr; {format} cannot be written directly, convert it first"
            )));
        };
        let (aspect_num, aspect_den) = params.pixel_aspect;
        if aspect_num == 0 || aspect_den == 0 {
            return Err(Error::config(format!(
                "pixel aspect {aspect_num}:{aspect_den} must have non-zero terms"
            )));
        }

        let Some(range) = range_tag(params.range) else {
            return Err(Error::unsupported(format!(
                "Y4M can only declare limited or full range, not {}",
                params.range.name()
            )));
        };
        // `XYSCSS` repeats the chroma code in upper case for mjpegtools
        // predating the `C` tag; `XCOLORRANGE` is the range the specification
        // has no field for. Both are FFmpeg conventions on the free `X` tag.
        let header = format!(
            "YUV4MPEG2 W{width} H{height} F{fps_num}:{fps_den} I{interlacing} A{aspect_num}:{aspect_den} C{chroma} XYSCSS={upper} XCOLORRANGE={range}\n",
            width = size.width(),
            height = size.height(),
            fps_num = params.fps.num(),
            fps_den = params.fps.den(),
            interlacing = params.interlacing.tag(),
            upper = chroma.to_uppercase(),
        );

        let mut writer = writer;
        writer.write_all(header.as_bytes())?;

        Ok(Self {
            writer,
            format,
            size,
            frames_written: 0,
        })
    }

    /// Append one frame.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the frame's format or size differs from the
    /// header's — nothing is written in that case, so the stream stays valid.
    /// [`Error::Io`] from the underlying writer.
    pub fn write_frame(&mut self, frame: &VideoFrame) -> Result<()> {
        if frame.format() != self.format || frame.size() != self.size {
            return Err(Error::config(format!(
                "stream is {} {}, frame is {} {}: a Y4M stream cannot change shape",
                self.format,
                self.size,
                frame.format(),
                frame.size()
            )));
        }

        self.writer.write_all(FRAME_MAGIC)?;
        for index in 0..frame.plane_count() {
            // Both are `Some` for every index below `plane_count`.
            let (Some(layout), Some(bytes)) = (frame.plane_layout(index), frame.plane(index))
            else {
                return Err(Error::config(format!(
                    "{} does not describe plane {index}",
                    frame.format()
                )));
            };
            write_plane(
                &mut self.writer,
                bytes,
                layout.stride(),
                layout.row_bytes(),
                layout.height(),
            )?;
        }

        self.frames_written += 1;
        Ok(())
    }

    /// How many frames have been appended.
    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// The pixel format fixed by the header.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// The frame size fixed by the header.
    #[must_use]
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// Flush and hand the underlying writer back.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the flush fails. A file whose last write never
    /// reached the disk is a truncated stream, so this is worth checking rather
    /// than leaving to `Drop`.
    pub fn finish(mut self) -> Result<W> {
        self.writer.flush()?;
        Ok(self.writer)
    }
}

/// Write one plane, leaving any stride padding behind.
///
/// Takes the layout as loose parameters rather than a frame so the padded path
/// can be tested today: every frame Voltra allocates has `stride == row_bytes`,
/// and padded rows only arrive with frames adopted from a capture API, which do
/// not exist yet. Writing `stride` bytes per row instead of `row_bytes` is the
/// classic way to produce a file that almost looks right.
fn write_plane<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    stride: usize,
    row_bytes: usize,
    height: u32,
) -> std::io::Result<()> {
    if stride == row_bytes {
        // The common case: the plane is contiguous, so it is one write rather
        // than one per row.
        return writer.write_all(&bytes[..row_bytes * height as usize]);
    }

    for row in 0..height as usize {
        let start = row * stride;
        writer.write_all(&bytes[start..start + row_bytes])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FRAME_MAGIC, Y4mInterlacing, Y4mParams, Y4mWriter, write_plane};

    use voltra_core::{ColorRange, Fps, FrameSize, PixelFormat, Timestamp, VideoFrame};

    fn size(width: u32, height: u32) -> FrameSize {
        FrameSize::new(width, height).unwrap()
    }

    fn frame(format: PixelFormat, width: u32, height: u32) -> VideoFrame {
        VideoFrame::new(format, size(width, height), Timestamp::ZERO).unwrap()
    }

    /// The bytes up to and including the first newline.
    fn header_of(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == b'\n').expect("a header");
        String::from_utf8(bytes[..=end].to_vec()).expect("ASCII header")
    }

    fn write(format: PixelFormat, width: u32, height: u32, params: Y4mParams) -> Vec<u8> {
        let writer =
            Y4mWriter::new(Vec::new(), format, size(width, height), params).expect("a writer");
        writer.finish().expect("flushing a Vec cannot fail")
    }

    /// The header is what every reader parses first and what is easiest to get
    /// quietly wrong, so it is pinned byte for byte.
    #[test]
    fn the_header_is_exact() {
        let bytes = write(PixelFormat::I420, 1280, 720, Y4mParams::at(Fps::FPS_60));
        assert_eq!(
            header_of(&bytes),
            "YUV4MPEG2 W1280 H720 F60:1 Ip A1:1 C420jpeg XYSCSS=420JPEG XCOLORRANGE=LIMITED\n"
        );
    }

    /// The whole point of a rational clock: 29.97 reaches the file as the exact
    /// ratio, because Y4M cannot express a decimal and 30:1 would drift.
    #[test]
    fn drop_frame_rates_stay_rational() {
        let bytes = write(PixelFormat::I420, 640, 480, Y4mParams::at(Fps::FPS_29_97));
        assert!(header_of(&bytes).contains(" F30000:1001 "));
    }

    #[test]
    fn full_range_is_declared() {
        let params = Y4mParams {
            range: ColorRange::Full,
            ..Y4mParams::at(Fps::FPS_30)
        };
        let bytes = write(PixelFormat::I420, 64, 64, params);
        assert!(header_of(&bytes).ends_with(" XCOLORRANGE=FULL\n"));
    }

    #[test]
    fn interlacing_and_aspect_reach_the_header() {
        let params = Y4mParams {
            interlacing: Y4mInterlacing::TopFieldFirst,
            pixel_aspect: (16, 15),
            ..Y4mParams::at(Fps::FPS_30)
        };
        let bytes = write(PixelFormat::I420, 720, 576, params);
        assert!(header_of(&bytes).contains(" It A16:15 "));
    }

    #[test]
    fn luma_only_frames_are_monochrome() {
        let bytes = write(PixelFormat::Y8, 64, 64, Y4mParams::default());
        assert!(header_of(&bytes).contains(" Cmono "));
        assert!(header_of(&bytes).contains(" XYSCSS=MONO "));
    }

    /// 4:2:0 is one luma sample and half a chroma sample per pixel, plus the
    /// six bytes of `FRAME\n`.
    #[test]
    fn each_frame_costs_exactly_its_planes() {
        let mut writer = Y4mWriter::new(
            Vec::new(),
            PixelFormat::I420,
            size(64, 32),
            Y4mParams::default(),
        )
        .unwrap();
        let header_len = {
            let mut probe = Y4mWriter::new(
                Vec::new(),
                PixelFormat::I420,
                size(64, 32),
                Y4mParams::default(),
            )
            .unwrap();
            probe
                .write_frame(&frame(PixelFormat::I420, 64, 32))
                .unwrap();
            probe.finish().unwrap().len() - (6 + 64 * 32 * 3 / 2)
        };

        for _ in 0..3 {
            writer
                .write_frame(&frame(PixelFormat::I420, 64, 32))
                .unwrap();
        }
        assert_eq!(writer.frames_written(), 3);

        let bytes = writer.finish().unwrap();
        assert_eq!(bytes.len(), header_len + 3 * (6 + 64 * 32 * 3 / 2));
    }

    /// A stream that changes shape halfway through is unreadable, so the frame
    /// is refused and nothing is written.
    #[test]
    fn a_mismatched_frame_is_refused_without_writing() {
        let mut writer = Y4mWriter::new(
            Vec::new(),
            PixelFormat::I420,
            size(64, 32),
            Y4mParams::default(),
        )
        .unwrap();
        let before = writer.frames_written();

        assert!(
            writer
                .write_frame(&frame(PixelFormat::I420, 32, 32))
                .is_err()
        );
        assert!(writer.write_frame(&frame(PixelFormat::Y8, 64, 32)).is_err());

        assert_eq!(writer.frames_written(), before);
        let bytes = writer.finish().unwrap();
        assert!(
            !bytes.windows(FRAME_MAGIC.len()).any(|w| w == FRAME_MAGIC),
            "a refused frame must leave no trace in the stream"
        );
    }

    #[test]
    fn formats_y4m_cannot_carry_are_refused_up_front() {
        for format in [PixelFormat::Nv12, PixelFormat::Bgra8, PixelFormat::Rgba8] {
            let result = Y4mWriter::new(Vec::new(), format, size(64, 64), Y4mParams::default());
            assert!(result.is_err(), "{format} should not be writable as Y4M");
        }
    }

    #[test]
    fn a_degenerate_aspect_ratio_is_refused() {
        let params = Y4mParams {
            pixel_aspect: (0, 1),
            ..Y4mParams::default()
        };
        assert!(Y4mWriter::new(Vec::new(), PixelFormat::I420, size(64, 64), params).is_err());
    }

    /// Padded rows exist in frames adopted from capture APIs. Writing `stride`
    /// instead of `row_bytes` produces a file that almost looks right, which is
    /// the worst kind of wrong.
    #[test]
    fn stride_padding_never_reaches_the_stream() {
        // Three rows of four meaningful bytes, each padded to six.
        let plane = [
            1, 2, 3, 4, 0xFF, 0xFF, //
            5, 6, 7, 8, 0xFF, 0xFF, //
            9, 10, 11, 12, 0xFF, 0xFF,
        ];
        let mut out = Vec::new();
        write_plane(&mut out, &plane, 6, 4, 3).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn contiguous_planes_take_the_single_write_path() {
        let plane = [1, 2, 3, 4, 5, 6];
        let mut out = Vec::new();
        write_plane(&mut out, &plane, 3, 3, 2).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    /// A failing sink has to surface as an error: a dropped write is a
    /// truncated recording, and CLAUDE.md §3 forbids panicking in library code.
    #[test]
    fn a_failing_writer_propagates_instead_of_panicking() {
        #[derive(Debug)]
        struct Broken;

        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let result = Y4mWriter::new(
            Broken,
            PixelFormat::I420,
            size(64, 64),
            Y4mParams::default(),
        );
        assert!(matches!(result, Err(voltra_core::Error::Io(_))));
    }
}
