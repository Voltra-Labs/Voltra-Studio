//! What recycling a frame saves over allocating one.
//!
//! Plan 003 measured a fresh 1080p BGRA frame at 387 µs, which at 60 fps is
//! 23 ms of wasted work per second of broadcast. This is the number that says
//! whether the pool actually removes it.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use voltra_core::{FramePool, FrameSize, PixelFormat, Timestamp, VideoFrame};

fn hd() -> FrameSize {
    let Ok(size) = FrameSize::new(1920, 1080) else {
        unreachable!("1920x1080 is a valid frame size")
    };
    size
}

fn frame_supply(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_supply_1080p_bgra");

    group.bench_function("allocate", |b| {
        b.iter(|| {
            VideoFrame::new(
                black_box(PixelFormat::Bgra8),
                black_box(hd()),
                Timestamp::ZERO,
            )
        });
    });

    let mut pool = FramePool::new(PixelFormat::Bgra8, hd());
    group.bench_function("pool_acquire_release", |b| {
        b.iter(|| {
            if let Ok(frame) = pool.acquire(black_box(Timestamp::ZERO)) {
                pool.release(frame);
            }
        });
    });

    // The honest comparison for a caller that cannot overwrite every pixel.
    let mut zeroing_pool = FramePool::new(PixelFormat::Bgra8, hd());
    group.bench_function("pool_acquire_zeroed_release", |b| {
        b.iter(|| {
            if let Ok(frame) = zeroing_pool.acquire_zeroed(black_box(Timestamp::ZERO)) {
                zeroing_pool.release(frame);
            }
        });
    });

    group.finish();
}

criterion_group!(benches, frame_supply);
criterion_main!(benches);
