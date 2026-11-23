//! What resampling one block costs.
//!
//! A 1024-sample stereo block of input, the size a capture hands over, against
//! the 21.3 ms a 48 kHz block lasts.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_audio::Resampler;
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

const FRAMES: usize = 1024;

fn resample(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample 1024 stereo frames");
    for (name, from, to) in [
        ("44.1 to 48 kHz", SampleRate::HZ_44100, SampleRate::HZ_48000),
        ("48 to 44.1 kHz", SampleRate::HZ_48000, SampleRate::HZ_44100),
    ] {
        let mut resampler = Resampler::new(from, to, ChannelLayout::Stereo, FRAMES).unwrap();
        let mut input =
            AudioBuffer::new(from, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap();
        for (channel, samples) in input.channels_mut().enumerate() {
            for (k, sample) in samples.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let t = (k + 17 * channel) as f32;
                *sample = (t / 7.0).sin() * 0.5;
            }
        }
        let mut output = AudioBuffer::new(
            to,
            ChannelLayout::Stereo,
            resampler.max_output_frames(FRAMES),
            Timestamp::ZERO,
        )
        .unwrap();
        let mut consumed = 0u64;
        group.bench_function(name, |b| {
            b.iter(|| {
                input.set_pts(from.pts(consumed));
                consumed += FRAMES as u64;
                black_box(resampler.process(&input, &mut output).unwrap())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, resample);
criterion_main!(benches);
