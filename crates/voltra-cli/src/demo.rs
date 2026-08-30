//! The scene `voltra render` draws when nobody supplied one.
//!
//! There is no project format yet — that is an architecture decision with its
//! own ADR — so the demonstration scene lives in code. It is built from
//! [`ColorSource`], the only source that exists, and everything moves through
//! the item transforms: orbit, rotation, pulsing scale and mixed alpha, so one
//! glance at the output exercises what plan 008 built (placement, rotation,
//! scaling, clipping against the canvas edge and alpha blending).
//!
//! Frame *n* depends only on *n*. Two runs produce identical files, which is
//! what lets a test assert on the bytes.

use std::f32::consts::TAU;

use voltra_core::{
    Anchor, FrameSize, Result, ScaleFilter, Scene, SceneItemId, Source, SourceId, TickContext,
    Vec2, VideoFrame, VideoSource,
};
use voltra_render::FrameProvider;
use voltra_sources::ColorSource;

/// How long one full orbit takes. Expressed in seconds and turned into a whole
/// number of frames from the rational clock, so the motion looks the same at
/// 30, at 60 and at 30000/1001.
const ORBIT_SECONDS: u64 = 4;

/// The colours of the orbiting bars, as `[r, g, b, a]`.
///
/// Two are opaque and two are not, so a still frame shows both the blend and
/// the plain copy.
const BAR_COLORS: [[u8; 4]; 4] = [
    [235, 64, 52, 255],
    [52, 168, 235, 180],
    [250, 200, 40, 255],
    [120, 235, 130, 140],
];

/// The animated demonstration scene, and the frames behind it.
#[derive(Debug)]
pub struct DemoScene {
    scene: Scene,
    /// Indexed by the raw value of the [`SourceId`] each item refers to.
    sources: Vec<ColorSource>,
    /// The panel first, then the bars, in the order they were added.
    items: Vec<SceneItemId>,
    canvas: FrameSize,
}

impl DemoScene {
    /// Build the scene for a canvas of `size`, resampling with `filter`.
    ///
    /// # Errors
    ///
    /// Propagates from [`ColorSource::new`], which allocates each source's
    /// frame.
    pub fn new(size: FrameSize, filter: ScaleFilter) -> Result<Self> {
        let mut scene = Scene::new("Demo", size);
        let mut sources = Vec::with_capacity(BAR_COLORS.len() + 1);
        let mut items = Vec::with_capacity(BAR_COLORS.len() + 1);

        // A translucent panel across the middle. It is added first, so it sits
        // underneath, and it is deliberately smaller than the area it covers so
        // the scale filter has something to do.
        let panel = FrameSize::new(even(size.width() / 3), even(size.height() / 3))?;
        sources.push(ColorSource::new(panel, [24, 32, 56, 150])?);
        items.push(scene.add(SourceId::from_raw(0)));

        let bar = FrameSize::new(even(size.width() / 8), even(size.height() / 16))?;
        for (index, color) in BAR_COLORS.iter().enumerate() {
            sources.push(ColorSource::new(bar, *color)?);
            items.push(scene.add(SourceId::from_raw(index as u64 + 1)));
        }

        for id in &items {
            if let Some(item) = scene.item_mut(*id) {
                item.scale_filter = filter;
                item.transform.anchor = Anchor::Center;
            }
        }

        Ok(Self {
            scene,
            sources,
            items,
            canvas: size,
        })
    }

    /// The scene as it stands.
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Advance every source and place every item for this frame.
    ///
    /// Ticking first and placing second is the order libobs uses and plan 008
    /// adopted: no source changes state while a composition is running.
    ///
    /// # Errors
    ///
    /// Propagates from [`Source::tick`].
    // Every cast below is exact in practice. The canvas cannot be wider than
    // 2^24 pixels — a single row would be 64 MB — so `u32 as f32` never loses a
    // bit, and the orbit period is a few hundred frames.
    #[allow(clippy::cast_precision_loss)]
    pub fn tick(&mut self, context: &TickContext) -> Result<()> {
        for source in &mut self.sources {
            source.tick(context)?;
        }

        // The period in whole frames, straight from the rational clock: no
        // float rounding, and 29.97 gets the same treatment as 30.
        let fps = context.fps();
        let period = (ORBIT_SECONDS * u64::from(fps.num()) / u64::from(fps.den())).max(1);
        let phase = (context.frame_index() % period) as f32 / period as f32;

        let center = Vec2::new(
            self.canvas.width() as f32 / 2.0,
            self.canvas.height() as f32 / 2.0,
        );

        // The panel: centred, stretched to half the canvas, breathing slightly.
        if let Some(item) = self.items.first().and_then(|id| self.scene.item_mut(*id)) {
            let breath = 0.05f32.mul_add((phase * TAU).sin(), 1.5);
            item.transform.pos = center;
            item.transform.scale = Vec2::splat(breath);
            item.transform.rotation = 0.0;
        }

        // The bars: evenly spaced around one orbit, each spinning on itself.
        // The orbit is an ellipse wide enough that a bar runs off the left and
        // right edges, so the output exercises clipping and not just placement.
        let radius = Vec2::new(
            self.canvas.width() as f32 * 0.40,
            self.canvas.height() as f32 * 0.36,
        );
        for (index, id) in self.items.iter().skip(1).enumerate() {
            let Some(item) = self.scene.item_mut(*id) else {
                continue;
            };
            let angle = (phase + index as f32 / BAR_COLORS.len() as f32) * TAU;
            item.transform.pos = Vec2::new(
                radius.x.mul_add(angle.cos(), center.x),
                radius.y.mul_add(angle.sin(), center.y),
            );
            item.transform.rotation = angle.to_degrees();
            let pulse = 0.6f32.mul_add((angle * 2.0).sin(), 1.6);
            item.transform.scale = Vec2::new(pulse, pulse);
        }

        Ok(())
    }
}

impl FrameProvider for DemoScene {
    fn frame(&self, source: SourceId) -> Option<&VideoFrame> {
        self.sources
            .get(usize::try_from(source.get()).ok()?)
            .and_then(VideoSource::frame)
    }
}

/// Round down to an even number, never below two.
///
/// 4:2:0 chroma cannot describe half a row, so every frame Voltra allocates has
/// even dimensions.
const fn even(value: u32) -> u32 {
    if value < 2 { 2 } else { value & !1 }
}

#[cfg(test)]
mod tests {
    use super::{DemoScene, even};
    use voltra_core::{Fps, FrameSize, ScaleFilter, SourceId, TickContext};
    use voltra_render::FrameProvider;

    #[test]
    fn every_item_resolves_to_a_frame_after_the_first_tick() {
        let size = FrameSize::new(320, 180).unwrap();
        let mut demo = DemoScene::new(size, ScaleFilter::Bilinear).unwrap();
        demo.tick(&TickContext::new(0, Fps::FPS_60)).unwrap();

        assert_eq!(demo.scene().len(), 5);
        for item in demo.scene().items() {
            assert!(
                demo.frame(item.source()).is_some(),
                "item {:?} has no frame",
                item.id()
            );
        }
    }

    #[test]
    fn an_unknown_source_has_no_frame() {
        let size = FrameSize::new(320, 180).unwrap();
        let demo = DemoScene::new(size, ScaleFilter::Point).unwrap();
        assert!(demo.frame(SourceId::from_raw(99)).is_none());
    }

    /// The animation is a pure function of the frame index: same index, same
    /// placement. That is what makes the rendered file reproducible.
    #[test]
    fn placement_depends_only_on_the_frame_index() {
        let size = FrameSize::new(320, 180).unwrap();
        let context = TickContext::new(37, Fps::FPS_60);

        let mut first = DemoScene::new(size, ScaleFilter::Point).unwrap();
        first.tick(&context).unwrap();
        let mut second = DemoScene::new(size, ScaleFilter::Point).unwrap();
        second.tick(&TickContext::new(0, Fps::FPS_60)).unwrap();
        second.tick(&context).unwrap();

        let a: Vec<_> = first.scene().items().map(|i| i.transform).collect();
        let b: Vec<_> = second.scene().items().map(|i| i.transform).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn sizes_stay_even_and_non_zero() {
        assert_eq!(even(0), 2);
        assert_eq!(even(1), 2);
        assert_eq!(even(7), 6);
        assert_eq!(even(8), 8);
    }
}
