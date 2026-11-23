//! What metering one block costs.
//!
//! 1024 stereo samples at 48 kHz, the block the mixer produces, read against
//! its 21.3 ms.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_audio::{Meter, PeakMode};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

fn programme() -> AudioBuffer {
    let mut block = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Stereo,
        1024,
        Timestamp::ZERO,
    )
    .unwrap();
    for (channel, samples) in block.channels_mut().enumerate() {
        for (k, sample) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = (k + 13 * channel) as f32;
            *sample = (t / 9.0).sin() * 0.4 + (t / 2.3).sin() * 0.2;
        }
    }
    block
}

fn meter(c: &mut Criterion) {
    let block = programme();
    let mut group = c.benchmark_group("meter 1024 stereo frames");
    for (name, mode) in [
        ("sample peak", PeakMode::Sample),
        ("true peak", PeakMode::True),
    ] {
        let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, mode);
        group.bench_function(name, |b| {
            b.iter(|| black_box(meter.process(&block)));
        });
        black_box(reader.take());
    }
    group.finish();
}

criterion_group!(benches, meter);
criterion_main!(benches);
