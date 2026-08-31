//! What crossing the audio edge costs.
//!
//! The unit is the block libobs mixes in — 1024 samples, 21.3 ms at 48 kHz —
//! because that is the real unit of work, not an arbitrary round number.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_core::{
    AudioBuffer, ChannelLayout, SampleFormat, SampleOrder, SampleRate, SampleSpec, Timestamp,
};

/// One block of stereo at 48 kHz, filled with something that is not zero.
fn block(frames: usize) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Stereo,
        frames,
        Timestamp::ZERO,
    )
    .unwrap();
    for (index, channel) in buffer.channels_mut().enumerate() {
        for (position, sample) in channel.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let value = (position as f32 / 128.0).sin() * 0.7;
            *sample = if index == 0 { value } else { -value };
        }
    }
    buffer
}

fn edge_conversion(c: &mut Criterion) {
    const FRAMES: usize = 1024;
    let source = block(FRAMES);

    let cases = [
        ("i16 interleaved", SampleSpec::I16_INTERLEAVED),
        (
            "i16 planar",
            SampleSpec::new(SampleFormat::I16, SampleOrder::Planar),
        ),
        ("f32 interleaved", SampleSpec::F32_INTERLEAVED),
        (
            "f32 planar",
            SampleSpec::new(SampleFormat::F32, SampleOrder::Planar),
        ),
    ];

    let mut group = c.benchmark_group("audio edge, 1024 stereo frames");
    for (name, spec) in cases {
        let mut bytes = vec![0u8; spec.byte_len(FRAMES, 2)];
        source.write_to(&mut bytes, spec).unwrap();

        group.bench_function(format!("decode {name}"), |b| {
            let mut target = block(FRAMES);
            b.iter(|| {
                target.fill_from(black_box(&bytes), spec).unwrap();
                black_box(target.as_slice()[0])
            });
        });

        group.bench_function(format!("encode {name}"), |b| {
            let mut out = vec![0u8; spec.byte_len(FRAMES, 2)];
            b.iter(|| {
                source.write_to(black_box(&mut out), spec).unwrap();
                black_box(out[0])
            });
        });
    }
    group.finish();
}

fn buffer_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio buffer");

    group.bench_function("allocate 1024 stereo", |b| {
        b.iter(|| {
            black_box(
                AudioBuffer::new(
                    SampleRate::HZ_48000,
                    ChannelLayout::Stereo,
                    black_box(1024),
                    Timestamp::ZERO,
                )
                .unwrap(),
            )
        });
    });

    group.bench_function("silence 1024 stereo", |b| {
        let mut buffer = block(1024);
        b.iter(|| {
            buffer.silence();
            black_box(buffer.as_slice()[0])
        });
    });

    group.finish();
}

criterion_group!(benches, edge_conversion, buffer_lifecycle);
criterion_main!(benches);
