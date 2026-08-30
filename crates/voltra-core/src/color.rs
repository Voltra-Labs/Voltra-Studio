//! Colour spaces, ranges, and the coefficients that convert between them.
//!
//! libobs derives every matrix from two numbers — `Kb` and `Kr` — in
//! `initialize_matrix()`, rather than tabulating magic constants. This module
//! does the same, at compile time, so the numbers cannot drift from the standard
//! they claim to implement. A test checks each derived integer against the
//! floating-point formula.
//!
//! Space and range travel together in [`ColorSpec`] because separating them is
//! how streams end up subtly wrong: converting as BT.601 and decoding as BT.709,
//! or tagging full-range content as limited, shifts every colour slightly and
//! nobody notices until the recording is published.

/// The luma coefficients a colour space is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorSpace {
    /// BT.601, standard definition. `Kb = 0.114`, `Kr = 0.299`.
    Bt601,
    /// BT.709, high definition. `Kb = 0.0722`, `Kr = 0.2126`.
    ///
    /// The default, matching OBS.
    #[default]
    Bt709,
}

impl ColorSpace {
    /// The blue and red luma coefficients, `(Kb, Kr)`.
    const fn luma_coefficients(self) -> (f64, f64) {
        match self {
            ColorSpace::Bt601 => (0.114, 0.299),
            ColorSpace::Bt709 => (0.0722, 0.2126),
        }
    }

    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            ColorSpace::Bt601 => "BT.601",
            ColorSpace::Bt709 => "BT.709",
        }
    }
}

/// How the 8-bit code values map onto the signal range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// Luma 16–235, chroma 16–240. Broadcast convention, and the default.
    #[default]
    Limited,
    /// The whole 0–255 range.
    Full,
}

impl ColorRange {
    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            ColorRange::Limited => "limited",
            ColorRange::Full => "full",
        }
    }

    /// `(luma span, chroma span, black level)` in 8-bit code values.
    const fn spans(self) -> (f64, f64, i32) {
        match self {
            // 235 - 16 = 219 luma, 240 - 16 = 224 chroma.
            ColorRange::Limited => (219.0, 224.0, 16),
            ColorRange::Full => (255.0, 255.0, 0),
        }
    }
}

/// A colour space together with its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ColorSpec {
    /// The luma coefficients in use.
    pub space: ColorSpace,
    /// How code values map onto the signal range.
    pub range: ColorRange,
}

impl ColorSpec {
    /// Build a specification.
    pub const fn new(space: ColorSpace, range: ColorRange) -> Self {
        Self { space, range }
    }

    /// BT.709 limited — what OBS defaults to, and what players expect.
    pub const BT709_LIMITED: ColorSpec = ColorSpec::new(ColorSpace::Bt709, ColorRange::Limited);

    /// The integer coefficients for converting RGB to YCbCr under this spec.
    pub const fn rgb_to_yuv(self) -> RgbToYuv {
        RgbToYuv::derive(self)
    }
}

impl std::fmt::Display for ColorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.space.name(), self.range.name())
    }
}

/// Fractional bits in the fixed-point coefficients.
///
/// Q16 leaves plenty of headroom: the largest product is about
/// `0.72 × 255 × 65536 ≈ 1.2e7`, well inside `i32`, and the rounding error stays
/// below half an 8-bit code value.
pub(crate) const FRACTION_BITS: u32 = 16;

/// Added before the shift so truncation rounds to nearest.
pub(crate) const ROUNDING: i32 = 1 << (FRACTION_BITS - 1);

/// Fixed-point RGB to YCbCr coefficients, derived from a [`ColorSpec`].
///
/// Each channel follows the same shape:
///
/// ```text
/// Y  = ((y_red·R + y_green·G + y_blue·B + ½) >> 16) + luma_offset
/// Cb = ((u_red·R + u_green·G + u_blue·B + ½) >> 16) + 128
/// Cr = ((v_red·R + v_green·G + v_blue·B + ½) >> 16) + 128
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbToYuv {
    /// Red term of the luma equation.
    pub y_red: i32,
    /// Green term of the luma equation.
    pub y_green: i32,
    /// Blue term of the luma equation.
    pub y_blue: i32,
    /// Red term of the Cb equation.
    pub u_red: i32,
    /// Green term of the Cb equation.
    pub u_green: i32,
    /// Blue term of the Cb equation.
    pub u_blue: i32,
    /// Red term of the Cr equation.
    pub v_red: i32,
    /// Green term of the Cr equation.
    pub v_green: i32,
    /// Blue term of the Cr equation.
    pub v_blue: i32,
    /// Black level added to luma: 16 for limited range, 0 for full.
    pub luma_offset: i32,
}

/// Scale a normalised coefficient into fixed point.
#[allow(
    clippy::cast_possible_truncation,
    reason = "coefficients are bounded by ±1, so the scaled value fits i32 with three orders of magnitude to spare"
)]
const fn to_fixed(value: f64) -> i32 {
    let scaled = value * (1u32 << FRACTION_BITS) as f64;
    // `round` is not available in const context; add half a unit and truncate.
    if scaled < 0.0 {
        (scaled - 0.5) as i32
    } else {
        (scaled + 0.5) as i32
    }
}

impl RgbToYuv {
    /// Derive the coefficients from first principles.
    ///
    /// With `Kg = 1 - Kr - Kb`, luma is `Kr·R + Kg·G + Kb·B` scaled to the
    /// range's luma span, and the chroma differences are `B - Y` and `R - Y`
    /// scaled so that they span the range's chroma span:
    ///
    /// ```text
    /// Cb - 128 = (chroma_span / 255) / (2·(1 - Kb)) · (B - Y)
    /// Cr - 128 = (chroma_span / 255) / (2·(1 - Kr)) · (R - Y)
    /// ```
    pub const fn derive(spec: ColorSpec) -> Self {
        let (kb, kr) = spec.space.luma_coefficients();
        let kg = 1.0 - kr - kb;
        let (luma_span, chroma_span, luma_offset) = spec.range.spans();

        let luma_scale = luma_span / 255.0;
        let blue_scale = (chroma_span / 255.0) / (2.0 * (1.0 - kb));
        let red_scale = (chroma_span / 255.0) / (2.0 * (1.0 - kr));

        Self {
            y_red: to_fixed(kr * luma_scale),
            y_green: to_fixed(kg * luma_scale),
            y_blue: to_fixed(kb * luma_scale),

            // Cb ∝ B - Y, expanded through Y = Kr·R + Kg·G + Kb·B.
            u_red: to_fixed(-kr * blue_scale),
            u_green: to_fixed(-kg * blue_scale),
            u_blue: to_fixed((1.0 - kb) * blue_scale),

            // Cr ∝ R - Y, expanded the same way.
            v_red: to_fixed((1.0 - kr) * red_scale),
            v_green: to_fixed(-kg * red_scale),
            v_blue: to_fixed(-kb * red_scale),

            luma_offset,
        }
    }

    /// Luma for one pixel, already clamped to 8 bits.
    #[inline]
    pub fn luma(&self, red: i32, green: i32, blue: i32) -> u8 {
        let value = (self.y_red * red + self.y_green * green + self.y_blue * blue + ROUNDING)
            >> FRACTION_BITS;
        clamp_u8(value + self.luma_offset)
    }

    /// The chroma pair for one pixel, already clamped to 8 bits.
    ///
    /// Callers working in 4:2:0 pass the average of a 2×2 block.
    #[inline]
    pub fn chroma(&self, red: i32, green: i32, blue: i32) -> (u8, u8) {
        let blue_difference =
            (self.u_red * red + self.u_green * green + self.u_blue * blue + ROUNDING)
                >> FRACTION_BITS;
        let red_difference =
            (self.v_red * red + self.v_green * green + self.v_blue * blue + ROUNDING)
                >> FRACTION_BITS;
        (
            clamp_u8(blue_difference + 128),
            clamp_u8(red_difference + 128),
        )
    }
}

/// Clamp to the 8-bit range.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamp above proves the value is within 0..=255"
)]
pub(crate) fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::{ColorRange, ColorSpace, ColorSpec, FRACTION_BITS, RgbToYuv, clamp_u8};

    const SPECS: [ColorSpec; 4] = [
        ColorSpec::new(ColorSpace::Bt601, ColorRange::Limited),
        ColorSpec::new(ColorSpace::Bt601, ColorRange::Full),
        ColorSpec::new(ColorSpace::Bt709, ColorRange::Limited),
        ColorSpec::new(ColorSpace::Bt709, ColorRange::Full),
    ];

    /// Reference implementation in floating point, straight from the standard.
    fn reference(spec: ColorSpec, red: f64, green: f64, blue: f64) -> (f64, f64, f64) {
        let (kb, kr) = match spec.space {
            ColorSpace::Bt601 => (0.114, 0.299),
            ColorSpace::Bt709 => (0.0722, 0.2126),
        };
        let kg = 1.0 - kr - kb;
        let (luma_span, chroma_span, offset) = match spec.range {
            ColorRange::Limited => (219.0, 224.0, 16.0),
            ColorRange::Full => (255.0, 255.0, 0.0),
        };

        let luma = kr * red + kg * green + kb * blue;
        let y = luma * (luma_span / 255.0) + offset;
        let u = (blue - luma) * (chroma_span / 255.0) / (2.0 * (1.0 - kb)) + 128.0;
        let v = (red - luma) * (chroma_span / 255.0) / (2.0 * (1.0 - kr)) + 128.0;
        (y, u, v)
    }

    /// The integer path must agree with the standard's own formula. Without
    /// this, the fixed-point constants are just numbers somebody typed.
    #[test]
    fn fixed_point_matches_the_floating_point_standard() {
        for spec in SPECS {
            let coefficients = spec.rgb_to_yuv();
            for red in (0..=255).step_by(17) {
                for green in (0..=255).step_by(17) {
                    for blue in (0..=255).step_by(17) {
                        let (want_y, want_u, want_v) =
                            reference(spec, f64::from(red), f64::from(green), f64::from(blue));
                        let got_y = coefficients.luma(red, green, blue);
                        let (got_u, got_v) = coefficients.chroma(red, green, blue);

                        for (got, want, channel) in [
                            (got_y, want_y, 'Y'),
                            (got_u, want_u, 'U'),
                            (got_v, want_v, 'V'),
                        ] {
                            let error = (f64::from(got) - want).abs();
                            assert!(
                                error <= 1.0,
                                "{spec} {channel} at rgb({red},{green},{blue}): \
                                 got {got}, standard says {want:.3}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn limited_range_puts_black_at_16_and_white_at_235() {
        for space in [ColorSpace::Bt601, ColorSpace::Bt709] {
            let limited = ColorSpec::new(space, ColorRange::Limited).rgb_to_yuv();
            assert_eq!(limited.luma(0, 0, 0), 16, "{} black", space.name());
            assert_eq!(limited.luma(255, 255, 255), 235, "{} white", space.name());

            let full = ColorSpec::new(space, ColorRange::Full).rgb_to_yuv();
            assert_eq!(full.luma(0, 0, 0), 0, "{} black", space.name());
            assert_eq!(full.luma(255, 255, 255), 255, "{} white", space.name());
        }
    }

    /// Any neutral grey must be colourless. A drift here is a colour cast on
    /// every frame the studio ever produces.
    #[test]
    fn greys_carry_no_chroma() {
        for spec in SPECS {
            let coefficients = spec.rgb_to_yuv();
            for level in [0, 16, 64, 128, 192, 255] {
                let (u, v) = coefficients.chroma(level, level, level);
                assert_eq!((u, v), (128, 128), "{spec} at grey {level}");
            }
        }
    }

    /// Primaries must land on the extremes of the chroma range, which is the
    /// property that catches a swapped Cb/Cr.
    #[test]
    fn primaries_push_chroma_the_right_way() {
        let coefficients = ColorSpec::BT709_LIMITED.rgb_to_yuv();

        let (blue_u, blue_v) = coefficients.chroma(0, 0, 255);
        assert!(blue_u > 200, "blue should maximise Cb, got {blue_u}");
        assert!(blue_v < 128, "blue should lower Cr, got {blue_v}");

        let (red_u, red_v) = coefficients.chroma(255, 0, 0);
        assert!(red_v > 200, "red should maximise Cr, got {red_v}");
        assert!(red_u < 128, "red should lower Cb, got {red_u}");
    }

    #[test]
    fn coefficients_sum_to_the_expected_totals() {
        let unit = 1 << FRACTION_BITS;
        for spec in SPECS {
            let c = spec.rgb_to_yuv();
            // Luma terms span the range's luma span out of 255.
            let luma_total = c.y_red + c.y_green + c.y_blue;
            let expected = match spec.range {
                ColorRange::Limited => (219 * unit) / 255,
                ColorRange::Full => unit,
            };
            assert!(
                (luma_total - expected).abs() <= 2,
                "{spec} luma terms sum to {luma_total}, expected about {expected}"
            );
            // Chroma terms cancel out, or a neutral grey would not be neutral.
            assert!((c.u_red + c.u_green + c.u_blue).abs() <= 2);
            assert!((c.v_red + c.v_green + c.v_blue).abs() <= 2);
        }
    }

    #[test]
    fn clamping_saturates_instead_of_wrapping() {
        assert_eq!(clamp_u8(-30), 0);
        assert_eq!(clamp_u8(300), 255);
        assert_eq!(clamp_u8(128), 128);
    }

    #[test]
    fn defaults_match_obs() {
        assert_eq!(ColorSpec::default(), ColorSpec::BT709_LIMITED);
        assert_eq!(ColorSpec::default().to_string(), "BT.709 limited");
    }

    #[test]
    fn derivation_is_available_in_const_context() {
        const COEFFICIENTS: RgbToYuv = ColorSpec::BT709_LIMITED.rgb_to_yuv();
        assert_eq!(COEFFICIENTS.luma_offset, 16);
    }
}
