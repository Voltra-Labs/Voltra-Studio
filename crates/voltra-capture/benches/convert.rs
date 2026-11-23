//! The copy a shared-memory capture costs: a 1080p system buffer, rows padded
//! to a 64-byte stride as GPUs and compositors like them, into a BGRA frame.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use voltra_capture::{SourceFormat, copy_to_bgra};
use voltra_core::{FrameSize, PixelFormat, Timestamp, VideoFrame};

fn copy(c: &mut Criterion) {
    let (width, height) = (1920u32, 1080u32);
    let stride = (width as usize * 4).next_multiple_of(64) + 64;
    let source = vec![0x7Fu8; stride * height as usize];
    let size = FrameSize::new(width, height).unwrap();
    let mut frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).unwrap();
    let mut group = c.benchmark_group("capture_copy_1080p");
    group.throughput(Throughput::Bytes(u64::from(width * height * 4)));
    for (name, format) in [
        ("bgra", SourceFormat::Bgra),
        ("bgrx", SourceFormat::Bgrx),
        ("rgbx", SourceFormat::Rgbx),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| copy_to_bgra(&source, stride, format, &mut frame));
        });
    }
    group.finish();
}

criterion_group!(benches, copy);
criterion_main!(benches);
