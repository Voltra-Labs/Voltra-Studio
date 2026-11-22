//! Quality, timing and robustness of the resampler.
//!
//! Quality is measured the way `docs/references/audio-resampling.md` §3 does:
//! fit a sinusoid at the exact frequency by least squares; the gain comes from
//! the fit and the error is everything left over — images, aliasing, rounding.

#![allow(clippy::float_cmp, clippy::cast_precision_loss)]

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::Resampler;
use crate::Error;

fn rate(hz: u32) -> SampleRate {
    SampleRate::new(hz).unwrap()
}

/// A mono tone of amplitude 1: samples `start .. start + len` at `fs`.
fn tone(fs: SampleRate, hz: f64, start: usize, len: usize) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation)]
    (start..start + len)
        .map(|n| (std::f64::consts::TAU * hz * n as f64 / f64::from(fs.hz())).sin() as f32)
        .collect()
}

/// Run `signal` through a fresh resampler in blocks of the given lengths,
/// cycling them, and return the concatenated output. Checks on the way that
/// every output block is stamped exactly where the previous one ended.
fn run(from: SampleRate, to: SampleRate, signal: &[f32], blocks: &[usize]) -> Vec<f32> {
    let largest = blocks.iter().copied().max().unwrap();
    let mut resampler = Resampler::new(from, to, ChannelLayout::Mono, largest).unwrap();
    let mut output = AudioBuffer::new(
        to,
        ChannelLayout::Mono,
        resampler.max_output_frames(largest),
        Timestamp::ZERO,
    )
    .unwrap();
    let mut out = Vec::new();
    let mut consumed = 0;
    for &len in blocks.iter().cycle() {
        if consumed >= signal.len() {
            break;
        }
        let len = len.min(signal.len() - consumed);
        let mut input =
            AudioBuffer::new(from, ChannelLayout::Mono, len, from.pts(consumed as u64)).unwrap();
        input
            .channel_mut(0)
            .unwrap()
            .copy_from_slice(&signal[consumed..consumed + len]);
        let produced = resampler.process(&input, &mut output).unwrap();
        if produced > 0 {
            assert_eq!(
                output.pts(),
                to.pts(out.len() as u64),
                "contiguous timestamps"
            );
            out.extend_from_slice(output.channel(0).unwrap());
        }
        consumed += len;
    }
    assert_eq!(resampler.discontinuities(), 0);
    out
}

/// `(gain, error)` of the best sinusoid at `hz` through `samples`, whose first
/// element is output sample `first` at `fs`; error relative to that sinusoid.
fn fit(samples: &[f32], hz: f64, fs: SampleRate, first: usize) -> (f64, f64) {
    let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let omega = std::f64::consts::TAU * hz / f64::from(fs.hz());
    for (k, &y) in samples.iter().enumerate() {
        let phase = omega * (first + k) as f64;
        let (s, c) = phase.sin_cos();
        let y = f64::from(y);
        ss += s * s;
        cc += c * c;
        sc += s * c;
        ys += y * s;
        yc += y * c;
    }
    let det = ss * cc - sc * sc;
    let a = (ys * cc - yc * sc) / det;
    let b = (yc * ss - ys * sc) / det;
    let gain = a.hypot(b);
    let residual: f64 = samples
        .iter()
        .enumerate()
        .map(|(k, &y)| {
            let (s, c) = (omega * (first + k) as f64).sin_cos();
            (f64::from(y) - a * s - b * c).powi(2)
        })
        .sum::<f64>()
        / samples.len() as f64;
    (gain, residual.sqrt() / (gain / 2f64.sqrt()))
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The steady part of a resampled tone: past the start-up and before the end.
fn steady(out: &[f32]) -> (&[f32], usize) {
    (&out[1_000..out.len() - 1_000], 1_000)
}

#[test]
fn passband_is_flat_and_clean_both_ways() {
    for (from, to) in [(44_100, 48_000), (48_000, 44_100)] {
        let (from, to) = (rate(from), rate(to));
        for hz in [1_000.0, 10_000.0, 18_000.0, 20_000.0] {
            let out = run(from, to, &tone(from, hz, 0, 12_000), &[1024]);
            let (part, first) = steady(&out);
            let (gain, error) = fit(part, hz, to, first);
            assert!(
                db(gain).abs() < 0.05,
                "{from}->{to} {hz} Hz: gain {} dB",
                db(gain)
            );
            assert!(
                db(error) < -110.0,
                "{from}->{to} {hz} Hz: error {} dB",
                db(error)
            );
        }
    }
}

#[test]
fn content_above_the_output_nyquist_does_not_fold_back() {
    let (from, to) = (rate(48_000), rate(44_100));
    let out = run(from, to, &tone(from, 23_000.0, 0, 12_000), &[1024]);
    let (part, _) = steady(&out);
    let rms = (part.iter().map(|&y| f64::from(y).powi(2)).sum::<f64>() / part.len() as f64).sqrt();
    assert!(
        db(rms / 0.5f64.sqrt()) < -100.0,
        "alias at {} dB",
        db(rms / 0.5f64.sqrt())
    );
}

#[test]
fn output_is_aligned_with_input_in_time() {
    // No phase fitted: output sample k must be the input tone at k / 48 kHz.
    let (from, to) = (rate(44_100), rate(48_000));
    let out = run(from, to, &tone(from, 1_000.0, 0, 12_000), &[1024]);
    let (part, first) = steady(&out);
    let ideal = tone(to, 1_000.0, first, part.len());
    let error = (part
        .iter()
        .zip(&ideal)
        .map(|(&y, &x)| f64::from(y - x).powi(2))
        .sum::<f64>()
        / part.len() as f64)
        .sqrt();
    assert!(
        db(error / 0.5f64.sqrt()) < -100.0,
        "misaligned: {} dB",
        db(error / 0.5f64.sqrt())
    );
}

#[test]
fn splitting_into_blocks_changes_nothing() {
    let (from, to) = (rate(44_100), rate(48_000));
    let signal = tone(from, 3_333.0, 0, 9_000);
    let whole = run(from, to, &signal, &[1024]);
    for blocks in [&[1usize, 7, 100, 1500][..], &[333], &[2048]] {
        assert_eq!(run(from, to, &signal, blocks), whole, "blocks {blocks:?}");
    }
}

#[test]
fn ten_seconds_do_not_drift() {
    let (from, to) = (rate(44_100), rate(48_000));
    let input = 441_000usize;
    let out = run(from, to, &vec![0.25; input], &[1024]);
    // Exactly what the ratio allows, less the look-ahead still held back.
    let ideal = input * 160 / 147;
    assert!(
        out.len() <= ideal && ideal - out.len() <= 80,
        "{} for {ideal}",
        out.len()
    );
}

#[test]
fn a_jump_in_the_input_restarts_the_stream() {
    let (from, to) = (rate(44_100), rate(48_000));
    let mut resampler = Resampler::new(from, to, ChannelLayout::Mono, 1024).unwrap();
    let mut output = AudioBuffer::new(
        to,
        ChannelLayout::Mono,
        resampler.max_output_frames(1024),
        Timestamp::ZERO,
    )
    .unwrap();
    let first = AudioBuffer::new(from, ChannelLayout::Mono, 1024, Timestamp::ZERO).unwrap();
    resampler.process(&first, &mut output).unwrap();

    // Half a sample of rounding is continuity, not a jump.
    let nudged = AudioBuffer::new(
        from,
        ChannelLayout::Mono,
        1024,
        Timestamp::from_nanos(from.pts(1024).as_nanos() + 5_000),
    )
    .unwrap();
    resampler.process(&nudged, &mut output).unwrap();
    assert_eq!(resampler.discontinuities(), 0);

    let jumped = Timestamp::from_millis(10_000);
    let late = AudioBuffer::new(from, ChannelLayout::Mono, 1024, jumped).unwrap();
    resampler.process(&late, &mut output).unwrap();
    assert_eq!(resampler.discontinuities(), 1);
    assert_eq!(output.pts(), jumped, "the new run starts at its own time");
}

#[test]
fn a_tiny_first_block_produces_nothing_and_touches_nothing() {
    let (from, to) = (rate(44_100), rate(48_000));
    let mut resampler = Resampler::new(from, to, ChannelLayout::Mono, 1024).unwrap();
    let mut output = AudioBuffer::new(to, ChannelLayout::Mono, 1200, Timestamp::ZERO).unwrap();
    output.as_mut_slice().fill(7.0);
    let tiny = AudioBuffer::new(from, ChannelLayout::Mono, 10, Timestamp::ZERO).unwrap();
    assert_eq!(resampler.process(&tiny, &mut output).unwrap(), 0);
    assert_eq!(output.frames(), 1200);
    assert!(output.as_slice().iter().all(|&s| s == 7.0));
}

#[test]
fn wrong_blocks_are_refused_without_side_effects() {
    let (from, to) = (rate(44_100), rate(48_000));
    let mut resampler = Resampler::new(from, to, ChannelLayout::Stereo, 512).unwrap();
    let good = AudioBuffer::new(from, ChannelLayout::Stereo, 512, Timestamp::ZERO).unwrap();
    let mut output = AudioBuffer::new(
        to,
        ChannelLayout::Stereo,
        resampler.max_output_frames(512),
        Timestamp::ZERO,
    )
    .unwrap();

    let wrong_rate = AudioBuffer::new(to, ChannelLayout::Stereo, 512, Timestamp::ZERO).unwrap();
    assert!(matches!(
        resampler.process(&wrong_rate, &mut output),
        Err(Error::FormatMismatch { .. })
    ));
    let mut mono_out = AudioBuffer::new(to, ChannelLayout::Mono, 2048, Timestamp::ZERO).unwrap();
    assert!(matches!(
        resampler.process(&good, &mut mono_out),
        Err(Error::FormatMismatch { .. })
    ));
    let long = AudioBuffer::new(from, ChannelLayout::Stereo, 513, Timestamp::ZERO).unwrap();
    assert_eq!(
        resampler.process(&long, &mut output).unwrap_err(),
        Error::BlockTooLarge {
            frames: 513,
            max: 512
        }
    );
    let mut small = AudioBuffer::new(to, ChannelLayout::Stereo, 100, Timestamp::ZERO).unwrap();
    assert!(matches!(
        resampler.process(&good, &mut small),
        Err(Error::OutputTooSmall { .. })
    ));

    // None of that consumed anything: the stream still starts at zero.
    resampler.process(&good, &mut output).unwrap();
    assert_eq!(output.pts(), Timestamp::ZERO);
    assert_eq!(resampler.discontinuities(), 0);
}

#[test]
fn stereo_channels_are_independent() {
    let (from, to) = (rate(48_000), rate(44_100));
    let mut resampler = Resampler::new(from, to, ChannelLayout::Stereo, 1024).unwrap();
    let mut input = AudioBuffer::new(from, ChannelLayout::Stereo, 1024, Timestamp::ZERO).unwrap();
    input.channel_mut(0).unwrap().fill(0.5);
    let mut output = AudioBuffer::new(
        to,
        ChannelLayout::Stereo,
        resampler.max_output_frames(1024),
        Timestamp::ZERO,
    )
    .unwrap();
    resampler.process(&input, &mut output).unwrap();
    assert!(output.channel(1).unwrap().iter().all(|&s| s == 0.0));
    // Past the onset, DC passes at exactly the per-phase unity gain.
    let left = output.channel(0).unwrap();
    assert!(
        left[200..].iter().all(|&s| (s - 0.5).abs() < 1e-5),
        "{}",
        left[200]
    );
}

#[test]
fn latency_is_the_look_ahead() {
    let resampler = Resampler::new(rate(44_100), rate(48_000), ChannelLayout::Mono, 1024).unwrap();
    // 136 taps centred at 67: 68 samples ahead, 1.54 ms at 44.1 kHz.
    assert_eq!(resampler.latency(), rate(44_100).duration_of(68));
    assert!(Resampler::new(rate(44_100), rate(48_000), ChannelLayout::Mono, 0).is_err());
}
