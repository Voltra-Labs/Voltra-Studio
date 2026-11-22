//! The meters every track carries, read through `Mixer::take_track_meter`.

use voltra_core::{ChannelLayout, Timestamp};

use super::{constant, settled, signal, spec};
use crate::{Balance, Gain, Meter, Mixer, PeakMode};

#[test]
fn the_input_meter_reads_what_a_meter_on_the_block_reads() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let mut meter = mixer.take_track_meter(track.id()).unwrap();
    meter.take();
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    track.set_gain(Gain::new(0.3).unwrap());
    mixer.mix([(track.id(), &input)], &mut out).unwrap();

    let (mut reference, mut reading) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
    reference.process(&input);
    assert_eq!(
        meter.take().input,
        reading.take(),
        "gain does not touch the input"
    );
}

#[test]
fn the_output_meter_follows_gain_balance_and_pan() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let mut meter = mixer.take_track_meter(track.id()).unwrap();
    let ones = constant(ChannelLayout::Stereo, 1.0);

    track.set_gain(Gain::new(0.5).unwrap());
    track.set_balance(Balance::LEFT);
    // One block to ramp, then a steady one to read.
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    meter.take();
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let output = meter.take().output;
    assert_eq!(output.channel(0).unwrap().peak, 0.5);
    assert!((output.channel(0).unwrap().rms - 0.5).abs() < 1e-6);
    assert_eq!(
        output.channel(1).unwrap().peak,
        0.0,
        "balance reaches the meter"
    );

    let (mut mixer, voice, mut out) = settled(ChannelLayout::Mono);
    let mut meter = mixer.take_track_meter(voice.id()).unwrap();
    let mono = constant(ChannelLayout::Mono, 0.8);
    mixer.mix([(voice.id(), &mono)], &mut out).unwrap();
    meter.take();
    mixer.mix([(voice.id(), &mono)], &mut out).unwrap();
    let levels = meter.take();
    assert_eq!(levels.input.channel_count(), 1);
    assert_eq!(levels.output.channel_count(), 2);
    for side in levels.output.channels() {
        assert!((side.peak - 0.8 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }
}

#[test]
fn during_a_ramp_the_output_meter_takes_the_larger_gain() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let mut meter = mixer.take_track_meter(track.id()).unwrap();
    let ones = constant(ChannelLayout::Stereo, 1.0);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    meter.take();
    track.set_gain(Gain::new(0.25).unwrap());
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    assert_eq!(
        meter.take().output.channel(0).unwrap().peak,
        1.0,
        "never under-reads"
    );
}

#[test]
fn a_track_that_does_not_sound_lets_its_meters_fall() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let mut meter = mixer.take_track_meter(track.id()).unwrap();
    let loud = constant(ChannelLayout::Stereo, 0.9);
    let mut poisoned = constant(ChannelLayout::Stereo, 0.9);
    poisoned.channel_mut(0).unwrap()[3] = f32::NAN;
    let late = signal(
        ChannelLayout::Stereo,
        voltra_core::SampleRate::HZ_48000.pts(1),
    );

    for silent in [None, Some(&poisoned), Some(&late)] {
        mixer.mix([(track.id(), &loud)], &mut out).unwrap();
        meter.take();
        match silent {
            None => mixer.mix(std::iter::empty(), &mut out).unwrap(),
            Some(bad) => mixer.mix([(track.id(), bad)], &mut out).unwrap(),
        };
        let levels = meter.take();
        assert_eq!(levels.input.frames(), 64, "the block still counts as time");
        assert!(
            levels
                .input
                .channels()
                .iter()
                .all(|c| c.peak == 0.0 && c.rms == 0.0)
        );
        assert!(levels.output.channels().iter().all(|c| c.peak == 0.0));
    }
}

#[test]
fn a_track_meter_is_handed_out_once() {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let track = mixer.add_track(ChannelLayout::Stereo).unwrap();
    assert!(mixer.take_track_meter(track.id()).is_some());
    assert!(mixer.take_track_meter(track.id()).is_none());
    let gone = mixer.add_track(ChannelLayout::Stereo).unwrap();
    mixer.remove_track(gone.id());
    assert!(mixer.take_track_meter(gone.id()).is_none());
}

#[test]
fn a_meter_taken_late_has_seen_everything() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let mut click = constant(ChannelLayout::Stereo, 0.0);
    click.channel_mut(1).unwrap()[10] = 0.6;
    mixer.mix([(track.id(), &click)], &mut out).unwrap();
    let mut meter = mixer.take_track_meter(track.id()).unwrap();
    let levels = meter.take();
    assert_eq!(
        levels.input.frames(),
        2 * 64,
        "both blocks since the track began"
    );
    assert_eq!(levels.input.channel(1).unwrap().peak, 0.6);
}
