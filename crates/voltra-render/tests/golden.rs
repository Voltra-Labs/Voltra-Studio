//! Golden-image tests for the CPU compositor.
//!
//! The canvases are small enough that the expected result is written out by
//! hand in the test, which makes a failure readable: you can see which pixel
//! moved and where to.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use voltra_core::{
    Anchor, BlendMode, FrameSize, PixelFormat, ScaleFilter, Scene, SourceId, Timestamp, Vec2,
    VideoFrame,
};
use voltra_render::{Compositor, CpuCompositor, FrameProvider};

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
    let mut frame = VideoFrame::new(PixelFormat::Bgra8, size(width, height), Timestamp::ZERO)
        .expect("valid frame");
    for row in frame.rows_mut(0).unwrap() {
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
    frame
}

/// One canvas pixel as `[b, g, r, a]`.
fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    let stride = frame.plane_layout(0).unwrap().stride();
    let offset = y as usize * stride + x as usize * 4;
    let bytes = &frame.plane(0).unwrap()[offset..offset + 4];
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

#[test]
fn an_empty_scene_is_the_background() {
    let scene = Scene::new("Empty", size(4, 2));
    let mut compositor = CpuCompositor::new(size(4, 2)).unwrap();
    compositor.set_background([10, 20, 30, 255]);

    let canvas = compositor
        .composite(&scene, &Sources::default(), Timestamp::ZERO)
        .unwrap();

    // Stored as BGRA, so the red given as 10 lands in the third byte.
    assert!(
        canvas
            .plane(0)
            .unwrap()
            .chunks_exact(4)
            .all(|px| px == [30, 20, 10, 255])
    );
    assert_eq!(compositor.stats().drawn, 0);
}

#[test]
fn an_opaque_source_at_one_to_one_reproduces_itself() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(4, 4, [10, 20, 30, 255]));

    let mut scene = Scene::new("Live", size(4, 4));
    scene.add(id);

    let mut compositor = CpuCompositor::new(size(4, 4)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(canvas.plane(0).unwrap(), sources.0[&1].plane(0).unwrap());
    assert_eq!(compositor.stats().drawn, 1);
}

#[test]
fn a_half_transparent_white_over_black_lands_halfway() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(2, 2, [255, 255, 255, 128]));

    let mut scene = Scene::new("Live", size(2, 2));
    scene.add(id);

    let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    // Premultiplied source-over: 255·(128/255) + 0 = 128.
    assert_eq!(pixel(canvas, 0, 0), [128, 128, 128, 255]);
}

#[test]
fn an_offset_item_leaves_the_background_alone() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(2, 2, [255, 255, 255, 255]));

    let mut scene = Scene::new("Live", size(4, 4));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().transform.pos = Vec2::new(2.0, 2.0);

    let mut compositor = CpuCompositor::new(size(4, 4)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(
        pixel(canvas, 0, 0),
        [0, 0, 0, 255],
        "background was touched"
    );
    assert_eq!(pixel(canvas, 1, 1), [0, 0, 0, 255], "bled outside its box");
    assert_eq!(pixel(canvas, 2, 2), [255, 255, 255, 255]);
    assert_eq!(pixel(canvas, 3, 3), [255, 255, 255, 255]);
}

#[test]
fn an_item_hanging_off_the_canvas_is_clipped() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(4, 4, [255, 255, 255, 255]));

    let mut scene = Scene::new("Live", size(4, 4));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().transform.pos = Vec2::new(-2.0, -2.0);

    let mut compositor = CpuCompositor::new(size(4, 4)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(pixel(canvas, 0, 0), [255, 255, 255, 255]);
    assert_eq!(pixel(canvas, 1, 1), [255, 255, 255, 255]);
    assert_eq!(pixel(canvas, 2, 2), [0, 0, 0, 255], "clipped away");
    assert_eq!(compositor.stats().drawn, 1);
}

#[test]
fn an_item_entirely_off_canvas_is_skipped() {
    let mut sources = Sources::default();
    let id = sources.insert(1, solid(2, 2, [255, 255, 255, 255]));

    let mut scene = Scene::new("Live", size(4, 4));
    let item = scene.add(id);
    scene.item_mut(item).unwrap().transform.pos = Vec2::new(100.0, 100.0);

    let mut compositor = CpuCompositor::new(size(4, 4)).unwrap();
    compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(compositor.stats().drawn, 0);
    assert_eq!(compositor.stats().skipped, 1);
}

/// Rotating a two-pixel strip by half a turn must swap its ends exactly.
#[test]
fn a_half_turn_swaps_the_ends() {
    let mut left_right = VideoFrame::new(PixelFormat::Bgra8, size(2, 1), Timestamp::ZERO).unwrap();
    {
        let row = left_right.rows_mut(0).unwrap().next().unwrap();
        row[..4].copy_from_slice(&[1, 1, 1, 255]);
        row[4..8].copy_from_slice(&[2, 2, 2, 255]);
    }

    let mut sources = Sources::default();
    let id = sources.insert(1, left_right);

    let mut scene = Scene::new("Live", size(2, 1));
    let item = scene.add(id);
    {
        let entry = scene.item_mut(item).unwrap();
        entry.transform.rotation = 180.0;
        entry.transform.anchor = Anchor::Center;
        entry.transform.pos = Vec2::new(1.0, 0.5);
        entry.scale_filter = ScaleFilter::Point;
    }

    let mut compositor = CpuCompositor::new(size(2, 1)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(pixel(canvas, 0, 0), [2, 2, 2, 255]);
    assert_eq!(pixel(canvas, 1, 0), [1, 1, 1, 255]);
}

#[test]
fn point_duplicates_pixels_where_bilinear_interpolates() {
    let mut ramp = VideoFrame::new(PixelFormat::Bgra8, size(2, 1), Timestamp::ZERO).unwrap();
    {
        let row = ramp.rows_mut(0).unwrap().next().unwrap();
        row[..4].copy_from_slice(&[0, 0, 0, 255]);
        row[4..8].copy_from_slice(&[255, 255, 255, 255]);
    }

    let mut sources = Sources::default();
    let id = sources.insert(1, ramp);

    let mut scene = Scene::new("Live", size(8, 1));
    let item = scene.add(id);
    {
        let entry = scene.item_mut(item).unwrap();
        entry.transform.scale = Vec2::new(4.0, 1.0);
        entry.scale_filter = ScaleFilter::Point;
    }

    let mut compositor = CpuCompositor::new(size(8, 1)).unwrap();
    let sharp: Vec<u8> = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap()
        .plane(0)
        .unwrap()
        .chunks_exact(4)
        .map(|px| px[0])
        .collect();
    assert_eq!(sharp, vec![0, 0, 0, 0, 255, 255, 255, 255]);

    scene.item_mut(item).unwrap().scale_filter = ScaleFilter::Bilinear;
    let smooth: Vec<u8> = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap()
        .plane(0)
        .unwrap()
        .chunks_exact(4)
        .map(|px| px[0])
        .collect();

    assert!(
        smooth.windows(2).all(|pair| pair[0] <= pair[1]),
        "a bilinear upscale should ramp: {smooth:?}"
    );
    assert!(
        smooth.iter().any(|&value| value > 0 && value < 255),
        "no intermediate value was produced: {smooth:?}"
    );
}

/// One known case per blend mode, over a mid-grey background.
#[test]
fn every_blend_mode_has_its_own_answer() {
    let expected = [
        (BlendMode::Normal, 64),
        (BlendMode::Additive, 192),
        (BlendMode::Subtract, 64),
        (BlendMode::Screen, 160),
        (BlendMode::Multiply, 32),
        (BlendMode::Lighten, 128),
        (BlendMode::Darken, 64),
    ];

    for (mode, want) in expected {
        let mut sources = Sources::default();
        let id = sources.insert(1, solid(2, 2, [64, 64, 64, 255]));

        let mut scene = Scene::new("Live", size(2, 2));
        let item = scene.add(id);
        scene.item_mut(item).unwrap().blend = mode;

        let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
        compositor.set_background([128, 128, 128, 255]);
        let canvas = compositor
            .composite(&scene, &sources, Timestamp::ZERO)
            .unwrap();

        assert_eq!(
            pixel(canvas, 0, 0)[0],
            want,
            "{} blended wrong",
            mode.name()
        );
    }
}

#[test]
fn the_last_item_is_drawn_on_top() {
    let mut sources = Sources::default();
    let back = sources.insert(1, solid(2, 2, [10, 10, 10, 255]));
    let front = sources.insert(2, solid(2, 2, [20, 20, 20, 255]));

    let mut scene = Scene::new("Live", size(2, 2));
    scene.add(back);
    let top = scene.add(front);

    let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
    assert_eq!(
        pixel(
            compositor
                .composite(&scene, &sources, Timestamp::ZERO)
                .unwrap(),
            0,
            0
        ),
        [20, 20, 20, 255]
    );

    // Send it to the back and the other one wins.
    scene.lower_to_bottom(top);
    assert_eq!(
        pixel(
            compositor
                .composite(&scene, &sources, Timestamp::ZERO)
                .unwrap(),
            0,
            0
        ),
        [10, 10, 10, 255]
    );
}

#[test]
fn hidden_items_and_missing_sources_are_counted_not_drawn() {
    let mut sources = Sources::default();
    let present = sources.insert(1, solid(2, 2, [255, 255, 255, 255]));

    let mut scene = Scene::new("Live", size(2, 2));
    let hidden = scene.add(present);
    scene.item_mut(hidden).unwrap().visible = false;
    scene.add(SourceId::from_raw(99));

    let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(
        pixel(canvas, 0, 0),
        [0, 0, 0, 255],
        "nothing should be drawn"
    );
    assert_eq!(compositor.stats().drawn, 0);
    assert_eq!(
        compositor.stats().skipped,
        1,
        "only the missing source counts"
    );
}

/// A camera handing over I420 must be reported, not drawn as garbage.
#[test]
fn yuv_sources_are_reported_as_unsupported() {
    let mut sources = Sources::default();
    let yuv = VideoFrame::new(PixelFormat::I420, size(2, 2), Timestamp::ZERO).unwrap();
    let id = sources.insert(1, yuv);

    let mut scene = Scene::new("Live", size(2, 2));
    scene.add(id);

    let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    assert_eq!(
        pixel(canvas, 0, 0),
        [0, 0, 0, 255],
        "the canvas was corrupted"
    );
    assert_eq!(compositor.stats().unsupported, 1);
    assert_eq!(compositor.stats().drawn, 0);
}

#[test]
fn the_canvas_follows_the_scene_size_and_timestamp() {
    let mut compositor = CpuCompositor::new(size(2, 2)).unwrap();
    let scene = Scene::new("Live", size(6, 4));

    let canvas = compositor
        .composite(&scene, &Sources::default(), Timestamp::from_millis(500))
        .unwrap();

    assert_eq!(canvas.size(), size(6, 4));
    assert_eq!(canvas.pts(), Timestamp::from_millis(500));
}

#[test]
fn rgba_sources_are_swizzled_rather_than_refused() {
    let mut frame = VideoFrame::new(PixelFormat::Rgba8, size(1, 1), Timestamp::ZERO).unwrap();
    // Red, green, blue, alpha.
    frame.as_bytes_mut()[..4].copy_from_slice(&[200, 100, 50, 255]);

    let mut sources = Sources::default();
    let id = sources.insert(1, frame);

    let mut scene = Scene::new("Live", size(1, 1));
    scene.add(id);

    let mut compositor = CpuCompositor::new(size(1, 1)).unwrap();
    let canvas = compositor
        .composite(&scene, &sources, Timestamp::ZERO)
        .unwrap();

    // The canvas is BGRA, so the channels come back in the other order.
    assert_eq!(pixel(canvas, 0, 0), [50, 100, 200, 255]);
}
