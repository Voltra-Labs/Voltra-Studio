//! How an item's pixels combine with what is already on the canvas.
//!
//! These are the equations libobs sets as GPU blend state, read from the table
//! at the top of `obs-scene.c`. Each row is
//! `(src_colour, src_alpha, dst_colour, dst_alpha, op)`:
//!
//! | Mode | Equation |
//! |---|---|
//! | `Normal` | `ONE, ONE, INVSRCALPHA, INVSRCALPHA, ADD` → `src + dst·(1−srcA)` |
//! | `Additive` | `ONE, ONE, ONE, ONE, ADD` → `src + dst` |
//! | `Subtract` | `ONE, ONE, ONE, ONE, REVERSE_SUBTRACT` → `dst − src` |
//! | `Screen` | `ONE, ONE, INVSRCCOLOR, INVSRCALPHA, ADD` → `src + dst·(1−src)` |
//! | `Multiply` | `DSTCOLOR, DSTALPHA, INVSRCALPHA, INVSRCALPHA, ADD` |
//! | `Lighten` | `ONE, ONE, ONE, ONE, MAX` → `max(src, dst)` |
//! | `Darken` | `ONE, ONE, ONE, ONE, MIN` → `min(src, dst)` |
//!
//! Note the `ONE` on the source colour of `Normal`: OBS composites with
//! **premultiplied alpha**. Sources here carry straight alpha, so the sampled
//! pixel is premultiplied before the equation is applied. Doing it any other way
//! would look right for opaque content and subtly wrong for everything else.
//!
//! Each mode is a separate type rather than a `match`, so the compositor
//! resolves it once per item and the pixel loop carries no branch (CLAUDE.md
//! §4.4).

/// A blend equation, applied per pixel to premultiplied source and destination.
pub(crate) trait Blend {
    /// Combine one premultiplied source pixel with the destination.
    ///
    /// Both are `[blue, green, red, alpha]`.
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4];
}

/// Clamp to the 8-bit range.
///
/// `clamp` rather than a chain of `if`s: the branchless form compiles to two
/// select instructions and leaves the loop vectorisable. Measured at 11% on the
/// colour converter in plan 004, and this loop runs it sixteen times per pixel.
#[inline]
fn clamp(value: i32) -> u8 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255 immediately above"
    )]
    {
        value.clamp(0, 255) as u8
    }
}

/// `a · b / 255`, rounded to nearest.
#[inline]
fn scale(a: i32, b: i32) -> i32 {
    let product = a * b + 128;
    (product + (product >> 8)) >> 8
}

/// Premultiply a straight-alpha pixel.
#[inline]
pub(crate) fn premultiply(pixel: [u8; 4]) -> [u8; 4] {
    // An opaque pixel is already premultiplied. Most content is opaque, so this
    // branch predicts perfectly and skips three multiplies.
    if pixel[3] == 255 {
        return pixel;
    }
    let alpha = i32::from(pixel[3]);
    [
        clamp(scale(i32::from(pixel[0]), alpha)),
        clamp(scale(i32::from(pixel[1]), alpha)),
        clamp(scale(i32::from(pixel[2]), alpha)),
        pixel[3],
    ]
}

/// `src + dst·(1−srcA)` — the default.
pub(crate) struct Normal;

impl Blend for Normal {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        // Opaque source-over is a copy: `src + dst·0`.
        if source[3] == 255 {
            return source;
        }
        let inverse = 255 - i32::from(source[3]);
        [
            clamp(i32::from(source[0]) + scale(i32::from(destination[0]), inverse)),
            clamp(i32::from(source[1]) + scale(i32::from(destination[1]), inverse)),
            clamp(i32::from(source[2]) + scale(i32::from(destination[2]), inverse)),
            clamp(i32::from(source[3]) + scale(i32::from(destination[3]), inverse)),
        ]
    }
}

/// `src + dst`, saturating. Light, fire, glows.
pub(crate) struct Additive;

impl Blend for Additive {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        [
            clamp(i32::from(source[0]) + i32::from(destination[0])),
            clamp(i32::from(source[1]) + i32::from(destination[1])),
            clamp(i32::from(source[2]) + i32::from(destination[2])),
            clamp(i32::from(source[3]) + i32::from(destination[3])),
        ]
    }
}

/// `dst − src`, saturating.
pub(crate) struct Subtract;

impl Blend for Subtract {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        [
            clamp(i32::from(destination[0]) - i32::from(source[0])),
            clamp(i32::from(destination[1]) - i32::from(source[1])),
            clamp(i32::from(destination[2]) - i32::from(source[2])),
            clamp(i32::from(destination[3]) - i32::from(source[3])),
        ]
    }
}

/// `src + dst·(1−src)` — never darkens.
pub(crate) struct Screen;

impl Blend for Screen {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        [
            clamp(
                i32::from(source[0]) + scale(i32::from(destination[0]), 255 - i32::from(source[0])),
            ),
            clamp(
                i32::from(source[1]) + scale(i32::from(destination[1]), 255 - i32::from(source[1])),
            ),
            clamp(
                i32::from(source[2]) + scale(i32::from(destination[2]), 255 - i32::from(source[2])),
            ),
            clamp(
                i32::from(source[3]) + scale(i32::from(destination[3]), 255 - i32::from(source[3])),
            ),
        ]
    }
}

/// `src·dst + dst·(1−srcA)` — never lightens.
pub(crate) struct Multiply;

impl Blend for Multiply {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        let inverse = 255 - i32::from(source[3]);
        [
            clamp(
                scale(i32::from(source[0]), i32::from(destination[0]))
                    + scale(i32::from(destination[0]), inverse),
            ),
            clamp(
                scale(i32::from(source[1]), i32::from(destination[1]))
                    + scale(i32::from(destination[1]), inverse),
            ),
            clamp(
                scale(i32::from(source[2]), i32::from(destination[2]))
                    + scale(i32::from(destination[2]), inverse),
            ),
            clamp(
                scale(i32::from(source[3]), i32::from(destination[3]))
                    + scale(i32::from(destination[3]), inverse),
            ),
        ]
    }
}

/// `max(src, dst)`.
pub(crate) struct Lighten;

impl Blend for Lighten {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        [
            source[0].max(destination[0]),
            source[1].max(destination[1]),
            source[2].max(destination[2]),
            source[3].max(destination[3]),
        ]
    }
}

/// `min(src, dst)`.
pub(crate) struct Darken;

impl Blend for Darken {
    #[inline]
    fn apply(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
        [
            source[0].min(destination[0]),
            source[1].min(destination[1]),
            source[2].min(destination[2]),
            source[3].min(destination[3]),
        ]
    }
}
