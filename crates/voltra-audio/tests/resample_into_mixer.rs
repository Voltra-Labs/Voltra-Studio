//! The thing this step exists for: a 44.1 kHz source reaching a 48 kHz mixer.
//!
//! Plan 012 made the mixer refuse a track whose rate does not match, on the
//! grounds that resampling it silently would be worse than saying no. This is
//! the test that the refusal now has an answer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use voltra_audio::{ChannelLayout, Gain, MixInput, Mixer, Resampler, SampleRate};
use voltra_core::{AudioBuffer, Timestamp};

const SOURCE: SampleRate = SampleRate::HZ_44100;
const MIX: SampleRate = SampleRate::HZ_48000;

fn tone(rate: SampleRate, frames: usize, hertz: f32, offset: usize) -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(rate, ChannelLayout::Stereo, frames, Timestamp::ZERO).unwrap();
    #[allow(clippy::cast_precision_loss)]
    let step = hertz / rate.hz() as f32 * std::f32::consts::TAU;
    for channel in buffer.channels_mut() {
        for (index, sample) in channel.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let phase = step * (index + offset) as f32;
            *sample = phase.sin() * 0.5;
        }
    }
    buffer
}

/// Without the resampler this is the rejection path of plan 012; with it, the
/// track mixes.
#[test]
fn a_forty_four_one_source_reaches_a_forty_eight_mixer() {
    let mut mixer = Mixer::new(MIX, ChannelLayout::Stereo, 4).unwrap();
    let (id, fader) = mixer.add_track().unwrap();
    fader.set_gain(Gain::UNITY);

    let mut resampler = Resampler::new(SOURCE, MIX, ChannelLayout::Stereo, 1024).unwrap();
    let mut converted =
        AudioBuffer::new(MIX, ChannelLayout::Stereo, 2048, Timestamp::ZERO).unwrap();
    let mut out = AudioBuffer::new(MIX, ChannelLayout::Stereo, 2048, Timestamp::ZERO).unwrap();

    // The unconverted source is still refused, which is the behaviour plan 012
    // chose and this step does not change.
    let raw = tone(SOURCE, 1024, 1_000.0, 0);
    mixer.mix(&[MixInput::new(id, &raw)], &mut out).unwrap();
    assert_eq!(mixer.stats().rejected, 1);
    assert_eq!(mixer.stats().mixed, 0);

    // Converted, it mixes.
    let mut consumed = 0usize;
    let mut produced = 0usize;
    for _ in 0..8 {
        let input = tone(SOURCE, 1024, 1_000.0, consumed);
        produced = resampler.process(&input, &mut converted).unwrap();
        consumed += 1024;
        mixer
            .mix(&[MixInput::new(id, &converted)], &mut out)
            .unwrap();
    }

    assert_eq!(mixer.stats().rejected, 0);
    assert_eq!(mixer.stats().mixed, 1);
    assert!(produced > 1_000);

    // The meter sees the signal, at roughly the level it went in at.
    let levels = fader.levels(2);
    assert!(
        (levels.peak() - 0.5).abs() < 0.02,
        "the track peaked at {} after resampling",
        levels.peak()
    );
}

/// The resampler is late by a fixed amount, and says so. A caller that ignores
/// this shifts audio against video by a constant — lips that never quite match.
#[test]
fn the_resampler_declares_the_delay_it_adds() {
    let resampler = Resampler::new(SOURCE, MIX, ChannelLayout::Stereo, 1024).unwrap();
    let latency = resampler.latency();

    assert!(latency > std::time::Duration::ZERO);
    // A 32-tap filter at 48 kHz is well under a millisecond: small, but constant
    // and therefore exactly the kind of offset that never averages out.
    assert!(latency < std::time::Duration::from_millis(2), "{latency:?}");
    // The two ways of asking agree to within one sample, and cannot do better:
    // `latency()` rounds down to whole nanoseconds, and converting that back
    // rounds down again. The frame count is the exact figure; the duration is
    // the convenience.
    let round_tripped = usize::try_from(MIX.frames_in(latency)).unwrap();
    assert!(
        resampler.latency_frames().abs_diff(round_tripped) <= 1,
        "{} frames became {round_tripped} through a Duration",
        resampler.latency_frames()
    );
}
