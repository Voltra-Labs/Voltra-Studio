//! Behaviour of the mixer: sums, laws in context, ramps and isolation.

// The mixer promises bit-exact results where it can; the tests say so.
#![allow(clippy::float_cmp)]

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::{MixReport, MixSpec, Mixer};
use crate::{Balance, Error, Gain, TrackControl};

const FRAMES: usize = 64;

fn spec(layout: ChannelLayout) -> MixSpec {
    MixSpec::new(SampleRate::HZ_48000, layout, FRAMES).unwrap()
}

/// A block whose channel `c`, sample `k` is a distinct, non-trivial value.
fn signal(layout: ChannelLayout, pts: Timestamp) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(SampleRate::HZ_48000, layout, FRAMES, pts).unwrap();
    for (channel, samples) in buffer.channels_mut().enumerate() {
        for (k, sample) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "small integers")]
            let phase = (k + 17 * channel) as f32;
            *sample = (phase * 0.3).sin() * 0.5;
        }
    }
    buffer
}

fn constant(layout: ChannelLayout, value: f32) -> AudioBuffer {
    let mut buffer =
        AudioBuffer::new(SampleRate::HZ_48000, layout, FRAMES, Timestamp::ZERO).unwrap();
    buffer.as_mut_slice().fill(value);
    buffer
}

/// A stereo mixer with one track, already past its fade-in.
fn settled(layout: ChannelLayout) -> (Mixer, TrackControl, AudioBuffer) {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let track = mixer.add_track(layout).unwrap();
    let mut out = spec(ChannelLayout::Stereo).buffer(Timestamp::ZERO).unwrap();
    let input = signal(layout, Timestamp::ZERO);
    mixer.mix([(track.id(), &input)], &mut out).unwrap();
    (mixer, track, out)
}

#[test]
fn a_settled_track_at_unity_passes_bit_exact() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    let report = mixer.mix([(track.id(), &input)], &mut out).unwrap();
    assert_eq!(report.mixed, 1);
    assert_eq!(out.as_slice(), input.as_slice());
}

#[test]
fn tracks_sum_and_the_sum_is_not_clipped() {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let a = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let b = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let loud = constant(ChannelLayout::Stereo, 0.8);
    let mut out = spec(ChannelLayout::Stereo).buffer(Timestamp::ZERO).unwrap();
    for _ in 0..2 {
        mixer
            .mix([(a.id(), &loud), (b.id(), &loud)], &mut out)
            .unwrap();
    }
    assert!(out.as_slice().iter().all(|&sample| sample == 1.6));
}

#[test]
fn the_output_is_overwritten_not_accumulated() {
    let (mut mixer, _track, mut out) = settled(ChannelLayout::Stereo);
    out.as_mut_slice().fill(123.0);
    let report = mixer.mix(std::iter::empty(), &mut out).unwrap();
    assert_eq!(report.missing, 1);
    assert!(out.as_slice().iter().all(|&sample| sample == 0.0));
}

#[test]
fn a_new_track_fades_in_from_silence() {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let track = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let ones = constant(ChannelLayout::Stereo, 1.0);
    let mut out = spec(ChannelLayout::Stereo).buffer(Timestamp::ZERO).unwrap();
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let left = out.channel(0).unwrap();
    assert!(
        left[0] > 0.0 && left[0] <= 1.0 / 32.0,
        "starts one step above zero"
    );
    assert!((left[FRAMES - 1] - 1.0).abs() < 1e-6, "ends on unity");
    assert!(left.windows(2).all(|pair| pair[1] >= pair[0]), "monotonic");
}

#[test]
fn a_gain_change_is_a_linear_ramp_across_one_block() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let ones = constant(ChannelLayout::Stereo, 1.0);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();

    track.set_gain(Gain::new(0.5).unwrap());
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let left = out.channel(0).unwrap();
    let step = 0.5 / 64.0;
    for (k, &sample) in left.iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "small integers")]
        let expected = 1.0 - step * (k + 1) as f32;
        assert!(
            (sample - expected).abs() < 1e-6,
            "sample {k}: {sample} vs {expected}"
        );
    }

    // And the block after holds the new gain exactly.
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    assert!(out.as_slice().iter().all(|&sample| sample == 0.5));
}

#[test]
fn muting_fades_instead_of_clicking() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let ones = constant(ChannelLayout::Stereo, 1.0);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let last_before = out.channel(0).unwrap()[FRAMES - 1];

    track.set_muted(true);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let left = out.channel(0).unwrap();
    // On a constant signal, no step between neighbours exceeds one ramp step.
    let largest_step = std::iter::once(last_before)
        .chain(left.iter().copied())
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        largest_step <= 1.0 / 64.0 + 1e-6,
        "largest step {largest_step}"
    );
    assert_eq!(left[FRAMES - 1], 0.0);

    // Muting keeps the gain for unmuting.
    assert_eq!(track.gain(), Gain::UNITY);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    assert!(out.as_slice().iter().all(|&sample| sample == 0.0));
}

#[test]
fn stereo_balance_in_the_mix() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    track.set_balance(Balance::LEFT);
    for _ in 0..2 {
        mixer.mix([(track.id(), &input)], &mut out).unwrap();
    }
    assert_eq!(out.channel(0), input.channel(0));
    assert!(out.channel(1).unwrap().iter().all(|&sample| sample == 0.0));
}

#[test]
fn a_mono_track_is_panned_into_both_sides() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Mono);
    let ones = constant(ChannelLayout::Mono, 1.0);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    for channel in 0..2 {
        assert!(
            out.channel(channel)
                .unwrap()
                .iter()
                .all(|&sample| (sample - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6)
        );
    }
    track.set_balance(Balance::RIGHT);
    for _ in 0..2 {
        mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    }
    assert!(out.channel(0).unwrap().iter().all(|&sample| sample == 0.0));
    assert!(out.channel(1).unwrap().iter().all(|&sample| sample == 1.0));
}

/// Mix `good` alone and with `bad` alongside; the output must be identical.
fn bad_input_is_isolated(bad: &AudioBuffer, expect: MixReport) {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let good = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let broken = mixer.add_track(ChannelLayout::Stereo).unwrap();
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    let mut alone = spec(ChannelLayout::Stereo).buffer(Timestamp::ZERO).unwrap();
    let mut with_bad = alone.clone();

    let mut reference = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let reference_track = reference.add_track(ChannelLayout::Stereo).unwrap();
    reference
        .mix([(reference_track.id(), &input)], &mut alone)
        .unwrap();

    let report = mixer
        .mix([(good.id(), &input), (broken.id(), bad)], &mut with_bad)
        .unwrap();
    assert_eq!(
        with_bad.as_slice(),
        alone.as_slice(),
        "the good track is untouched"
    );
    assert_eq!(report, expect);
    assert_eq!(broken.faults(), 1);
    assert_eq!(good.faults(), 0);
}

#[test]
fn a_nan_drops_only_its_own_track() {
    let mut bad = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    bad.channel_mut(1).unwrap()[40] = f32::NAN;
    bad_input_is_isolated(
        &bad,
        MixReport {
            mixed: 1,
            non_finite: 1,
            ..MixReport::default()
        },
    );
}

#[test]
fn an_infinity_drops_only_its_own_track() {
    let mut bad = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    bad.channel_mut(0).unwrap()[3] = f32::NEG_INFINITY;
    bad_input_is_isolated(
        &bad,
        MixReport {
            mixed: 1,
            non_finite: 1,
            ..MixReport::default()
        },
    );
}

#[test]
fn a_misaligned_block_is_rejected() {
    let bad = signal(ChannelLayout::Stereo, SampleRate::HZ_48000.pts(1));
    bad_input_is_isolated(
        &bad,
        MixReport {
            mixed: 1,
            rejected: 1,
            ..MixReport::default()
        },
    );
}

#[test]
fn a_wrong_rate_or_length_is_rejected() {
    let other_rate = AudioBuffer::new(
        SampleRate::HZ_44100,
        ChannelLayout::Stereo,
        FRAMES,
        Timestamp::ZERO,
    )
    .unwrap();
    bad_input_is_isolated(
        &other_rate,
        MixReport {
            mixed: 1,
            rejected: 1,
            ..MixReport::default()
        },
    );
    let short = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Stereo,
        FRAMES - 1,
        Timestamp::ZERO,
    )
    .unwrap();
    bad_input_is_isolated(
        &short,
        MixReport {
            mixed: 1,
            rejected: 1,
            ..MixReport::default()
        },
    );
}

#[test]
fn a_layout_other_than_the_tracks_is_rejected() {
    let mut mixer = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    let mono = mixer.add_track(ChannelLayout::Mono).unwrap();
    let stereo_block = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    let mut out = spec(ChannelLayout::Stereo).buffer(Timestamp::ZERO).unwrap();
    let report = mixer.mix([(mono.id(), &stereo_block)], &mut out).unwrap();
    assert_eq!(report.rejected, 1);
    assert_eq!(mono.faults(), 1);
}

#[test]
fn unknown_and_duplicate_tracks_are_rejected() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    let gone = mixer.add_track(ChannelLayout::Stereo).unwrap();
    assert!(mixer.remove_track(gone.id()));
    assert!(!mixer.remove_track(gone.id()));

    let report = mixer
        .mix(
            [
                (track.id(), &input),
                (track.id(), &input),
                (gone.id(), &input),
            ],
            &mut out,
        )
        .unwrap();
    assert_eq!(
        report,
        MixReport {
            mixed: 1,
            rejected: 2,
            ..MixReport::default()
        }
    );
    assert_eq!(
        out.as_slice(),
        input.as_slice(),
        "the first copy counts once"
    );
}

#[test]
fn a_track_that_returns_fades_back_in() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let ones = constant(ChannelLayout::Stereo, 1.0);
    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let report = mixer.mix(std::iter::empty(), &mut out).unwrap();
    assert_eq!(report.missing, 1);
    assert_eq!(track.faults(), 0, "missing is not a fault");

    mixer.mix([(track.id(), &ones)], &mut out).unwrap();
    let left = out.channel(0).unwrap();
    assert!(left[0] < 0.05, "no click on return: {}", left[0]);
    assert!((left[FRAMES - 1] - 1.0).abs() < 1e-6);
}

#[test]
fn layouts_that_need_a_remix_are_refused_up_front() {
    let mut stereo = Mixer::new(spec(ChannelLayout::Stereo)).unwrap();
    assert_eq!(
        stereo.add_track(ChannelLayout::FivePoint1).unwrap_err(),
        Error::UnsupportedLayout {
            track: ChannelLayout::FivePoint1,
            mix: ChannelLayout::Stereo
        }
    );
    let mut mono = Mixer::new(spec(ChannelLayout::Mono)).unwrap();
    assert!(mono.add_track(ChannelLayout::Stereo).is_err());
    assert!(mono.add_track(ChannelLayout::Mono).is_ok());
}

#[test]
fn a_mismatched_output_is_an_error_and_is_left_alone() {
    let (mut mixer, track, _) = settled(ChannelLayout::Stereo);
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    let mut wrong = constant(ChannelLayout::Mono, 7.0);
    let error = mixer.mix([(track.id(), &input)], &mut wrong).unwrap_err();
    assert!(matches!(error, Error::OutputMismatch { .. }));
    assert!(wrong.as_slice().iter().all(|&sample| sample == 7.0));
}

#[test]
fn totals_accumulate_reports() {
    let (mut mixer, track, mut out) = settled(ChannelLayout::Stereo);
    let input = signal(ChannelLayout::Stereo, Timestamp::ZERO);
    mixer.mix([(track.id(), &input)], &mut out).unwrap();
    mixer.mix(std::iter::empty(), &mut out).unwrap();
    let totals = mixer.totals();
    assert_eq!((totals.blocks, totals.mixed, totals.missing), (3, 2, 1));
}

#[test]
fn an_empty_block_is_refused() {
    assert_eq!(
        MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 0).unwrap_err(),
        Error::EmptyBlock
    );
    let mut raw = spec(ChannelLayout::Stereo);
    raw.frames = 0;
    assert!(Mixer::new(raw).is_err());
}

#[test]
fn the_mixer_can_move_to_the_audio_thread() {
    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}
    send::<Mixer>();
    send_sync::<TrackControl>();
}

mod meters;
