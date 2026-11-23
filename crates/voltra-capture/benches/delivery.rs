//! The cost of handing a frame over: publish on one side, update on the other.
//! No pixels move, so the size of the frame should not matter.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use voltra_capture::frame_channel;
use voltra_core::{FrameSize, PixelFormat, Timestamp};

fn handover(c: &mut Criterion) {
    let mut group = c.benchmark_group("capture_handover");
    for (name, width, height) in [("64x36", 64, 36), ("1080p", 1920, 1080)] {
        let size = FrameSize::new(width, height).unwrap();
        let format = PixelFormat::Bgra8;
        let (mut writer, mut reader) = frame_channel(format, size).unwrap();
        let mut n = 0u64;
        group.bench_function(name, |b| {
            b.iter(|| {
                writer.frame_mut(format, size).unwrap();
                writer.publish(Timestamp::from_nanos(n));
                n += 1;
                black_box(reader.update());
                black_box(reader.frame());
            });
        });
    }
    group.finish();
}

criterion_group!(benches, handover);
criterion_main!(benches);
