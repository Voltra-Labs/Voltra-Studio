//! Bringing the finished canvas back to system memory.
//!
//! # The 256-byte rule
//!
//! `copy_texture_to_buffer` requires `bytes_per_row` to be a multiple of
//! [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] (256). A 1920-pixel BGRA row is 7680
//! bytes, which happens to be 30×256, so the mistake hides at 1080p and shows up
//! the moment somebody renders 100 pixels wide: every row lands 144 bytes late
//! and the image comes out sheared.
//!
//! It is the same class of bug as writing `stride` instead of `row_bytes` into a
//! Y4M file (plan 009), in the other direction: padding that belongs to the
//! transport must never reach the pixels.

use voltra_core::{Error, Result, VideoFrame};

use super::context::GpuContext;

/// Round `bytes` up to the alignment `copy_texture_to_buffer` demands.
pub(crate) const fn aligned_bytes_per_row(bytes: u32) -> u32 {
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes.div_ceil(alignment) * alignment
}

/// Copy `texture` into `frame`, dropping the alignment padding on the way.
///
/// # Errors
///
/// [`Error::Unsupported`] when the frame has no pixel plane, and
/// [`Error::Config`] when the mapping fails or the sizes disagree.
pub(crate) fn read_texture_into(
    context: &GpuContext,
    texture: &wgpu::Texture,
    staging: &wgpu::Buffer,
    frame: &mut VideoFrame,
) -> Result<()> {
    let width = frame.width();
    let height = frame.height();
    let unpadded = width * 4;
    let padded = aligned_bytes_per_row(unpadded);

    let mut encoder = context
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("voltra readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    context.queue().submit(Some(encoder.finish()));

    let slice = staging.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        // The receiver is alive until this function returns, but a send that
        // fails is not worth failing the frame over: the poll below decides.
        let _ = sender.send(result);
    });
    context
        .device()
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|error| Error::config(format!("waiting for the GPU failed: {error}")))?;
    match receiver.recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(Error::config(format!(
                "mapping the readback buffer: {error}"
            )));
        }
        Err(error) => {
            return Err(Error::config(format!("the GPU never answered: {error}")));
        }
    }

    {
        let mapped = slice
            .get_mapped_range()
            .map_err(|error| Error::config(format!("reading back the canvas: {error}")))?;
        let Some(rows) = frame.rows_mut(0) else {
            drop(mapped);
            staging.unmap();
            return Err(Error::unsupported("the target frame has no pixel plane"));
        };
        let unpadded = unpadded as usize;
        let padded = padded as usize;
        for (index, row) in rows.enumerate() {
            let start = index * padded;
            // Only the meaningful bytes; the alignment padding stays behind.
            row[..unpadded].copy_from_slice(&mapped[start..start + unpadded]);
        }
    }
    staging.unmap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::aligned_bytes_per_row;

    /// 1080p hides the bug: its rows are already aligned. Everything else is
    /// where a missing pad shows up.
    #[test]
    fn rows_are_padded_to_the_copy_alignment() {
        // 1920 * 4 = 7680 = 30 * 256, already aligned.
        assert_eq!(aligned_bytes_per_row(1920 * 4), 7680);
        // 100 * 4 = 400, which is not.
        assert_eq!(aligned_bytes_per_row(100 * 4), 512);
        // 64 pixels is exactly one alignment unit.
        assert_eq!(aligned_bytes_per_row(64 * 4), 256);
        assert_eq!(aligned_bytes_per_row(65 * 4), 512);
    }

    #[test]
    fn an_aligned_row_is_left_alone() {
        for pixels in [64u32, 128, 192, 256, 1280, 1920] {
            let bytes = pixels * 4;
            assert_eq!(aligned_bytes_per_row(bytes), bytes, "{pixels} px");
        }
    }
}
