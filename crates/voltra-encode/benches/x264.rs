//! What x264 `veryfast` costs per frame, through libavcodec, at the sizes
//! that matter. Needs `--features ffmpeg`.
//!
//! Throughput, not latency: x264 looks ahead and works on several frames at
//! once, so the time per call in steady state is the cost per frame of the
//! pipeline.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)]

use criterion::{Criterion, criterion_group, criterion_main};

use voltra_core::{Fps, FrameSize, PixelFormat, Timestamp, VideoFrame};
use voltra_encode::{
    EncodedPacket, EncoderInput, FfmpegVideoEncoder, Result, VideoEncoder, VideoSettings,
};

fn x264(c: &mut Criterion) {
    let mut group = c.benchmark_group("x264 veryfast, per frame");
    group.sample_size(30);
    for (name, width, height) in [("720p", 1280, 720), ("1080p", 1920, 1080)] {
        let size = FrameSize::new(width, height).unwrap();
        let fps = Fps::FPS_60;
        let mut encoder = FfmpegVideoEncoder::h264(VideoSettings::new(size, fps)).unwrap();
        let mut frame = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).unwrap();
        let mut n = 0u64;
        let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
            std::hint::black_box(packet.data.len());
            Ok(())
        };
        group.bench_function(name, |b| {
            b.iter(|| {
                // A moving gradient: content that changes, as a scene does.
                if let Some(luma) = frame.plane_mut(0) {
                    for (index, byte) in luma.iter_mut().enumerate().step_by(7) {
                        *byte = ((index as u64 + n * 5) % 220 + 16) as u8;
                    }
                }
                frame.set_pts(fps.pts(n));
                n += 1;
                encoder
                    .encode(EncoderInput::Frame(&frame), &mut sink)
                    .unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, x264);
criterion_main!(benches);
