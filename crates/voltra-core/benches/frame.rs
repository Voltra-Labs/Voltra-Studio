//! Baseline numbers for frame allocation and traversal.
//!
//! Allocation is the number the frame pool (plan 005) has to beat: recycling
//! only earns its complexity if it removes a cost worth removing. Traversal is
//! the floor for every per-pixel pass that follows — no filter or converter can
//! be faster than walking the bytes once.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use voltra_core::{FrameSize, PixelFormat, Timestamp, VideoFrame};

/// 1080p, the budget resolution from CLAUDE.md §4.
fn hd() -> FrameSize {
    let Ok(size) = FrameSize::new(1920, 1080) else {
        unreachable!("1920x1080 is a valid frame size")
    };
    size
}

fn allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_alloc_1080p");
    for format in [PixelFormat::Bgra8, PixelFormat::I420, PixelFormat::Nv12] {
        group.bench_function(format.name(), |b| {
            b.iter(|| VideoFrame::new(black_box(format), black_box(hd()), Timestamp::ZERO));
        });
    }
    group.finish();
}

fn traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_traversal_1080p");

    let Ok(frame) = VideoFrame::new(PixelFormat::Bgra8, hd(), Timestamp::ZERO) else {
        unreachable!("1080p BGRA is a valid frame")
    };

    // Reading every byte once: the floor for any per-pixel pass.
    group.bench_function("read_rows_bgra", |b| {
        b.iter(|| {
            let mut total = 0u64;
            if let Some(rows) = frame.rows(0) {
                for row in rows {
                    total += row.iter().copied().map(u64::from).sum::<u64>();
                }
            }
            black_box(total)
        });
    });

    let mut writable = frame.clone();
    group.bench_function("write_rows_bgra", |b| {
        b.iter(|| {
            if let Some(rows) = writable.rows_mut(0) {
                for row in rows {
                    row.fill(black_box(0x20));
                }
            }
        });
    });

    group.finish();
}

criterion_group!(benches, allocation, traversal);
criterion_main!(benches);
