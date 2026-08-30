//! Pixel formats and their plane geometry.
//!
//! The set is deliberately small: every format here has a consumer today or in
//! the next step. libobs declares twenty-five, most of which exist to talk to
//! one specific capture device; adding them speculatively would be dead code to
//! maintain.
//!
//! Plane geometry is *derived* from the format rather than stored alongside it.
//! In libobs, `data[i]` and `linesize[i]` are parallel C arrays that nothing
//! forces to stay consistent with `format`. Here it is impossible to build a
//! frame whose strides disagree with its format.

/// The largest number of planes any supported format uses.
///
/// Three, for [`PixelFormat::I420`]. Widening this is a one-line change.
pub const MAX_PLANES: usize = 3;

/// How pixels are laid out in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8-bit blue, green, red, alpha. One plane, four bytes per pixel.
    ///
    /// The compositing format. OBS's render path uses the same channel order,
    /// and so do the Windows and X11 capture APIs, which avoids a swizzle on
    /// the most common path.
    Bgra8,
    /// 8-bit red, green, blue, alpha. One plane, four bytes per pixel.
    ///
    /// For interoperability: image files, `egui` textures and readable tests.
    Rgba8,
    /// Planar 4:2:0 luma plus interleaved chroma. Two planes.
    ///
    /// What hardware encoders want. Chroma is half resolution on both axes.
    Nv12,
    /// Planar 4:2:0 with separate U and V planes. Three planes.
    ///
    /// What every software encoder accepts.
    I420,
    /// 8-bit luma only. One plane, one byte per pixel.
    ///
    /// Masks, luma keying, and cheap test material.
    Y8,
}

impl PixelFormat {
    /// How many planes this format uses.
    pub const fn plane_count(self) -> usize {
        match self {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Y8 => 1,
            PixelFormat::Nv12 => 2,
            PixelFormat::I420 => 3,
        }
    }

    /// Whether chroma lives in its own plane or planes.
    pub const fn is_planar(self) -> bool {
        self.plane_count() > 1
    }

    /// Whether the format carries an alpha channel.
    pub const fn has_alpha(self) -> bool {
        matches!(self, PixelFormat::Bgra8 | PixelFormat::Rgba8)
    }

    /// Whether both dimensions must be even.
    ///
    /// True for 4:2:0 formats: an odd dimension would leave half a chroma row
    /// or column undefined. That is a classic silent corruption, so it is
    /// rejected at construction instead.
    pub const fn requires_even_size(self) -> bool {
        matches!(self, PixelFormat::Nv12 | PixelFormat::I420)
    }

    /// A short, stable name — for logs, the CLI and config files.
    pub const fn name(self) -> &'static str {
        match self {
            PixelFormat::Bgra8 => "BGRA",
            PixelFormat::Rgba8 => "RGBA",
            PixelFormat::Nv12 => "NV12",
            PixelFormat::I420 => "I420",
            PixelFormat::Y8 => "Y8",
        }
    }

    /// Bytes in one row of `plane`, before any alignment padding.
    ///
    /// This is libobs's `linesize`. Returns `None` when the plane does not
    /// exist for this format.
    pub const fn plane_stride(self, plane: usize, width: u32) -> Option<usize> {
        let width = width as usize;
        let stride = match (self, plane) {
            (PixelFormat::Bgra8 | PixelFormat::Rgba8, 0) => width * 4,
            // One byte per column: luma everywhere, plus NV12's chroma plane,
            // which interleaves U and V and so is as wide as luma even though
            // it covers half the pixels.
            (PixelFormat::Y8 | PixelFormat::I420, 0) | (PixelFormat::Nv12, 0 | 1) => width,
            // I420 keeps U and V apart, at half width each.
            (PixelFormat::I420, 1 | 2) => width.div_ceil(2),
            _ => return None,
        };
        Some(stride)
    }

    /// Number of rows in `plane`.
    ///
    /// Returns `None` when the plane does not exist for this format.
    pub const fn plane_height(self, plane: usize, height: u32) -> Option<u32> {
        let rows = match (self, plane) {
            // Every supported format carries full-height data in plane 0.
            (_, 0) => height,
            // 4:2:0 chroma covers every other row.
            (PixelFormat::Nv12, 1) | (PixelFormat::I420, 1 | 2) => height.div_ceil(2),
            _ => return None,
        };
        Some(rows)
    }
}

impl std::fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PLANES, PixelFormat};

    const ALL: [PixelFormat; 5] = [
        PixelFormat::Bgra8,
        PixelFormat::Rgba8,
        PixelFormat::Nv12,
        PixelFormat::I420,
        PixelFormat::Y8,
    ];

    #[test]
    fn plane_geometry_at_1080p() {
        // Packed formats: one plane, four bytes per pixel, full height.
        assert_eq!(PixelFormat::Bgra8.plane_stride(0, 1920), Some(7680));
        assert_eq!(PixelFormat::Bgra8.plane_height(0, 1080), Some(1080));

        // NV12: full-width chroma row covering half the rows.
        assert_eq!(PixelFormat::Nv12.plane_stride(0, 1920), Some(1920));
        assert_eq!(PixelFormat::Nv12.plane_stride(1, 1920), Some(1920));
        assert_eq!(PixelFormat::Nv12.plane_height(0, 1080), Some(1080));
        assert_eq!(PixelFormat::Nv12.plane_height(1, 1080), Some(540));

        // I420: half-width, half-height chroma in two planes.
        assert_eq!(PixelFormat::I420.plane_stride(0, 1920), Some(1920));
        assert_eq!(PixelFormat::I420.plane_stride(1, 1920), Some(960));
        assert_eq!(PixelFormat::I420.plane_stride(2, 1920), Some(960));
        assert_eq!(PixelFormat::I420.plane_height(1, 1080), Some(540));

        assert_eq!(PixelFormat::Y8.plane_stride(0, 1920), Some(1920));
    }

    #[test]
    fn absent_planes_report_none() {
        for format in ALL {
            let missing = format.plane_count();
            assert_eq!(format.plane_stride(missing, 1920), None);
            assert_eq!(format.plane_height(missing, 1080), None);
            assert_eq!(format.plane_stride(MAX_PLANES + 1, 1920), None);
        }
    }

    #[test]
    fn every_format_describes_all_its_planes() {
        for format in ALL {
            assert!(format.plane_count() <= MAX_PLANES);
            for plane in 0..format.plane_count() {
                assert!(format.plane_stride(plane, 64).is_some());
                assert!(format.plane_height(plane, 64).is_some());
            }
        }
    }

    #[test]
    fn subsampled_formats_demand_even_sizes() {
        assert!(PixelFormat::I420.requires_even_size());
        assert!(PixelFormat::Nv12.requires_even_size());
        assert!(!PixelFormat::Bgra8.requires_even_size());
        assert!(!PixelFormat::Y8.requires_even_size());
    }

    #[test]
    fn only_packed_rgb_formats_carry_alpha() {
        assert!(PixelFormat::Bgra8.has_alpha());
        assert!(PixelFormat::Rgba8.has_alpha());
        assert!(!PixelFormat::I420.has_alpha());
        assert!(!PixelFormat::Y8.has_alpha());
    }

    #[test]
    fn names_are_stable_and_unique() {
        let mut names: Vec<&str> = ALL.iter().map(|format| format.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
        assert_eq!(PixelFormat::I420.to_string(), "I420");
    }
}
