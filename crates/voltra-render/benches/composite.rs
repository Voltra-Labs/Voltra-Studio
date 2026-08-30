//! What compositing a scene costs per frame.
//!
//! CLAUDE.md §4 reserves about 4 ms of the 16.6 ms frame budget for
//! composition. This is the number that says whether the CPU reference path
//! fits inside it, and the baseline the `wgpu` backend of ADR 0001 has to beat.

// `criterion_group!` expands to an undocumented public function, and panicking
// helpers are fine in a benchmark harness (CLAUDE.md §3).
#![allow(missing_docs, clippy::expect_used)]

use std::collections::HashMap;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use voltra_core::{
    FrameSize, PixelFormat, ScaleFilter, Scene, SourceId, Timestamp, Vec2, VideoFrame,
};
use voltra_render::{Compositor, CpuCompositor, FrameProvider};

#[derive(Debug, Default)]
struct Sources(HashMap<u64, VideoFrame>);

impl FrameProvider for Sources {
    fn frame(&self, source: SourceId) -> Option<&VideoFrame> {
        self.0.get(&source.get())
    }
}

fn size(width: u32, height: u32) -> FrameSize {
    let Ok(size) = FrameSize::new(width, height) else {
        unreachable!("benchmark dimensions are valid")
    };
    size
}

/// A frame with a gradient, so the branch predictor gets no free ride and the
/// alpha channel is not uniformly opaque.
fn gradient(width: u32, height: u32, alpha: u8) -> VideoFrame {
    let Ok(mut frame) = VideoFrame::new(PixelFormat::Bgra8, size(width, height), Timestamp::ZERO)
    else {
        unreachable!("BGRA frames have no size constraints")
    };
    if let Some(rows) = frame.rows_mut(0) {
        for (y, row) in rows.enumerate() {
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                pixel[0] = u8::try_from(x % 256).unwrap_or(0);
                pixel[1] = u8::try_from(y % 256).unwrap_or(0);
                pixel[2] = u8::try_from((x + y) % 256).unwrap_or(0);
                pixel[3] = alpha;
            }
        }
    }
    frame
}

fn composite(c: &mut Criterion) {
    let canvas = size(1920, 1080);
    let mut group = c.benchmark_group("composite_1080p");
    group.throughput(Throughput::Elements(1920 * 1080));

    // One full-screen opaque layer: the floor for any scene.
    {
        let mut sources = Sources::default();
        sources.0.insert(1, gradient(1920, 1080, 255));
        let mut scene = Scene::new("One layer", canvas);
        scene.add(SourceId::from_raw(1));

        let mut compositor = CpuCompositor::new(canvas).expect("compositor");
        group.bench_function("one_fullscreen_layer", |b| {
            b.iter(|| {
                compositor
                    .composite(black_box(&scene), black_box(&sources), Timestamp::ZERO)
                    .map(VideoFrame::byte_len)
            });
        });
    }

    // The acceptance case: a background plus four overlays, one of them scaled
    // and rotated so the general path is exercised rather than a lucky 1:1.
    {
        let mut sources = Sources::default();
        sources.0.insert(1, gradient(1920, 1080, 255));
        sources.0.insert(2, gradient(640, 360, 200));

        let mut scene = Scene::new("Five items", canvas);
        scene.add(SourceId::from_raw(1));
        for index in 0..4u16 {
            let item = scene.add(SourceId::from_raw(2));
            if let Some(entry) = scene.item_mut(item) {
                entry.transform.pos = Vec2::new(f32::from(index) * 300.0, 100.0);
                if index == 3 {
                    entry.transform.rotation = 15.0;
                    entry.transform.scale = Vec2::new(1.5, 1.5);
                }
            }
        }

        let mut compositor = CpuCompositor::new(canvas).expect("compositor");
        group.bench_function("five_items", |b| {
            b.iter(|| {
                compositor
                    .composite(black_box(&scene), black_box(&sources), Timestamp::ZERO)
                    .map(VideoFrame::byte_len)
            });
        });
    }

    // Point against bilinear on a full-screen upscale, to price the sampler.
    for filter in [ScaleFilter::Point, ScaleFilter::Bilinear] {
        let mut sources = Sources::default();
        sources.0.insert(1, gradient(960, 540, 255));

        let mut scene = Scene::new("Upscaled", canvas);
        let item = scene.add(SourceId::from_raw(1));
        if let Some(entry) = scene.item_mut(item) {
            entry.transform.scale = Vec2::new(2.0, 2.0);
            entry.scale_filter = filter;
        }

        let mut compositor = CpuCompositor::new(canvas).expect("compositor");
        group.bench_function(format!("fullscreen_upscale_{}", filter.name()), |b| {
            b.iter(|| {
                compositor
                    .composite(black_box(&scene), black_box(&sources), Timestamp::ZERO)
                    .map(VideoFrame::byte_len)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, composite);
criterion_main!(benches);
