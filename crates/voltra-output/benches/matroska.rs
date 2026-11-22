//! What muxing one frame and one audio block costs, against `io::sink()` so
//! the number is the muxer's and not the disk's.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_core::{
    AudioBuffer, ChannelLayout, Fps, FrameSize, PixelFormat, SampleRate, Timestamp, VideoFrame,
};
use voltra_output::{MatroskaWriter, TrackSpec};

fn matroska(c: &mut Criterion) {
    let size = FrameSize::new(1920, 1080).unwrap();
    let fps = Fps::FPS_60;
    let rate = SampleRate::HZ_48000;
    let tracks = [
        TrackSpec::RawVideo { size, fps },
        TrackSpec::PcmAudio {
            rate,
            layout: ChannelLayout::Stereo,
        },
    ];
    let frame = VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO).unwrap();
    let mut block = AudioBuffer::new(rate, ChannelLayout::Stereo, 1024, Timestamp::ZERO).unwrap();

    let mut group = c.benchmark_group("matroska");
    let mut writer = MatroskaWriter::new(std::io::sink(), &tracks).unwrap();
    let mut n = 0u64;
    group.bench_function("I420 1080p frame", |b| {
        b.iter(|| {
            writer.write_video(0, &frame, fps.pts(n)).unwrap();
            n += 1;
        });
    });
    let mut writer = MatroskaWriter::new(std::io::sink(), &tracks).unwrap();
    let mut k = 0u64;
    group.bench_function("1024 stereo float samples", |b| {
        b.iter(|| {
            block.set_pts(rate.pts(k * 1024));
            writer.write_audio(1, black_box(&block)).unwrap();
            k += 1;
        });
    });
    group.finish();
}

criterion_group!(benches, matroska);
criterion_main!(benches);
