//! What carrying one block from a source to the mix costs.
//!
//! `push` (the source's side) and `pull` (the mix's side) are timed apart with
//! `iter_custom`, each running the other untimed so the ring never fills.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use voltra_audio::{InputSpec, MixSpec, audio_input};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

const MIX: SampleRate = SampleRate::HZ_48000;

fn input(c: &mut Criterion) {
    let spec = MixSpec::new(MIX, ChannelLayout::Stereo, 1024).unwrap();
    let mut group = c.benchmark_group("input, 1024 stereo frames");
    for (name, source) in [("48 kHz", MIX), ("44.1 kHz", SampleRate::HZ_44100)] {
        // A source block of the same duration as a mix block, more or less.
        let len = if source == MIX { 1024 } else { 941 };
        for side in ["push", "pull"] {
            let (mut writer, mut reader) =
                audio_input(InputSpec::new(source, ChannelLayout::Stereo, len, spec)).unwrap();
            let mut block =
                AudioBuffer::new(source, ChannelLayout::Stereo, len, Timestamp::ZERO).unwrap();
            block.as_mut_slice().fill(0.25);
            let mut out = spec.buffer(Timestamp::ZERO).unwrap();
            let mut pushed = 0u64;
            let mut tick = 0u64;
            group.bench_function(format!("{side}, source at {name}"), |b| {
                b.iter_custom(|iterations| {
                    let mut timed = Duration::ZERO;
                    for _ in 0..iterations {
                        block.set_pts(source.pts(pushed));
                        let started = Instant::now();
                        writer.push(&block).unwrap();
                        if side == "push" {
                            timed += started.elapsed();
                        }
                        pushed += len as u64;
                        // Keep the mix a few blocks behind the source.
                        while MIX.pts((tick + 4) * 1024) < source.pts(pushed) {
                            let started = Instant::now();
                            black_box(reader.pull(MIX.pts(tick * 1024), &mut out).unwrap());
                            if side == "pull" {
                                timed += started.elapsed();
                            }
                            tick += 1;
                        }
                    }
                    timed
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, input);
criterion_main!(benches);
