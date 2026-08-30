//! Converting the composited canvas into what an encoder accepts.
//!
//! Packed RGB in, planar 4:2:0 out. The loops here are the first genuinely hot
//! ones in the project, so they follow the rules CLAUDE.md §4 lays down and the
//! measurement from plan 003 — a badly shaped read loop ran five times slower
//! than memory:
//!
//! - Row *pairs*, the natural unit of 4:2:0: each 2×2 block yields four luma
//!   samples and one chroma pair.
//! - `chunks_exact` everywhere, so no bounds check survives in the inner loop.
//! - Integer arithmetic only; the coefficients are fixed point.
//! - Channel order is a const generic, so BGRA and RGBA compile to separate
//!   loops with no per-pixel branch.
//!
//! Chroma averages the four RGB samples of a block *before* converting, which
//! is one addition instead of four conversions. libyuv does the same. It is not
//! bit-identical to converting first and averaging after, so a comparison
//! against `FFmpeg` may differ by a code value.

use crate::color::{ColorSpec, RgbToYuv};
use crate::frame::VideoFrame;
use crate::pixel::PixelFormat;
use crate::{Error, Result};

/// Bytes per packed RGB pixel.
const RGB_BYTES: usize = 4;

/// Convert a packed RGB frame into a planar 4:2:0 frame.
///
/// Supported sources are [`PixelFormat::Bgra8`] and [`PixelFormat::Rgba8`];
/// supported destinations are [`PixelFormat::I420`] and [`PixelFormat::Nv12`].
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for any other format pair, and
/// [`Error::Config`] when the two frames differ in size.
///
/// # Examples
///
/// ```
/// use voltra_core::{ColorSpec, FrameSize, PixelFormat, Timestamp, VideoFrame, rgb_to_yuv};
///
/// let size = FrameSize::new(64, 64)?;
/// let mut canvas = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO)?;
/// // Paint the canvas white.
/// canvas.as_bytes_mut().fill(0xFF);
///
/// let mut encoded = VideoFrame::new(PixelFormat::Nv12, size, Timestamp::ZERO)?;
/// rgb_to_yuv(&canvas, &mut encoded, ColorSpec::BT709_LIMITED)?;
///
/// // White is luma 235 in limited range, with neutral chroma.
/// assert_eq!(encoded.plane(0).and_then(|luma| luma.first()), Some(&235));
/// assert_eq!(encoded.plane(1).and_then(|chroma| chroma.first()), Some(&128));
/// # Ok::<(), voltra_core::Error>(())
/// ```
pub fn rgb_to_yuv(
    source: &VideoFrame,
    destination: &mut VideoFrame,
    spec: ColorSpec,
) -> Result<()> {
    if source.size() != destination.size() {
        return Err(Error::config(format!(
            "cannot convert {} into {}: sizes differ",
            source.size(),
            destination.size()
        )));
    }

    let coefficients = spec.rgb_to_yuv();
    let width = source.width() as usize;
    let height = source.height() as usize;

    let Some(source_plane) = source.plane_layout(0) else {
        return Err(Error::unsupported(format!(
            "{} has no pixel plane",
            source.format()
        )));
    };
    let source_stride = source_plane.stride();
    let Some(pixels) = source.plane(0) else {
        return Err(Error::unsupported(format!(
            "{} has no pixels",
            source.format()
        )));
    };

    let destination_format = destination.format();
    let strides = plane_strides(destination);
    let planes = destination.planes_mut();

    match (source.format(), destination_format) {
        (PixelFormat::Bgra8, PixelFormat::I420) => {
            to_i420::<2, 0>(
                pixels,
                source_stride,
                planes,
                strides,
                width,
                height,
                &coefficients,
            );
        }
        (PixelFormat::Rgba8, PixelFormat::I420) => {
            to_i420::<0, 2>(
                pixels,
                source_stride,
                planes,
                strides,
                width,
                height,
                &coefficients,
            );
        }
        (PixelFormat::Bgra8, PixelFormat::Nv12) => {
            to_nv12::<2, 0>(
                pixels,
                source_stride,
                planes,
                strides,
                width,
                height,
                &coefficients,
            );
        }
        (PixelFormat::Rgba8, PixelFormat::Nv12) => {
            to_nv12::<0, 2>(
                pixels,
                source_stride,
                planes,
                strides,
                width,
                height,
                &coefficients,
            );
        }
        (from, into) => {
            return Err(Error::unsupported(format!(
                "colour conversion from {from} to {into}"
            )));
        }
    }

    destination.set_pts(source.pts());
    Ok(())
}

/// The stride of each plane, zero for planes the format does not have.
fn plane_strides(frame: &VideoFrame) -> [usize; crate::pixel::MAX_PLANES] {
    let mut strides = [0; crate::pixel::MAX_PLANES];
    for (index, stride) in strides.iter_mut().enumerate() {
        *stride = frame
            .plane_layout(index)
            .map_or(0, crate::frame::Plane::stride);
    }
    strides
}

/// One 2×2 block: four luma samples and the averaged chroma pair.
///
/// `RED` and `BLUE` are byte offsets within a pixel, so the caller's channel
/// order disappears at compile time.
#[inline]
fn block<const RED: usize, const BLUE: usize>(
    top: &[u8],
    bottom: &[u8],
    luma_top: &mut [u8],
    luma_bottom: &mut [u8],
    coefficients: &RgbToYuv,
) -> (u8, u8) {
    let samples = [
        (i32::from(top[RED]), i32::from(top[1]), i32::from(top[BLUE])),
        (
            i32::from(top[RGB_BYTES + RED]),
            i32::from(top[RGB_BYTES + 1]),
            i32::from(top[RGB_BYTES + BLUE]),
        ),
        (
            i32::from(bottom[RED]),
            i32::from(bottom[1]),
            i32::from(bottom[BLUE]),
        ),
        (
            i32::from(bottom[RGB_BYTES + RED]),
            i32::from(bottom[RGB_BYTES + 1]),
            i32::from(bottom[RGB_BYTES + BLUE]),
        ),
    ];

    luma_top[0] = coefficients.luma(samples[0].0, samples[0].1, samples[0].2);
    luma_top[1] = coefficients.luma(samples[1].0, samples[1].1, samples[1].2);
    luma_bottom[0] = coefficients.luma(samples[2].0, samples[2].1, samples[2].2);
    luma_bottom[1] = coefficients.luma(samples[3].0, samples[3].1, samples[3].2);

    // Average the block, rounding to nearest, then convert once.
    let red = (samples[0].0 + samples[1].0 + samples[2].0 + samples[3].0 + 2) >> 2;
    let green = (samples[0].1 + samples[1].1 + samples[2].1 + samples[3].1 + 2) >> 2;
    let blue = (samples[0].2 + samples[1].2 + samples[2].2 + samples[3].2 + 2) >> 2;
    coefficients.chroma(red, green, blue)
}

/// Packed RGB to three-plane 4:2:0.
fn to_i420<const RED: usize, const BLUE: usize>(
    pixels: &[u8],
    source_stride: usize,
    planes: [&mut [u8]; crate::pixel::MAX_PLANES],
    strides: [usize; crate::pixel::MAX_PLANES],
    width: usize,
    height: usize,
    coefficients: &RgbToYuv,
) {
    let [luma, blue_chroma, red_chroma] = planes;
    let row_bytes = width * RGB_BYTES;
    let half_width = width / 2;

    let source_rows = pixels
        .get(..source_stride * height)
        .unwrap_or(pixels)
        .chunks_exact(source_stride * 2);
    let luma_rows = luma.chunks_exact_mut(strides[0] * 2);
    let blue_rows = blue_chroma.chunks_exact_mut(strides[1]);
    let red_rows = red_chroma.chunks_exact_mut(strides[2]);

    for (((source_pair, luma_pair), blue_row), red_row) in
        source_rows.zip(luma_rows).zip(blue_rows).zip(red_rows)
    {
        let (source_top, source_bottom) = source_pair.split_at(source_stride);
        let (luma_top, luma_bottom) = luma_pair.split_at_mut(strides[0]);

        let blocks = source_top[..row_bytes]
            .chunks_exact(RGB_BYTES * 2)
            .zip(source_bottom[..row_bytes].chunks_exact(RGB_BYTES * 2))
            .zip(luma_top[..width].chunks_exact_mut(2))
            .zip(luma_bottom[..width].chunks_exact_mut(2))
            .zip(
                blue_row[..half_width]
                    .iter_mut()
                    .zip(red_row[..half_width].iter_mut()),
            );

        for ((((top, bottom), luma_top_pair), luma_bottom_pair), (blue, red)) in blocks {
            let (u, v) =
                block::<RED, BLUE>(top, bottom, luma_top_pair, luma_bottom_pair, coefficients);
            *blue = u;
            *red = v;
        }
    }
}

/// Packed RGB to two-plane 4:2:0 with interleaved chroma.
fn to_nv12<const RED: usize, const BLUE: usize>(
    pixels: &[u8],
    source_stride: usize,
    planes: [&mut [u8]; crate::pixel::MAX_PLANES],
    strides: [usize; crate::pixel::MAX_PLANES],
    width: usize,
    height: usize,
    coefficients: &RgbToYuv,
) {
    let [luma, chroma, _] = planes;
    let row_bytes = width * RGB_BYTES;

    let source_rows = pixels
        .get(..source_stride * height)
        .unwrap_or(pixels)
        .chunks_exact(source_stride * 2);
    let luma_rows = luma.chunks_exact_mut(strides[0] * 2);
    let chroma_rows = chroma.chunks_exact_mut(strides[1]);

    for ((source_pair, luma_pair), chroma_row) in source_rows.zip(luma_rows).zip(chroma_rows) {
        let (source_top, source_bottom) = source_pair.split_at(source_stride);
        let (luma_top, luma_bottom) = luma_pair.split_at_mut(strides[0]);

        let blocks = source_top[..row_bytes]
            .chunks_exact(RGB_BYTES * 2)
            .zip(source_bottom[..row_bytes].chunks_exact(RGB_BYTES * 2))
            .zip(luma_top[..width].chunks_exact_mut(2))
            .zip(luma_bottom[..width].chunks_exact_mut(2))
            .zip(chroma_row[..width].chunks_exact_mut(2));

        for ((((top, bottom), luma_top_pair), luma_bottom_pair), chroma_pair) in blocks {
            let (u, v) =
                block::<RED, BLUE>(top, bottom, luma_top_pair, luma_bottom_pair, coefficients);
            chroma_pair[0] = u;
            chroma_pair[1] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rgb_to_yuv;
    use crate::color::{ColorRange, ColorSpace, ColorSpec};
    use crate::frame::{FrameSize, VideoFrame};
    use crate::pixel::PixelFormat;
    use crate::time::Timestamp;

    /// A frame of a single colour, in the channel order of `format`.
    fn flat(
        format: PixelFormat,
        width: u32,
        height: u32,
        red: u8,
        green: u8,
        blue: u8,
    ) -> VideoFrame {
        let size = FrameSize::new(width, height).expect("valid size");
        let mut frame = VideoFrame::new(format, size, Timestamp::ZERO).expect("valid frame");
        let pixel = match format {
            PixelFormat::Bgra8 => [blue, green, red, 255],
            PixelFormat::Rgba8 => [red, green, blue, 255],
            other => unreachable!("{other} is not a packed RGB format"),
        };
        for row in frame.rows_mut(0).expect("pixel rows") {
            for chunk in row.chunks_exact_mut(4) {
                chunk.copy_from_slice(&pixel);
            }
        }
        frame
    }

    fn empty(format: PixelFormat, width: u32, height: u32) -> VideoFrame {
        let size = FrameSize::new(width, height).expect("valid size");
        VideoFrame::new(format, size, Timestamp::ZERO).expect("valid frame")
    }

    /// Known colours, checked against the coefficients rather than against
    /// numbers copied from somewhere: the coefficients themselves are already
    /// pinned to the standard in `color::tests`.
    #[test]
    fn flat_colours_convert_to_their_expected_samples() {
        let spec = ColorSpec::BT709_LIMITED;
        let coefficients = spec.rgb_to_yuv();

        for (red, green, blue) in [
            (0, 0, 0),
            (255, 255, 255),
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (128, 128, 128),
        ] {
            let source = flat(PixelFormat::Bgra8, 16, 16, red, green, blue);
            let mut destination = empty(PixelFormat::I420, 16, 16);
            rgb_to_yuv(&source, &mut destination, spec).expect("supported conversion");

            let luma = coefficients.luma(red.into(), green.into(), blue.into());
            let (u, v) = coefficients.chroma(red.into(), green.into(), blue.into());

            assert!(
                destination
                    .plane(0)
                    .expect("luma")
                    .iter()
                    .all(|&s| s == luma),
                "luma for rgb({red},{green},{blue}) should be {luma}"
            );
            assert!(destination.plane(1).expect("cb").iter().all(|&s| s == u));
            assert!(destination.plane(2).expect("cr").iter().all(|&s| s == v));
        }
    }

    #[test]
    fn limited_range_black_and_white_land_on_16_and_235() {
        let spec = ColorSpec::BT709_LIMITED;

        let black = flat(PixelFormat::Bgra8, 8, 8, 0, 0, 0);
        let mut out = empty(PixelFormat::I420, 8, 8);
        rgb_to_yuv(&black, &mut out, spec).expect("supported conversion");
        assert!(out.plane(0).expect("luma").iter().all(|&s| s == 16));

        let white = flat(PixelFormat::Bgra8, 8, 8, 255, 255, 255);
        rgb_to_yuv(&white, &mut out, spec).expect("supported conversion");
        assert!(out.plane(0).expect("luma").iter().all(|&s| s == 235));
    }

    /// NV12 stores Cb then Cr in one plane. Getting this backwards swaps red
    /// and blue in the encoded stream, which is invisible until playback.
    #[test]
    fn nv12_interleaves_blue_chroma_before_red() {
        let spec = ColorSpec::BT709_LIMITED;
        let coefficients = spec.rgb_to_yuv();
        let source = flat(PixelFormat::Bgra8, 8, 8, 255, 0, 0);
        let mut destination = empty(PixelFormat::Nv12, 8, 8);
        rgb_to_yuv(&source, &mut destination, spec).expect("supported conversion");

        let (u, v) = coefficients.chroma(255, 0, 0);
        let chroma = destination.plane(1).expect("chroma");
        assert_eq!(chroma[0], u, "first sample should be Cb");
        assert_eq!(chroma[1], v, "second sample should be Cr");
        assert!(chroma.chunks_exact(2).all(|pair| pair == [u, v]));
    }

    #[test]
    fn channel_order_does_not_change_the_result() {
        let spec = ColorSpec::BT709_LIMITED;
        let bgra = flat(PixelFormat::Bgra8, 8, 8, 200, 100, 50);
        let rgba = flat(PixelFormat::Rgba8, 8, 8, 200, 100, 50);

        let mut from_bgra = empty(PixelFormat::I420, 8, 8);
        let mut from_rgba = empty(PixelFormat::I420, 8, 8);
        rgb_to_yuv(&bgra, &mut from_bgra, spec).expect("supported conversion");
        rgb_to_yuv(&rgba, &mut from_rgba, spec).expect("supported conversion");

        assert_eq!(from_bgra.as_bytes(), from_rgba.as_bytes());
    }

    /// Luma keeps full resolution while chroma is averaged over the block.
    #[test]
    fn chroma_averages_the_block_but_luma_does_not() {
        let spec = ColorSpec::BT709_LIMITED;
        let size = FrameSize::new(2, 2).expect("valid size");
        let mut source = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).expect("frame");

        // Two black pixels and two white ones, chequered.
        let pixels: [[u8; 4]; 4] = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
        ];
        for (row, pair) in source
            .rows_mut(0)
            .expect("pixel rows")
            .zip(pixels.chunks_exact(2))
        {
            row[..4].copy_from_slice(&pair[0]);
            row[4..8].copy_from_slice(&pair[1]);
        }

        let mut destination =
            VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).expect("frame");
        rgb_to_yuv(&source, &mut destination, spec).expect("supported conversion");

        // Luma keeps every pixel distinct.
        assert_eq!(destination.plane(0).expect("luma"), &[16, 235, 235, 16]);
        // Chroma sees mid-grey, which is neutral.
        assert_eq!(destination.plane(1).expect("cb"), &[128]);
        assert_eq!(destination.plane(2).expect("cr"), &[128]);
    }

    #[test]
    fn every_supported_spec_round_trips_a_grey_without_a_colour_cast() {
        for space in [ColorSpace::Bt601, ColorSpace::Bt709] {
            for range in [ColorRange::Limited, ColorRange::Full] {
                let spec = ColorSpec::new(space, range);
                let source = flat(PixelFormat::Bgra8, 8, 8, 90, 90, 90);
                let mut destination = empty(PixelFormat::Nv12, 8, 8);
                rgb_to_yuv(&source, &mut destination, spec).expect("supported conversion");
                assert!(
                    destination
                        .plane(1)
                        .expect("chroma")
                        .iter()
                        .all(|&s| s == 128),
                    "{spec} tinted a neutral grey"
                );
            }
        }
    }

    #[test]
    fn unsupported_format_pairs_are_reported_not_ignored() {
        let spec = ColorSpec::BT709_LIMITED;
        let source = flat(PixelFormat::Bgra8, 8, 8, 10, 20, 30);

        let mut same_format = empty(PixelFormat::Bgra8, 8, 8);
        assert!(rgb_to_yuv(&source, &mut same_format, spec).is_err());

        let mut luma_only = empty(PixelFormat::Y8, 8, 8);
        assert!(rgb_to_yuv(&source, &mut luma_only, spec).is_err());

        let grey_source = empty(PixelFormat::Y8, 8, 8);
        let mut destination = empty(PixelFormat::I420, 8, 8);
        assert!(rgb_to_yuv(&grey_source, &mut destination, spec).is_err());
    }

    #[test]
    fn mismatched_sizes_are_rejected() {
        let spec = ColorSpec::BT709_LIMITED;
        let source = flat(PixelFormat::Bgra8, 16, 16, 10, 20, 30);
        let mut destination = empty(PixelFormat::I420, 8, 8);
        assert!(rgb_to_yuv(&source, &mut destination, spec).is_err());
    }

    #[test]
    fn the_timestamp_travels_with_the_frame() {
        let spec = ColorSpec::BT709_LIMITED;
        let mut source = flat(PixelFormat::Bgra8, 8, 8, 10, 20, 30);
        source.set_pts(Timestamp::from_millis(1234));
        let mut destination = empty(PixelFormat::I420, 8, 8);
        rgb_to_yuv(&source, &mut destination, spec).expect("supported conversion");
        assert_eq!(destination.pts(), Timestamp::from_millis(1234));
    }
}
