//! From a system capture buffer's pixels to a `Bgra8` frame: the one copy a
//! shared-memory capture costs (DMA-BUF, to come, avoids it).

use voltra_core::VideoFrame;

/// The 8-bit, four-byte formats a capture can arrive in, as PipeWire names
/// them; Windows and macOS hand over the same BGRA. OBS offers the same four (`docs/references/capture.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceFormat {
    /// Blue, green, red, padding.
    Bgrx,
    /// Blue, green, red, alpha.
    Bgra,
    /// Red, green, blue, padding.
    Rgbx,
    /// Red, green, blue, alpha.
    Rgba,
}

/// Copy `source` — rows of `stride` bytes, the first `width × 4` of each
/// meaningful — into `frame`, a `Bgra8` frame of the same size, converting as
/// needed. Padding becomes opaque alpha. Rows the source does not have are
/// left as they are.
///
/// The copy a shared-memory capture cannot avoid: the system's buffer goes
/// back to it as soon as this returns.
pub fn copy_to_bgra(source: &[u8], stride: usize, format: SourceFormat, frame: &mut VideoFrame) {
    let Some(rows) = frame.rows_mut(0) else {
        return;
    };
    for (out, row) in rows.zip(source.chunks(stride.max(1))) {
        let len = out.len().min(row.len());
        let (out, row) = (&mut out[..len], &row[..len]);
        match format {
            SourceFormat::Bgra => out.copy_from_slice(row),
            SourceFormat::Bgrx => {
                for (to, from) in out.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    to.copy_from_slice(&[from[0], from[1], from[2], 0xFF]);
                }
            }
            SourceFormat::Rgba => {
                for (to, from) in out.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    to.copy_from_slice(&[from[2], from[1], from[0], from[3]]);
                }
            }
            SourceFormat::Rgbx => {
                for (to, from) in out.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    to.copy_from_slice(&[from[2], from[1], from[0], 0xFF]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::{FrameSize, PixelFormat, Timestamp, VideoFrame};

    use super::{SourceFormat, copy_to_bgra};

    /// Two pixels a row, two rows, with a stride of 12: four bytes of padding
    /// at the end of each row that must not reach the frame.
    const SOURCE: [u8; 24] = [
        1, 2, 3, 4, 5, 6, 7, 8, 0xEE, 0xEE, 0xEE, 0xEE, //
        9, 10, 11, 12, 13, 14, 15, 16, 0xEE, 0xEE, 0xEE, 0xEE,
    ];

    fn convert(format: SourceFormat) -> Vec<u8> {
        let size = FrameSize::new(2, 2).unwrap();
        let mut frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).unwrap();
        copy_to_bgra(&SOURCE, 12, format, &mut frame);
        frame.rows(0).unwrap().flatten().copied().collect()
    }

    #[test]
    fn every_format_lands_as_bgra_without_the_row_padding() {
        assert_eq!(
            convert(SourceFormat::Bgra),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            convert(SourceFormat::Bgrx),
            [1, 2, 3, 255, 5, 6, 7, 255, 9, 10, 11, 255, 13, 14, 15, 255]
        );
        assert_eq!(
            convert(SourceFormat::Rgba),
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
        assert_eq!(
            convert(SourceFormat::Rgbx),
            [3, 2, 1, 255, 7, 6, 5, 255, 11, 10, 9, 255, 15, 14, 13, 255]
        );
    }
}
