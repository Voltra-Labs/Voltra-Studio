//! Baseline numbers for the per-frame geometry work.
//!
//! Neither of these runs per pixel — `place` runs once per scene item per frame
//! and `pts` once per frame — but both sit on the frame path, and CLAUDE.md §4
//! says an optimisation without a number is an opinion. These are the numbers
//! everything later is compared against.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use voltra_core::{Anchor, BoundsMode, Fps, Transform, Vec2};

fn placement(c: &mut Criterion) {
    let mut group = c.benchmark_group("placement");

    // The common case: an unrotated source dropped on the canvas.
    let simple = Transform {
        pos: Vec2::new(100.0, 100.0),
        ..Transform::default()
    };
    group.bench_function("simple", |b| {
        b.iter(|| black_box(simple).place(black_box(Vec2::new(1920.0, 1080.0))));
    });

    // The expensive case: rotation, non-uniform scale, crop and a bounding box.
    let complex = Transform {
        pos: Vec2::new(640.0, 360.0),
        scale: Vec2::new(1.5, 1.25),
        rotation: 37.5,
        anchor: Anchor::Center,
        crop: voltra_core::Crop::new(8.0, 8.0, 8.0, 8.0),
        bounds: Some(Vec2::new(800.0, 450.0)),
        bounds_mode: BoundsMode::Inner,
    };
    group.bench_function("rotated_bounded_cropped", |b| {
        b.iter(|| black_box(complex).place(black_box(Vec2::new(1920.0, 1080.0))));
    });

    // What a busy scene costs in geometry alone, once per frame.
    group.bench_function("scene_of_50_items", |b| {
        b.iter(|| {
            let mut placed = 0usize;
            for index in 0..50u16 {
                let transform = Transform {
                    pos: Vec2::new(f32::from(index) * 7.0, 32.0),
                    rotation: f32::from(index),
                    anchor: Anchor::Center,
                    ..Transform::default()
                };
                if transform
                    .place(black_box(Vec2::new(1920.0, 1080.0)))
                    .is_some()
                {
                    placed += 1;
                }
            }
            black_box(placed)
        });
    });

    group.finish();
}

fn timestamps(c: &mut Criterion) {
    c.bench_function("fps_pts_ntsc", |b| {
        b.iter(|| black_box(Fps::FPS_29_97).pts(black_box(1_234_567)));
    });
}

criterion_group!(benches, placement, timestamps);
criterion_main!(benches);
