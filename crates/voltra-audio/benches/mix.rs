//! What mixing a block costs.
//!
//! The unit is the block libobs mixes in — 1024 samples, 21.3 ms at 48 kHz.
//! Everything here has to fit inside that with room to spare, because the mix is
//! only one part of what the audio thread does.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use voltra_audio::{ChannelLayout, Gain, MixInput, Mixer, Resampler, SampleRate, TrackId};
use voltra_core::{AudioBuffer, Timestamp};

const RATE: SampleRate = SampleRate::HZ_48000;
const FRAMES: usize = 1024;

fn source() -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(RATE, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap();
    for (index, channel) in buffer.channels_mut().enumerate() {
        for (position, sample) in channel.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let value = (position as f32 / 128.0).sin() * 0.5;
            *sample = if index == 0 { value } else { -value };
        }
    }
    buffer
}

fn out() -> AudioBuffer {
    AudioBuffer::new(RATE, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap()
}

/// A mixer with `tracks` tracks, every ramp already settled.
fn settled(tracks: usize) -> (Mixer, Vec<TrackId>) {
    let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, tracks.max(1)).unwrap();
    let ids: Vec<TrackId> = (0..tracks).map(|_| mixer.add_track().unwrap().0).collect();
    (mixer, ids)
}

fn steady_state(c: &mut Criterion) {
    let samples = source();
    let mut group = c.benchmark_group("mix 1024 stereo frames");

    for tracks in [1usize, 4, 8, 16] {
        let (mut mixer, ids) = settled(tracks);
        let inputs: Vec<MixInput<'_>> = ids.iter().map(|id| MixInput::new(*id, &samples)).collect();
        let mut target = out();
        mixer.mix(&inputs, &mut target).unwrap();

        group.bench_function(format!("{tracks} tracks, settled"), |b| {
            b.iter(|| {
                mixer.mix(black_box(&inputs), &mut target).unwrap();
                black_box(target.as_slice()[0])
            });
        });
    }
    group.finish();
}

/// The ramp is a multiply-accumulate plus a step per sample instead of a plain
/// multiply-accumulate. This is what that costs — and the reason to have a fast
/// path for the settled case at all.
fn ramping(c: &mut Criterion) {
    let samples = source();
    let mut group = c.benchmark_group("mix 1024 stereo frames");

    let (mut mixer, ids) = settled(8);
    let faders: Vec<_> = (0..0).map(|_| ()).collect();
    drop(faders);
    let inputs: Vec<MixInput<'_>> = ids.iter().map(|id| MixInput::new(*id, &samples)).collect();
    let mut target = out();

    // Rebuild with handles so the gain can be moved every iteration.
    let mut mixer_with_faders = Mixer::new(RATE, ChannelLayout::Stereo, 8).unwrap();
    let mut ids2 = Vec::with_capacity(8);
    let mut faders = Vec::with_capacity(8);
    for _ in 0..8 {
        let (id, fader) = mixer_with_faders.add_track().unwrap();
        ids2.push(id);
        faders.push(fader);
    }
    let inputs2: Vec<MixInput<'_>> = ids2.iter().map(|id| MixInput::new(*id, &samples)).collect();

    group.bench_function("8 tracks, every gain ramping", |b| {
        let mut flip = false;
        b.iter(|| {
            flip = !flip;
            let gain = if flip { Gain::SILENT } else { Gain::UNITY };
            for fader in &faders {
                fader.set_gain(gain);
            }
            mixer_with_faders
                .mix(black_box(&inputs2), &mut target)
                .unwrap();
            black_box(target.as_slice()[0])
        });
    });

    // Keep the settled mixer alive so the two benchmarks share a shape.
    black_box(&mut mixer);
    black_box(&inputs);
    group.finish();
}

/// What converting a block between the two rates everyone uses costs.
fn resampling(c: &mut Criterion) {
    let mut group = c.benchmark_group("resample 1024 stereo frames");

    for (name, from, to) in [
        ("44.1 -> 48", SampleRate::HZ_44100, SampleRate::HZ_48000),
        ("48 -> 44.1", SampleRate::HZ_48000, SampleRate::HZ_44100),
    ] {
        let mut resampler = Resampler::new(from, to, ChannelLayout::Stereo, FRAMES).unwrap();
        let mut input =
            AudioBuffer::new(from, ChannelLayout::Stereo, FRAMES, Timestamp::ZERO).unwrap();
        for (index, channel) in input.channels_mut().enumerate() {
            for (position, sample) in channel.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let value = (position as f32 / 128.0).sin() * 0.5;
                *sample = if index == 0 { value } else { -value };
            }
        }
        let mut output =
            AudioBuffer::new(to, ChannelLayout::Stereo, FRAMES * 2, Timestamp::ZERO).unwrap();
        resampler.process(&input, &mut output).unwrap();

        group.bench_function(name, |b| {
            b.iter(|| {
                let produced = resampler.process(black_box(&input), &mut output).unwrap();
                black_box(produced)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, steady_state, ramping, resampling);
criterion_main!(benches);
