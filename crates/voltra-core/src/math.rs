//! Geometry for placing a source on the canvas.
//!
//! The model is the one libobs uses for scene items — crop, scale, alignment,
//! bounding box, rotation, position — because it is proven and because users
//! coming from OBS already reason in those terms.
//!
//! Where this differs is the output. OBS composites on the GPU and only needs
//! the forward matrix. Voltra's reference path composites on the CPU by
//! *inverse* mapping: for every destination pixel it asks where that pixel came
//! from in source space. [`Placement`] therefore carries both matrices, resolved
//! once per item per frame so the expensive work — trigonometry, matrix
//! inversion — never happens inside a per-pixel loop.

/// A point or a size in 2D.
///
/// `f32` rather than `f64` throughout: half the memory bandwidth, twice the SIMD
/// lanes, and exact for every integer up to 16.7 million — far beyond any canvas
/// this software will composite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
}

impl Vec2 {
    /// The origin.
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    /// Both components set to one.
    pub const ONE: Vec2 = Vec2 { x: 1.0, y: 1.0 };

    /// Build a vector from its components.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Build a vector with both components equal.
    pub const fn splat(value: f32) -> Self {
        Self { x: value, y: value }
    }
}

impl From<(f32, f32)> for Vec2 {
    fn from((x, y): (f32, f32)) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Build a rectangle from its origin and size.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// The right edge.
    pub fn right(self) -> f32 {
        self.x + self.w
    }

    /// The bottom edge.
    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    /// Whether the rectangle encloses no area.
    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// The overlap with `other`, or an empty rectangle when they are disjoint.
    #[must_use]
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
    }

    /// The smallest pixel-aligned bounds fully containing this rectangle, as
    /// `(left, top, right, bottom)`.
    ///
    /// This is the loop range the compositor will iterate: rounding outwards
    /// means a partially covered edge pixel still gets sampled and blended.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "floor/ceil then truncate is the intended rounding; canvas coordinates are far inside i64"
    )]
    pub fn outer_pixels(self) -> (i64, i64, i64, i64) {
        (
            self.x.floor() as i64,
            self.y.floor() as i64,
            self.right().ceil() as i64,
            self.bottom().ceil() as i64,
        )
    }
}

/// Which point of an item its position refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// The default, matching how OBS positions a freshly added source.
    #[default]
    TopLeft,
    /// Top edge, horizontally centred.
    Top,
    /// Top-right corner.
    TopRight,
    /// Left edge, vertically centred.
    Left,
    /// The centre of the item.
    Center,
    /// Right edge, vertically centred.
    Right,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom edge, horizontally centred.
    Bottom,
    /// Bottom-right corner.
    BottomRight,
}

impl Anchor {
    /// The anchor as normalised offsets inside the unit square.
    pub fn factors(self) -> Vec2 {
        let x = match self {
            Anchor::TopLeft | Anchor::Left | Anchor::BottomLeft => 0.0,
            Anchor::Top | Anchor::Center | Anchor::Bottom => 0.5,
            Anchor::TopRight | Anchor::Right | Anchor::BottomRight => 1.0,
        };
        let y = match self {
            Anchor::TopLeft | Anchor::Top | Anchor::TopRight => 0.0,
            Anchor::Left | Anchor::Center | Anchor::Right => 0.5,
            Anchor::BottomLeft | Anchor::Bottom | Anchor::BottomRight => 1.0,
        };
        Vec2::new(x, y)
    }
}

/// How a source is fitted into its bounding box.
///
/// These are the seven modes libobs offers (`OBS_BOUNDS_*`), kept identical so
/// that a scene authored in OBS means the same thing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BoundsMode {
    /// Ignore the bounding box and use the raw scale factors.
    #[default]
    None,
    /// Fill the box exactly, distorting the aspect ratio.
    Stretch,
    /// Fit inside the box, letterboxing. Aspect ratio preserved.
    Inner,
    /// Cover the box, overflowing on one axis. Aspect ratio preserved.
    Outer,
    /// Match the box width; height follows the aspect ratio.
    ToWidth,
    /// Match the box height; width follows the aspect ratio.
    ToHeight,
    /// Like [`Inner`](BoundsMode::Inner), but never scales up.
    MaxOnly,
}

/// Pixels trimmed from each edge of the source before scaling.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Crop {
    /// Pixels removed from the left edge.
    pub left: f32,
    /// Pixels removed from the top edge.
    pub top: f32,
    /// Pixels removed from the right edge.
    pub right: f32,
    /// Pixels removed from the bottom edge.
    pub bottom: f32,
}

impl Crop {
    /// Build a crop from its four edges.
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// Where and how a source is drawn on the canvas.
///
/// Applied in this order: crop, scale, bounding box, anchor, rotation, position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Canvas position of the anchor point, in pixels.
    pub pos: Vec2,
    /// Scale factors applied to the cropped source.
    pub scale: Vec2,
    /// Clockwise rotation in degrees.
    pub rotation: f32,
    /// Which point of the item `pos` refers to.
    pub anchor: Anchor,
    /// Pixels trimmed from the source before anything else.
    pub crop: Crop,
    /// Optional bounding box in canvas pixels.
    pub bounds: Option<Vec2>,
    /// How the source is fitted into `bounds`.
    pub bounds_mode: BoundsMode,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            pos: Vec2::ZERO,
            scale: Vec2::ONE,
            rotation: 0.0,
            anchor: Anchor::TopLeft,
            crop: Crop::default(),
            bounds: None,
            bounds_mode: BoundsMode::None,
        }
    }
}

/// A 2×3 affine matrix, laid out as in SVG: `[a c e; b d f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row 0, column 0.
    pub a: f32,
    /// Row 1, column 0.
    pub b: f32,
    /// Row 0, column 1.
    pub c: f32,
    /// Row 1, column 1.
    pub d: f32,
    /// Row 0, translation.
    pub e: f32,
    /// Row 1, translation.
    pub f: f32,
}

impl Affine {
    /// The transform that changes nothing.
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Map a point through the matrix.
    pub fn apply(self, point: Vec2) -> Vec2 {
        Vec2::new(
            self.a.mul_add(point.x, self.c.mul_add(point.y, self.e)),
            self.b.mul_add(point.x, self.d.mul_add(point.y, self.f)),
        )
    }

    /// The determinant, i.e. the signed area scale factor.
    pub fn determinant(self) -> f32 {
        self.a.mul_add(self.d, -(self.b * self.c))
    }

    /// The inverse matrix, or `None` when the transform collapses to zero area.
    pub fn invert(self) -> Option<Affine> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() < f32::EPSILON {
            return None;
        }
        let inv = 1.0 / det;
        Some(Affine {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: self.c.mul_add(self.f, -(self.d * self.e)) * inv,
            f: self.b.mul_add(self.e, -(self.a * self.f)) * inv,
        })
    }
}

/// A [`Transform`] resolved against a concrete source size.
///
/// Produced once per item per frame. The compositor iterates
/// [`bbox`](Placement::bbox) and samples through [`inverse`](Placement::inverse);
/// nothing here is recomputed per pixel.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    /// The region of the source that survives cropping, in source pixels.
    pub src: Rect,
    /// Source pixels to canvas pixels.
    pub forward: Affine,
    /// Canvas pixels to source pixels — the sampling direction.
    pub inverse: Affine,
    /// Canvas-space bounding box of the placed source, rotation included.
    pub bbox: Rect,
}

impl Transform {
    /// The scale actually applied, once the bounding box mode has had its say.
    fn effective_scale(self, cropped: Vec2) -> Vec2 {
        let Some(bounds) = self.bounds else {
            return self.scale;
        };
        if cropped.x <= 0.0 || cropped.y <= 0.0 {
            return self.scale;
        }
        let fit_x = bounds.x / cropped.x;
        let fit_y = bounds.y / cropped.y;
        let uniform = match self.bounds_mode {
            BoundsMode::None => return self.scale,
            BoundsMode::Stretch => return Vec2::new(fit_x, fit_y),
            BoundsMode::Inner => fit_x.min(fit_y),
            BoundsMode::Outer => fit_x.max(fit_y),
            BoundsMode::ToWidth => fit_x,
            BoundsMode::ToHeight => fit_y,
            BoundsMode::MaxOnly => fit_x.min(fit_y).min(1.0),
        };
        Vec2::splat(uniform)
    }

    /// Resolve this transform for a source of `size` pixels.
    ///
    /// Returns `None` when there is nothing to draw — the crop consumed the
    /// source, a scale factor is zero, or the matrix is otherwise degenerate.
    /// Making that a `None` rather than an empty rectangle means the compositor
    /// cannot forget to check.
    pub fn place(self, size: Vec2) -> Option<Placement> {
        let src = Rect::new(
            self.crop.left,
            self.crop.top,
            size.x - self.crop.left - self.crop.right,
            size.y - self.crop.top - self.crop.bottom,
        );
        if src.is_empty() {
            return None;
        }

        let cropped = Vec2::new(src.w, src.h);
        let scale = self.effective_scale(cropped);
        let scaled = Vec2::new(cropped.x * scale.x, cropped.y * scale.y);
        if !scaled.x.is_finite() || !scaled.y.is_finite() {
            return None;
        }

        // With a bounding box the anchor refers to the box, not to the scaled
        // image, so a letterboxed item stays put as its content changes size.
        let fitted = self.bounds_mode != BoundsMode::None;
        let anchor_box = match self.bounds {
            Some(bounds) if fitted => bounds,
            _ => scaled,
        };
        let factors = self.anchor.factors();
        let anchor_offset = Vec2::new(anchor_box.x * factors.x, anchor_box.y * factors.y);

        // Inside a bounding box the scaled image is centred on the leftover space.
        let inset = match self.bounds {
            Some(bounds) if fitted => {
                Vec2::new((bounds.x - scaled.x) * 0.5, (bounds.y - scaled.y) * 0.5)
            }
            _ => Vec2::ZERO,
        };

        let (sin, cos) = self.rotation.to_radians().sin_cos();

        // Compose: -crop, scale, inset - anchor, rotate, translate to pos.
        let pre_x = (-src.x).mul_add(scale.x, inset.x - anchor_offset.x);
        let pre_y = (-src.y).mul_add(scale.y, inset.y - anchor_offset.y);

        let forward = Affine {
            a: cos * scale.x,
            b: sin * scale.x,
            c: -sin * scale.y,
            d: cos * scale.y,
            e: cos.mul_add(pre_x, -(sin * pre_y)) + self.pos.x,
            f: sin.mul_add(pre_x, cos * pre_y) + self.pos.y,
        };
        let inverse = forward.invert()?;

        let corners = [
            forward.apply(Vec2::new(src.x, src.y)),
            forward.apply(Vec2::new(src.right(), src.y)),
            forward.apply(Vec2::new(src.right(), src.bottom())),
            forward.apply(Vec2::new(src.x, src.bottom())),
        ];
        let mut min = corners[0];
        let mut max = corners[0];
        for corner in &corners[1..] {
            min = Vec2::new(min.x.min(corner.x), min.y.min(corner.y));
            max = Vec2::new(max.x.max(corner.x), max.y.max(corner.y));
        }

        Some(Placement {
            src,
            forward,
            inverse,
            bbox: Rect::new(min.x, min.y, max.x - min.x, max.y - min.y),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, BoundsMode, Crop, Placement, Rect, Transform, Vec2};

    #[track_caller]
    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-3,
            "expected {expected}, got {actual}"
        );
    }

    #[track_caller]
    fn place(transform: Transform, size: (f32, f32)) -> Placement {
        transform
            .place(Vec2::new(size.0, size.1))
            .expect("expected a drawable placement")
    }

    #[test]
    fn identity_covers_the_whole_source() {
        let placed = place(Transform::default(), (100.0, 50.0));
        approx(placed.bbox.x, 0.0);
        approx(placed.bbox.y, 0.0);
        approx(placed.bbox.w, 100.0);
        approx(placed.bbox.h, 50.0);
    }

    /// The invariant the compositor depends on: sampling through the inverse
    /// must land back on the pixel the forward matrix came from.
    #[test]
    fn the_inverse_round_trips() {
        let transform = Transform {
            pos: Vec2::new(320.0, 180.0),
            scale: Vec2::new(1.5, 2.0),
            rotation: 33.0,
            anchor: Anchor::Center,
            ..Transform::default()
        };
        let placed = place(transform, (64.0, 64.0));

        for point in [Vec2::ZERO, Vec2::new(17.0, 41.0), Vec2::new(64.0, 64.0)] {
            let back = placed.inverse.apply(placed.forward.apply(point));
            approx(back.x, point.x);
            approx(back.y, point.y);
        }
    }

    #[test]
    fn a_centred_anchor_centres_the_item() {
        let transform = Transform {
            pos: Vec2::new(50.0, 50.0),
            anchor: Anchor::Center,
            ..Transform::default()
        };
        let placed = place(transform, (20.0, 10.0));
        approx(placed.bbox.x, 40.0);
        approx(placed.bbox.y, 45.0);
    }

    #[test]
    fn rotation_grows_the_bounding_box() {
        let transform = Transform {
            rotation: 45.0,
            anchor: Anchor::Center,
            ..Transform::default()
        };
        let placed = place(transform, (100.0, 100.0));
        // A square rotated 45 degrees spans its diagonal.
        approx(placed.bbox.w, 141.421);
        approx(placed.bbox.h, 141.421);
    }

    #[test]
    fn inner_bounds_letterbox_without_distortion() {
        let transform = Transform {
            bounds: Some(Vec2::new(400.0, 400.0)),
            bounds_mode: BoundsMode::Inner,
            ..Transform::default()
        };
        let placed = place(transform, (1920.0, 1080.0));
        approx(placed.bbox.w, 400.0);
        approx(placed.bbox.h, 225.0);
        // Equal bars above and below.
        approx(placed.bbox.y, 87.5);
        approx(400.0 - placed.bbox.bottom(), 87.5);
    }

    #[test]
    fn outer_bounds_cover_the_box() {
        let transform = Transform {
            bounds: Some(Vec2::new(400.0, 400.0)),
            bounds_mode: BoundsMode::Outer,
            ..Transform::default()
        };
        let placed = place(transform, (1920.0, 1080.0));
        approx(placed.bbox.h, 400.0);
        assert!(
            placed.bbox.w > 400.0,
            "the box should overflow horizontally"
        );
    }

    #[test]
    fn max_only_bounds_never_upscale() {
        let transform = Transform {
            bounds: Some(Vec2::new(400.0, 400.0)),
            bounds_mode: BoundsMode::MaxOnly,
            ..Transform::default()
        };
        let placed = place(transform, (64.0, 64.0));
        approx(placed.bbox.w, 64.0);
        approx(placed.bbox.h, 64.0);
    }

    #[test]
    fn stretch_bounds_fill_the_box_exactly() {
        let transform = Transform {
            bounds: Some(Vec2::new(400.0, 100.0)),
            bounds_mode: BoundsMode::Stretch,
            ..Transform::default()
        };
        let placed = place(transform, (1920.0, 1080.0));
        approx(placed.bbox.w, 400.0);
        approx(placed.bbox.h, 100.0);
    }

    #[test]
    fn cropping_shrinks_the_visible_area() {
        let transform = Transform {
            crop: Crop::new(10.0, 5.0, 10.0, 5.0),
            ..Transform::default()
        };
        let placed = place(transform, (100.0, 50.0));
        approx(placed.bbox.w, 80.0);
        approx(placed.bbox.h, 40.0);
        // The item stays where it was put; only its content is trimmed.
        approx(placed.bbox.x, 0.0);
        approx(placed.src.x, 10.0);
    }

    #[test]
    fn nothing_to_draw_is_none_not_an_empty_rect() {
        let zero_scale = Transform {
            scale: Vec2::ZERO,
            ..Transform::default()
        };
        assert!(zero_scale.place(Vec2::new(10.0, 10.0)).is_none());

        let over_cropped = Transform {
            crop: Crop::new(60.0, 0.0, 60.0, 0.0),
            ..Transform::default()
        };
        assert!(over_cropped.place(Vec2::new(100.0, 50.0)).is_none());

        let empty_source = Transform::default();
        assert!(empty_source.place(Vec2::ZERO).is_none());
    }

    #[test]
    fn rectangles_intersect_and_report_emptiness() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(50.0, 50.0, 100.0, 100.0);
        let overlap = a.intersect(b);
        approx(overlap.x, 50.0);
        approx(overlap.w, 50.0);
        assert!(a.intersect(Rect::new(200.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn outer_pixels_round_away_from_the_centre() {
        let rect = Rect::new(1.2, 2.7, 3.5, 4.1);
        assert_eq!(rect.outer_pixels(), (1, 2, 5, 7));
    }
}
