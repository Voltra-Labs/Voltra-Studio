//! What mixing one block costs.
//!
//! The unit is 1024 samples at 48 kHz — the block libobs mixes, 21.3 ms — so a
//! result reads directly against the callback's budget.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_audio::{Gain, MixSpec, Mixer, TrackControl};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

const FRAMES: usize = 1024;

fn spec() -> MixSpec {
    MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, FRAMES).unwrap()
}

fn block(layout: ChannelLayout, seed: usize) -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(SampleRate::HZ_48000, layout, FRAMES, Timestamp::ZERO).unwrap();
    for (channel, samples) in buffer.channels_mut().enumerate() {
        for (k, sample) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let phase = (k + 31 * channel + 7 * seed) as f32;
            *sample = (phase / 64.0).sin() * 0.3;
        }
    }
    buffer
}

/// A mixer with `count` tracks of `layout`, their blocks, and an output.
fn rig(
    count: usize,
    layout: ChannelLayout,
) -> (Mixer, Vec<TrackControl>, Vec<AudioBuffer>, AudioBuffer) {
    let mut mixer = Mixer::new(spec()).unwrap();
    let controls: Vec<_> = (0..count)
        .map(|_| mixer.add_track(layout).unwrap())
        .collect();
    for (index, control) in controls.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        control.set_gain(Gain::new(0.2 + 0.05 * (index % 8) as f32).unwrap());
    }
    let blocks = (0..count).map(|seed| block(layout, seed)).collect();
    let out = spec().buffer(Timestamp::ZERO).unwrap();
    (mixer, controls, blocks, out)
}

fn steady(c: &mut Criterion) {
    let mut group = c.benchmark_group("mix 1024 frames");
    for (name, count, layout) in [
        ("1 stereo track, steady", 1, ChannelLayout::Stereo),
        ("8 stereo tracks, steady", 8, ChannelLayout::Stereo),
        ("64 stereo tracks, steady", 64, ChannelLayout::Stereo),
        ("8 mono tracks panned, steady", 8, ChannelLayout::Mono),
    ] {
        let (mut mixer, controls, blocks, mut out) = rig(count, layout);
        group.bench_function(name, |b| {
            b.iter(|| {
                let inputs = controls.iter().map(TrackControl::id).zip(&blocks);
                black_box(mixer.mix(inputs, &mut out).unwrap());
            });
        });
    }

    // Every track's gain changes every block: a whole desk of faders dragged.
    let (mut mixer, controls, blocks, mut out) = rig(8, ChannelLayout::Stereo);
    let loud = Gain::new(0.8).unwrap();
    let quiet = Gain::new(0.2).unwrap();
    let mut flip = false;
    group.bench_function("8 stereo tracks, all ramping", |b| {
        b.iter(|| {
            flip = !flip;
            for control in &controls {
                control.set_gain(if flip { loud } else { quiet });
            }
            let inputs = controls.iter().map(TrackControl::id).zip(&blocks);
            black_box(mixer.mix(inputs, &mut out).unwrap());
        });
    });

    // The isolation check alone, by mixing a track whose block is rejected for
    // a NaN in its last sample: the whole block is scanned, nothing is summed.
    let (mut mixer, controls, mut blocks, mut out) = rig(8, ChannelLayout::Stereo);
    for block in &mut blocks {
        if let Some(last) = block.as_mut_slice().last_mut() {
            *last = f32::NAN;
        }
    }
    group.bench_function("8 stereo tracks, finite check only", |b| {
        b.iter(|| {
            let inputs = controls.iter().map(TrackControl::id).zip(&blocks);
            black_box(mixer.mix(inputs, &mut out).unwrap());
        });
    });
    group.finish();
}

criterion_group!(benches, steady);
criterion_main!(benches);
