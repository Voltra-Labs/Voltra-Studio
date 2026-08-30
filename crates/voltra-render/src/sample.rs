//! Reading a pixel out of a source frame at a fractional position.
//!
//! The compositor maps each destination pixel back into source space
//! (`Placement::inverse` from plan 002), which lands between texels whenever the
//! item is scaled or rotated. These are the two ways of resolving that.
//!
//! # Fixed point, not floating point
//!
//! Coordinates arrive as 16.16 fixed point. The first version of this module
//! carried them as `f32` and converted per texel, which cost 110 ms per 1080p
//! frame: a float-to-integer `as` cast in Rust saturates, so it compiles to a
//! compare-and-select sequence rather than one instruction, and bilinear
//! sampling does sixteen of them per pixel. Integers also step exactly — adding
//! a constant per pixel accumulates no error at all, which floats cannot
//! promise.
//!
//! Like the blend modes, each filter is a type, so the choice is made once per
//! item instead of once per pixel.

/// Fractional bits in the fixed-point coordinates.
pub(crate) const FRACTION_BITS: u32 = 16;

/// One whole pixel, in fixed point.
pub(crate) const ONE: i64 = 1 << FRACTION_BITS;

/// Half a pixel, in fixed point.
const HALF: i64 = ONE / 2;

/// Convert a coordinate to fixed point. Called once per row, never per pixel.
pub(crate) fn to_fixed(value: f32) -> i64 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "canvas coordinates are far inside i64 even scaled by 65536"
    )]
    {
        (value * 65_536.0) as i64
    }
}

/// A borrowed view of a source frame's pixels, with its clamping bounds already
/// resolved to integers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceView<'a> {
    /// The packed 8-bit-per-channel pixels.
    pub pixels: &'a [u8],
    /// Bytes per row.
    pub stride: usize,
    /// Leftmost readable column.
    pub min_x: i32,
    /// Topmost readable row.
    pub min_y: i32,
    /// Rightmost readable column, inclusive.
    pub max_x: i32,
    /// Bottom readable row, inclusive.
    pub max_y: i32,
    /// Byte offset of the red channel within a pixel.
    pub red: usize,
    /// Byte offset of the blue channel within a pixel.
    pub blue: usize,
}

impl SourceView<'_> {
    /// One texel as `[blue, green, red, alpha]`, clamped into the cropped
    /// region.
    ///
    /// Clamping rather than wrapping or returning transparent: an edge texel
    /// repeated is what every graphics API calls clamp-to-edge, and it keeps a
    /// scaled item's border from picking up whatever sits next to it in memory.
    #[inline]
    fn texel(self, x: i32, y: i32) -> [u8; 4] {
        let x = x.clamp(self.min_x, self.max_x);
        let y = y.clamp(self.min_y, self.max_y);

        #[allow(
            clippy::cast_sign_loss,
            reason = "the clamp bounds are non-negative by construction"
        )]
        let offset = (y as usize) * self.stride + (x as usize) * 4;

        match self.pixels.get(offset..offset + 4) {
            Some(pixel) => [pixel[self.blue], pixel[1], pixel[self.red], pixel[3]],
            // Unreachable for a well-formed frame; returning transparent beats
            // panicking on the render thread.
            None => [0, 0, 0, 0],
        }
    }
}

/// A way of reading a source pixel at a fractional coordinate.
pub(crate) trait Sampler {
    /// Sample at a 16.16 fixed-point position, returning `[b, g, r, a]`.
    fn sample(view: SourceView<'_>, x: i64, y: i64) -> [u8; 4];
}

/// Nearest neighbour: sharp and aliased. Right for pixel art, and exact at 1:1.
pub(crate) struct Point;

impl Sampler for Point {
    #[inline]
    fn sample(view: SourceView<'_>, x: i64, y: i64) -> [u8; 4] {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the whole part of a coordinate inside the frame fits i32"
        )]
        view.texel((x >> FRACTION_BITS) as i32, (y >> FRACTION_BITS) as i32)
    }
}

/// Bilinear: four texels weighted by distance.
pub(crate) struct Bilinear;

impl Sampler for Bilinear {
    #[inline]
    fn sample(view: SourceView<'_>, x: i64, y: i64) -> [u8; 4] {
        // Texel centres sit at half-pixel offsets, so shift before flooring or
        // the whole image drifts half a pixel. An arithmetic shift right is a
        // floor for negatives too, which is what this needs.
        let fx = x - HALF;
        let fy = y - HALF;

        #[allow(
            clippy::cast_possible_truncation,
            reason = "coordinates are bounded by the frame size"
        )]
        let (ix, iy) = ((fx >> FRACTION_BITS) as i32, (fy >> FRACTION_BITS) as i32);

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "masked to eight bits"
        )]
        let (tx, ty) = (((fx >> 8) & 0xFF) as u32, ((fy >> 8) & 0xFF) as u32);

        let top_left = view.texel(ix, iy);
        let top_right = view.texel(ix + 1, iy);
        let bottom_left = view.texel(ix, iy + 1);
        let bottom_right = view.texel(ix + 1, iy + 1);

        let mut out = [0u8; 4];
        for channel in 0..4 {
            let top = lerp(top_left[channel], top_right[channel], tx);
            let bottom = lerp(bottom_left[channel], bottom_right[channel], tx);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "interpolating two 8-bit values stays within 0..=255"
            )]
            {
                out[channel] = ((top * (256 - ty) + bottom * ty) >> 8) as u8;
            }
        }
        out
    }
}

/// Interpolate two 8-bit values with an 8-bit weight.
#[inline]
const fn lerp(a: u8, b: u8, weight: u32) -> u32 {
    let a = a as u32;
    let b = b as u32;
    (a * (256 - weight) + b * weight) >> 8
}
