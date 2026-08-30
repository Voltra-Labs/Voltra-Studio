//! What a colour conversion costs per frame.
//!
//! This is the first per-pixel loop in the project, so it is the first number
//! that has to fit inside the 16.6 ms budget of CLAUDE.md §4 — and the baseline
//! the GPU path of ADR 0001 will have to beat.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use voltra_core::{ColorSpec, FrameSize, PixelFormat, Timestamp, VideoFrame, rgb_to_yuv};

/// A canvas painted with a gradient, so no branch predictor gets a free ride
/// from uniform input.
fn canvas(width: u32, height: u32) -> VideoFrame {
    let Ok(size) = FrameSize::new(width, height) else {
        unreachable!("benchmark dimensions are valid")
    };
    let Ok(mut frame) = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO) else {
        unreachable!("BGRA frames have no size constraints")
    };
    if let Some(rows) = frame.rows_mut(0) {
        for (y, row) in rows.enumerate() {
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                pixel[0] = u8::try_from(x % 256).unwrap_or(0);
                pixel[1] = u8::try_from(y % 256).unwrap_or(0);
                pixel[2] = u8::try_from((x + y) % 256).unwrap_or(0);
                pixel[3] = 255;
            }
        }
    }
    frame
}

fn destination(format: PixelFormat, width: u32, height: u32) -> VideoFrame {
    let Ok(size) = FrameSize::new(width, height) else {
        unreachable!("benchmark dimensions are valid")
    };
    let Ok(frame) = VideoFrame::new(format, size, Timestamp::ZERO) else {
        unreachable!("benchmark dimensions are even")
    };
    frame
}

fn conversion(c: &mut Criterion) {
    for (label, width, height) in [("720p", 1280, 720), ("1080p", 1920, 1080)] {
        let source = canvas(width, height);
        let mut group = c.benchmark_group(format!("rgb_to_yuv_{label}"));
        group.throughput(Throughput::Elements(u64::from(width) * u64::from(height)));

        for format in [PixelFormat::Nv12, PixelFormat::I420] {
            let mut target = destination(format, width, height);
            group.bench_function(format.name(), |b| {
                b.iter(|| {
                    rgb_to_yuv(
                        black_box(&source),
                        black_box(&mut target),
                        ColorSpec::BT709_LIMITED,
                    )
                });
            });
        }
        group.finish();
    }
}

criterion_group!(benches, conversion);
criterion_main!(benches);
