//! Reading back what we wrote.
//!
//! The header tests in the crate pin the text; this pins the pixels. A Y4M file
//! is raw and lossless, so the planes that come back out have to be the exact
//! bytes the converter produced — no stride padding, no plane reordering, no
//! alignment leaking into the stream.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use voltra_core::{ColorSpec, Fps, FrameSize, PixelFormat, Timestamp, VideoFrame, rgb_to_yuv};
use voltra_output::{Y4mParams, Y4mWriter};

/// The smallest Y4M reader that can check our writer: header, then frames.
struct Reader<'a> {
    header: String,
    body: &'a [u8],
    frame_bytes: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], width: usize, height: usize) -> Self {
        let end = bytes.iter().position(|&b| b == b'\n').expect("a header");
        Self {
            header: String::from_utf8(bytes[..end].to_vec()).expect("ASCII header"),
            body: &bytes[end + 1..],
            frame_bytes: width * height * 3 / 2,
        }
    }

    /// The payload of frame `index`, checking its separator on the way.
    fn frame(&self, index: usize) -> &'a [u8] {
        let stride = b"FRAME\n".len() + self.frame_bytes;
        let start = index * stride;
        assert_eq!(&self.body[start..start + 6], b"FRAME\n", "frame {index}");
        &self.body[start + 6..start + stride]
    }
}

/// A canvas with a different value in every channel of every region, so a
/// swapped plane or a shifted row cannot pass unnoticed.
fn painted_canvas(size: FrameSize) -> VideoFrame {
    let mut frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).unwrap();
    let width = size.width();
    for (y, row) in frame.rows_mut(0).unwrap().enumerate() {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let x = u32::try_from(x).unwrap();
            let y = u32::try_from(y).unwrap();
            // Stored BGRA: blue, green, red, alpha.
            pixel.copy_from_slice(&[
                u8::try_from(x * 255 / width).unwrap(),
                u8::try_from((x ^ y) & 0xFF).unwrap(),
                u8::try_from(y & 0xFF).unwrap(),
                255,
            ]);
        }
    }
    frame
}

#[test]
fn what_goes_in_comes_back_out_byte_for_byte() {
    let size = FrameSize::new(96, 64).unwrap();
    let canvas = painted_canvas(size);

    let mut converted = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).unwrap();
    rgb_to_yuv(&canvas, &mut converted, ColorSpec::BT709_LIMITED).unwrap();

    let mut writer = Y4mWriter::new(
        Vec::new(),
        PixelFormat::I420,
        size,
        Y4mParams::at(Fps::FPS_30),
    )
    .unwrap();
    writer.write_frame(&converted).unwrap();
    writer.write_frame(&converted).unwrap();
    let bytes = writer.finish().unwrap();

    let reader = Reader::new(&bytes, 96, 64);
    assert!(reader.header.contains("W96 H64"));

    // The planes, concatenated with no padding between them. Note this is not
    // `converted.as_bytes()`: the frame pads each plane to a 32-byte boundary
    // and that padding must not reach the file.
    let expected: Vec<u8> = (0..converted.plane_count())
        .flat_map(|index| converted.plane(index).unwrap().to_vec())
        .collect();

    for index in 0..2 {
        assert_eq!(reader.frame(index), expected.as_slice(), "frame {index}");
    }
}

/// The luma plane comes first and the chroma planes follow in `Cb`, `Cr` order.
/// Swapping the last two turns skin tones blue, which is the classic first-file
/// bug.
#[test]
fn the_planes_arrive_in_y_cb_cr_order() {
    let size = FrameSize::new(32, 32).unwrap();
    let mut canvas = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).unwrap();
    // Pure red, stored BGRA.
    for row in canvas.rows_mut(0).unwrap() {
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
    }

    let mut converted = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).unwrap();
    rgb_to_yuv(&canvas, &mut converted, ColorSpec::BT709_LIMITED).unwrap();

    let mut writer =
        Y4mWriter::new(Vec::new(), PixelFormat::I420, size, Y4mParams::default()).unwrap();
    writer.write_frame(&converted).unwrap();
    let bytes = writer.finish().unwrap();

    let payload = Reader::new(&bytes, 32, 32).frame(0);
    let luma = &payload[..32 * 32];
    let cb = &payload[32 * 32..32 * 32 + 16 * 16];
    let cr = &payload[32 * 32 + 16 * 16..];

    // Red under BT.709 limited: dark luma, Cb well below neutral, Cr well above.
    assert!(luma[0] < 90, "luma was {}", luma[0]);
    assert!(cb[0] < 128, "Cb was {}", cb[0]);
    assert!(cr[0] > 128, "Cr was {}", cr[0]);
}
