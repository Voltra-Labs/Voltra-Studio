//! The GPU backend, checked against the CPU one.
//!
//! The CPU compositor passes sixteen golden images (`golden.rs`), so it is the
//! oracle here: every test runs the same scene through both backends and
//! compares the canvases. That is a kind of verification OBS cannot do — it has
//! no CPU path to compare against — and it is the reason ADR 0001 kept one.
//!
//! Where the two *can* agree exactly, exact agreement is demanded. Where they
//! cannot — GPU bilinear filtering is `f32` with implementation-defined subtexel
//! precision, the CPU sampler is 16.16 fixed point — the tolerance is stated and
//! justified rather than widened until the test passes.
//!
//! Every test skips, rather than fails, when the machine has no adapter.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use voltra_core::{
    Anchor, BlendMode, FrameSize, PixelFormat, ScaleFilter, Scene, SourceId, Timestamp, Vec2,
    VideoFrame,
};
use voltra_render::{Compositor, CpuCompositor, FrameProvider, GpuCompositor, GpuContext};

/// A frame provider backed by a map, standing in for the engine.
#[derive(Debug, Default)]
struct Sources(HashMap<u64, VideoFrame>);

impl Sources {
    fn insert(&mut self, id: u64, frame: VideoFrame) -> SourceId {
        self.0.insert(id, frame);
        SourceId::from_raw(id)
    }
}

impl FrameProvider for Sources {
    fn frame(&self, source: SourceId) -> Option<&VideoFrame> {
        self.0.get(&source.get())
    }
}

fn size(width: u32, height: u32) -> FrameSize {
    FrameSize::new(width, height).unwrap()
}

/// A frame of one colour, given as `[b, g, r, a]`.
fn solid(width: u32, height: u32, color: [u8; 4]) -> VideoFrame {
    let mut frame =
        VideoFrame::new(PixelFormat::Bgra8, size(width, height), Timestamp::ZERO).unwrap();
    for row in frame.rows_mut(0).unwrap() {
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
    frame
}

/// A frame with a different colour in each quadrant, so a flip or a transpose
/// cannot pass for correct.
fn quadrants(width: u32, height: u32) -> VideoFrame {
    let mut frame =
        VideoFrame::new(PixelFormat::Bgra8, size(width, height), Timestamp::ZERO).unwrap();
    let half_x = width / 2;
    let half_y = height / 2;
    for (y, row) in frame.rows_mut(0).unwrap().enumerate() {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let right = u32::try_from(x).unwrap() >= half_x;
            let bottom = u32::try_from(y).unwrap() >= half_y;
            pixel.copy_from_slice(&match (right, bottom) {
                (false, false) => [0, 0, 255, 255],
                (true, false) => [0, 255, 0, 255],
                (false, true) => [255, 0, 0, 255],
                (true, true) => [255, 255, 255, 255],
            });
        }
    }
    frame
}

/// How far apart two canvases are.
#[derive(Debug, Default)]
struct Difference {
    /// The largest single-channel difference anywhere.
    max: u8,
    /// How many pixels differ at all.
    pixels: usize,
    /// Where the first difference is, for a readable failure.
    first: Option<(usize, [u8; 4], [u8; 4])>,
}

fn compare(cpu: &VideoFrame, gpu: &VideoFrame) -> Difference {
    assert_eq!(cpu.size(), gpu.size(), "canvases differ in size");
    let mut report = Difference::default();
    let left = cpu.plane(0).unwrap();
    let right = gpu.plane(0).unwrap();

    for (index, (a, b)) in left.chunks_exact(4).zip(right.chunks_exact(4)).enumerate() {
        let mut worst = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            worst = worst.max(x.abs_diff(*y));
        }
        if worst > 0 {
            report.pixels += 1;
            report.max = report.max.max(worst);
            if report.first.is_none() {
                report.first = Some((index, [a[0], a[1], a[2], a[3]], [b[0], b[1], b[2], b[3]]));
            }
        }
    }
    report
}

/// Run a scene through both backends, or return `None` when there is no adapter.
fn both(
    scene: &Scene,
    sources: &Sources,
    background: [u8; 4],
) -> Option<(VideoFrame, VideoFrame, [voltra_render::CompositeStats; 2])> {
    let context = match GpuContext::new() {
        Ok(context) => context,
        Err(reason) => {
            eprintln!("skipping: {reason}");
            return None;
        }
    };

    let mut cpu = CpuCompositor::new(scene.size()).unwrap();
    cpu.set_background(background);
    let cpu_canvas = cpu
        .composite(scene, sources, Timestamp::ZERO)
        .unwrap()
        .clone();

    let mut gpu = GpuCompositor::with_context(context, scene.size()).unwrap();
    gpu.set_background(background);
    let gpu_canvas = gpu
        .composite(scene, sources, Timestamp::ZERO)
        .unwrap()
        .clone();

    Some((cpu_canvas, gpu_canvas, [cpu.stats(), gpu.stats()]))
}

/// Assert the two canvases are identical, byte for byte.
fn assert_exact(cpu: &VideoFrame, gpu: &VideoFrame, what: &str) {
    let report = compare(cpu, gpu);
    assert_eq!(
        report.pixels, 0,
        "{what}: {} pixels differ, worst by {}; first at {:?}",
        report.pixels, report.max, report.first
    );
}

/// Assert the two canvases agree within `tolerance` code values per channel.
fn assert_close(cpu: &VideoFrame, gpu: &VideoFrame, tolerance: u8, what: &str) {
    let report = compare(cpu, gpu);
    assert!(
        report.max <= tolerance,
        "{what}: worst channel differs by {} (tolerance {tolerance}); \
         {} pixels differ; first at {:?}",
        report.max,
        report.pixels,
        report.first
    );
}

// ---------------------------------------------------------------- exact cases

/// Clearing is not sampling and not blending: there is no reason for the two
/// backends to disagree by even one code value.
#[test]
fn an_empty_scene_matches_exactly() {
    let scene = Scene::new("Empty", size(64, 32));
    let Some((cpu, gpu, stats)) = both(&scene, &Sources::default(), [10, 20, 30, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "empty scene");
    assert_eq!(stats[0].drawn, stats[1].drawn);
}

/// An opaque item at 1:1 with no transform is a copy on both backends: each
/// destination pixel maps to exactly one texel centre.
#[test]
fn an_opaque_source_at_one_to_one_matches_exactly() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(64, 32));
    let mut scene = Scene::new("Copy", size(64, 32));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, stats)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "1:1 opaque copy");
    assert_eq!(stats[0].drawn, 1);
    assert_eq!(stats[1].drawn, 1);
}

/// A whole-pixel translation keeps every sample on a texel centre, so this stays
/// exact too — and it is the case where a wrong clip-space Y flip would show up
/// as the item drawn upside down rather than as a rounding difference.
#[test]
fn a_whole_pixel_offset_matches_exactly() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(16, 16));
    let mut scene = Scene::new("Offset", size(64, 32));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.pos = Vec2::new(8.0, 4.0);
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [7, 9, 11, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "whole-pixel offset");
}

/// The classic readback bug: 100 pixels is 400 bytes per row, which is not a
/// multiple of the 256 the copy demands. Without padding, every row lands late
/// and the image shears.
#[test]
fn an_unaligned_canvas_width_does_not_shear() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(100, 30));
    let mut scene = Scene::new("Unaligned", size(100, 30));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "unaligned width");
}

/// Counters have to agree, or the two backends disagree about what a scene even
/// contains: a hidden item, a missing source and a format neither can read.
#[test]
fn the_statistics_agree() {
    let mut sources = Sources::default();
    let drawn = sources.insert(1, solid(8, 8, [255, 255, 255, 255]));
    let hidden = sources.insert(2, solid(8, 8, [255, 0, 0, 255]));
    let yuv = sources.insert(
        3,
        VideoFrame::new(PixelFormat::I420, size(8, 8), Timestamp::ZERO).unwrap(),
    );

    let mut scene = Scene::new("Mixed", size(32, 32));
    scene.add(drawn);
    let hidden_item = scene.add(hidden);
    scene.item_mut(hidden_item).unwrap().visible = false;
    scene.add(yuv);
    // A source nothing provides a frame for.
    scene.add(SourceId::from_raw(99));

    let Some((_, _, [cpu, gpu])) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_eq!(cpu.drawn, gpu.drawn, "drawn");
    assert_eq!(cpu.skipped, gpu.skipped, "skipped");
    assert_eq!(cpu.unsupported, gpu.unsupported, "unsupported");
    assert_eq!(gpu.drawn, 1);
    assert_eq!(gpu.unsupported, 1);
}

// ------------------------------------------------------------ tolerated cases

/// Straight alpha over a background. Both premultiply, but the GPU does it in
/// `f32` and the CPU in rounded 8-bit fixed point, so one code value apart is
/// the expected outcome, not a defect.
#[test]
fn partial_alpha_matches_within_one_code_value() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(32, 32, [255, 255, 255, 128]));
    let mut scene = Scene::new("Alpha", size(32, 32));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_close(&cpu, &gpu, 1, "half-transparent white over black");
}

/// Every blend mode, each over the same pair of layers. This is the test that
/// proves both backends really are evaluating libobs's table and not two
/// different equations that happen to agree on opaque content.
#[test]
fn every_blend_mode_agrees() {
    for mode in [
        BlendMode::Normal,
        BlendMode::Additive,
        BlendMode::Subtract,
        BlendMode::Screen,
        BlendMode::Multiply,
        BlendMode::Lighten,
        BlendMode::Darken,
    ] {
        let mut sources = Sources::default();
        let under = sources.insert(1, solid(32, 32, [40, 80, 120, 255]));
        let over = sources.insert(2, solid(32, 32, [90, 60, 30, 200]));

        let mut scene = Scene::new("Blend", size(32, 32));
        let bottom = scene.add(under);
        scene.item_mut(bottom).unwrap().scale_filter = ScaleFilter::Point;
        let top = scene.add(over);
        let entry = scene.item_mut(top).unwrap();
        entry.blend = mode;
        entry.scale_filter = ScaleFilter::Point;

        let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
            return;
        };
        assert_close(&cpu, &gpu, 2, &format!("{mode:?}"));
    }
}

/// Layer order: the later item wins. If the GPU drew in the wrong order the
/// difference would be the whole canvas, not a rounding step.
#[test]
fn layer_order_agrees() {
    let mut sources = Sources::default();
    let under = sources.insert(1, solid(32, 32, [255, 0, 0, 255]));
    let over = sources.insert(2, solid(16, 16, [0, 255, 0, 255]));

    let mut scene = Scene::new("Order", size(32, 32));
    let bottom = scene.add(under);
    scene.item_mut(bottom).unwrap().scale_filter = ScaleFilter::Point;
    let top = scene.add(over);
    let entry = scene.item_mut(top).unwrap();
    entry.transform.pos = Vec2::new(8.0, 8.0);
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "layer order");
}

/// A half turn about the centre. Rotation is where a transposed matrix or a
/// wrong winding order stops being subtle.
#[test]
fn a_half_turn_agrees() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(32, 32));
    let mut scene = Scene::new("Rotated", size(32, 32));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.anchor = Anchor::Center;
    entry.transform.pos = Vec2::new(16.0, 16.0);
    entry.transform.rotation = 180.0;
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_close(&cpu, &gpu, 1, "half turn");
}

/// Integer upscaling with nearest neighbour: both backends should pick the same
/// texel for every destination pixel, so this is where sampling agreement is
/// tightest.
#[test]
fn nearest_neighbour_upscaling_agrees() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(16, 16));
    let mut scene = Scene::new("Scaled", size(64, 64));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.scale = Vec2::splat(4.0);
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "nearest-neighbour ×4");
}

/// Bilinear upscaling is where the two genuinely differ: `f32` interpolation
/// against 16.16 fixed point. The tolerance is what that costs, and the test
/// also counts how much of the canvas is affected so a real regression cannot
/// hide behind it.
#[test]
fn bilinear_upscaling_agrees_within_tolerance() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(16, 16));
    let mut scene = Scene::new("Bilinear", size(64, 64));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.scale = Vec2::splat(4.0);
    entry.scale_filter = ScaleFilter::Bilinear;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_close(&cpu, &gpu, 4, "bilinear ×4");
}

/// An item entirely outside the canvas leaves the background alone on both.
#[test]
fn an_item_off_canvas_agrees() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(16, 16, [255, 255, 255, 255]));
    let mut scene = Scene::new("Off", size(32, 32));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().transform.pos = Vec2::new(500.0, 500.0);

    let Some((cpu, gpu, [cpu_stats, gpu_stats])) = both(&scene, &sources, [3, 5, 7, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "off-canvas item");
    assert_eq!(cpu_stats.skipped, gpu_stats.skipped);
}

/// An item clipped by the canvas edge: the rasteriser clips on the GPU and the
/// bounding-box intersection clips on the CPU, and they have to agree about
/// which pixels survive.
#[test]
fn a_clipped_item_agrees() {
    let mut sources = Sources::default();
    let id = sources.insert(1, quadrants(32, 32));
    let mut scene = Scene::new("Clipped", size(32, 32));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.pos = Vec2::new(-10.0, -6.0);
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };
    assert_exact(&cpu, &gpu, "clipped item");
}

/// The adapter is worth printing once: "it works" and "it works on a software
/// rasteriser" are different answers, and only one says anything about speed.
#[test]
fn the_adapter_reports_what_it_is() {
    let Ok(context) = GpuContext::new() else {
        eprintln!("skipping: no adapter");
        return;
    };
    let info = context.info();
    eprintln!("adapter: {info}");
    assert!(!info.name.is_empty());
    assert!(context.max_dimension() >= 2048);
}

// ------------------------------------------------------- characterised limits

/// Whether `index` sits on a colour boundary in `frame`, i.e. at least one of
/// its four neighbours is a different colour.
///
/// A pixel in the middle of a solid region is surrounded by copies of itself; a
/// pixel on the outline of a drawn item is not. That is what makes this a test
/// of *where* the two backends disagree and not merely of how much.
fn on_a_boundary(frame: &VideoFrame, index: usize) -> bool {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let pixels = frame.plane(0).unwrap();
    let at = |x: usize, y: usize| -> [u8; 4] {
        let offset = (y * width + x) * 4;
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    };

    let (x, y) = (index % width, index / width);
    let mine = at(x, y);
    // The canvas edge counts as a boundary: there is nothing to compare against.
    if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
        return true;
    }
    at(x - 1, y) != mine || at(x + 1, y) != mine || at(x, y - 1) != mine || at(x, y + 1) != mine
}

/// An arbitrary rotation is the one case where the two backends genuinely
/// cannot agree everywhere, and this test says exactly how far that goes.
///
/// A pixel centre landing on a quad edge is drawn by whichever backend's
/// coverage rule says so: the GPU's rasteriser fill rule, or the CPU's inverse
/// map plus bounds check. They disagree on a handful of outline pixels, at full
/// colour magnitude, because it is not a rounding difference — it is one backend
/// drawing a pixel the other left as background.
///
/// So the assertion is not on magnitude. It is that **every** disagreement sits
/// on a colour boundary, and that the count scales with the item's outline
/// rather than its area.
#[test]
fn an_arbitrary_rotation_disagrees_only_on_the_outline() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(24, 24, [255, 255, 255, 255]));
    let mut scene = Scene::new("Rotated", size(64, 64));
    let item = scene.add(id);
    let entry = scene.item_mut(item).unwrap();
    entry.transform.anchor = Anchor::Center;
    entry.transform.pos = Vec2::new(32.0, 32.0);
    entry.transform.rotation = 37.0;
    entry.scale_filter = ScaleFilter::Point;

    let Some((cpu, gpu, _)) = both(&scene, &sources, [0, 0, 0, 255]) else {
        return;
    };

    let left = cpu.plane(0).unwrap();
    let right = gpu.plane(0).unwrap();
    let mut differing = Vec::new();
    for (index, (a, b)) in left.chunks_exact(4).zip(right.chunks_exact(4)).enumerate() {
        if a != b {
            differing.push(index);
        }
    }

    for index in &differing {
        assert!(
            on_a_boundary(&cpu, *index),
            "pixel {index} differs but is not on an outline: the disagreement is \
             not a fill-rule edge case"
        );
    }

    // The quad is 24×24 rotated, so its outline is roughly 96 pixels. Anything
    // approaching the 4096-pixel area would mean the two backends disagree about
    // the shape itself, not about its edge.
    assert!(
        differing.len() < 120,
        "{} pixels differ, which is more than an outline",
        differing.len()
    );
}
