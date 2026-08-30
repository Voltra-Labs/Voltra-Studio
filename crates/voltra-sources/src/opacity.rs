//! Scaling a frame's alpha channel.

use voltra_core::{
    Error, PixelFormat, Result, Source, SourceCapabilities, SourceType, VideoFilter, VideoFrame,
};

/// Multiplies every pixel's alpha by a constant.
///
/// The simplest filter that does real work, and the mechanism behind a fade:
/// a transition drives this from 1.0 to 0.0 over its duration.
///
/// Works in place and touches only the alpha byte — the colour channels come
/// out untouched, so the result stays correct under the straight-alpha blending
/// the compositor will do.
///
/// # Examples
///
/// ```
/// use voltra_core::{FrameSize, PixelFormat, Timestamp, VideoFilter, VideoFrame};
/// use voltra_sources::OpacityFilter;
///
/// let size = FrameSize::new(2, 2)?;
/// let mut frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO)?;
/// frame.as_bytes_mut().fill(0xFF);
///
/// let mut filter = OpacityFilter::new(0.5);
/// filter.filter(&mut frame)?;
///
/// // Colour untouched, alpha halved.
/// assert_eq!(&frame.plane(0).expect("pixels")[..4], &[255, 255, 255, 128]);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct OpacityFilter {
    /// Fixed-point opacity in Q8: 256 means fully opaque.
    scale: u32,
}

/// Fractional bits of the fixed-point opacity.
const FRACTION_BITS: u32 = 8;

/// Fully opaque, in fixed point.
const OPAQUE: u32 = 1 << FRACTION_BITS;

/// [`OPAQUE`] as a float, for converting to and from the public 0.0..=1.0 form.
const OPAQUE_F32: f32 = 256.0;

impl OpacityFilter {
    /// The stable type identifier.
    pub const KIND: &'static str = "opacity";

    /// Build a filter with `opacity` from 0.0 to 1.0, clamped.
    #[must_use]
    pub fn new(opacity: f32) -> Self {
        let mut filter = Self { scale: OPAQUE };
        filter.set_opacity(opacity);
        filter
    }

    /// The current opacity, from 0.0 to 1.0.
    #[must_use]
    pub fn opacity(self) -> f32 {
        // Exact: the numerator is a small integer.
        #[allow(
            clippy::cast_precision_loss,
            reason = "the scale is at most 256, exactly representable in f32"
        )]
        {
            self.scale as f32 / OPAQUE_F32
        }
    }

    /// Set the opacity, clamping to 0.0..=1.0.
    ///
    /// Out-of-range values are clamped rather than rejected: a transition
    /// driving this from an animation curve may overshoot slightly, and failing
    /// a frame over that would be worse than saturating.
    pub fn set_opacity(&mut self, opacity: f32) {
        let clamped = opacity.clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=1.0 then scaled, so the result is 0..=256"
        )]
        {
            self.scale = (clamped * OPAQUE_F32).round() as u32;
        }
    }
}

impl Source for OpacityFilter {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn source_type(&self) -> SourceType {
        SourceType::Filter
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::VIDEO
    }
}

impl VideoFilter for OpacityFilter {
    fn filter(&mut self, frame: &mut VideoFrame) -> Result<()> {
        // Alpha lives at a different offset per format, and formats without it
        // cannot express the operation at all.
        let alpha_offset = match frame.format() {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => 3,
            other => {
                return Err(Error::unsupported(format!(
                    "opacity needs an alpha channel, {other} has none"
                )));
            }
        };

        if self.scale == OPAQUE {
            return Ok(());
        }

        let scale = self.scale;
        let Some(rows) = frame.rows_mut(0) else {
            return Ok(());
        };
        for row in rows {
            for pixel in row.chunks_exact_mut(4) {
                let alpha = u32::from(pixel[alpha_offset]);
                // Rounding to nearest keeps a full fade symmetric.
                let scaled = (alpha * scale + (OPAQUE / 2)) >> FRACTION_BITS;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "alpha <= 255 and scale <= 256, so the result is at most 255"
                )]
                {
                    pixel[alpha_offset] = scaled.min(255) as u8;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::OpacityFilter;
    use voltra_core::{
        FrameSize, PixelFormat, Source, SourceType, Timestamp, VideoFilter, VideoFrame,
    };

    fn frame(format: PixelFormat, fill: u8) -> VideoFrame {
        let size = FrameSize::new(4, 4).expect("valid size");
        let mut frame = VideoFrame::new(format, size, Timestamp::ZERO).expect("frame");
        frame.as_bytes_mut().fill(fill);
        frame
    }

    #[test]
    fn full_opacity_changes_nothing() {
        let mut original = frame(PixelFormat::Bgra8, 0xC0);
        let expected = original.as_bytes().to_vec();

        OpacityFilter::new(1.0)
            .filter(&mut original)
            .expect("supported format");
        assert_eq!(original.as_bytes(), expected.as_slice());
    }

    #[test]
    fn zero_opacity_clears_alpha_but_keeps_colour() {
        let mut target = frame(PixelFormat::Bgra8, 0xFF);
        OpacityFilter::new(0.0)
            .filter(&mut target)
            .expect("supported format");

        for pixel in target.plane(0).expect("pixels").chunks_exact(4) {
            assert_eq!(&pixel[..3], &[0xFF, 0xFF, 0xFF], "colour was modified");
            assert_eq!(pixel[3], 0, "alpha should be cleared");
        }
    }

    #[test]
    fn half_opacity_halves_alpha() {
        let mut target = frame(PixelFormat::Bgra8, 0xFF);
        OpacityFilter::new(0.5)
            .filter(&mut target)
            .expect("supported format");
        assert!(
            target
                .plane(0)
                .expect("pixels")
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 128)
        );
    }

    #[test]
    fn rgba_alpha_sits_in_the_same_place() {
        let mut target = frame(PixelFormat::Rgba8, 0xFF);
        OpacityFilter::new(0.25)
            .filter(&mut target)
            .expect("supported format");
        assert!(
            target
                .plane(0)
                .expect("pixels")
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 64 && pixel[0] == 0xFF)
        );
    }

    #[test]
    fn formats_without_alpha_are_refused_not_corrupted() {
        for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Y8] {
            let mut target = frame(format, 0x80);
            let before = target.as_bytes().to_vec();
            assert!(
                OpacityFilter::new(0.5).filter(&mut target).is_err(),
                "{format} should be refused"
            );
            assert_eq!(
                target.as_bytes(),
                before.as_slice(),
                "{format} was modified despite the error"
            );
        }
    }

    #[test]
    fn opacity_is_clamped_rather_than_rejected() {
        let mut filter = OpacityFilter::new(4.0);
        assert!((filter.opacity() - 1.0).abs() < f32::EPSILON);
        filter.set_opacity(-2.0);
        assert!((filter.opacity() - 0.0).abs() < f32::EPSILON);
    }

    /// The float form of the fixed-point unit is written separately to keep
    /// clippy's precision lint quiet; this makes sure the two never drift.
    #[test]
    fn the_two_forms_of_the_fixed_point_unit_agree() {
        assert!((f64::from(super::OPAQUE) - f64::from(super::OPAQUE_F32)).abs() < f64::EPSILON);
    }

    #[test]
    fn declares_itself_as_a_filter() {
        let filter = OpacityFilter::new(1.0);
        assert_eq!(filter.kind(), "opacity");
        assert_eq!(filter.source_type(), SourceType::Filter);
    }
}
