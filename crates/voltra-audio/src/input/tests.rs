//! The placement rules, one at a time, and the three outcomes of a tick.

#![allow(
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::time::Duration;

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::{InputReader, InputSpec, InputWriter, PullOutcome, audio_input};
use crate::{Error, MixSpec};

const TICK: usize = 64;
const RATE: SampleRate = SampleRate::HZ_48000;

fn mix() -> MixSpec {
    MixSpec::new(RATE, ChannelLayout::Mono, TICK).unwrap()
}

/// An input at the mix rate, mono, taking blocks of up to 200 samples.
fn input(smoothing: Duration) -> (InputWriter, InputReader) {
    let mut spec = InputSpec::new(RATE, ChannelLayout::Mono, 200, mix());
    spec.smoothing = smoothing;
    audio_input(spec).unwrap()
}

/// A block whose sample `k` holds `value(first + k)`, stamped at `stamp`.
fn block(first: u64, len: usize, stamp: Timestamp) -> AudioBuffer {
    let mut buffer = AudioBuffer::new(RATE, ChannelLayout::Mono, len, stamp).unwrap();
    for (k, sample) in buffer.channel_mut(0).unwrap().iter_mut().enumerate() {
        *sample = value(first + k as u64);
    }
    buffer
}

/// A distinct, exactly representable value per index.
fn value(index: u64) -> f32 {
    (index % 100_000) as f32 + 1.0
}

/// A block at its own exact time.
fn at(first: u64, len: usize) -> AudioBuffer {
    block(first, len, RATE.pts(first))
}

fn tick_block() -> AudioBuffer {
    AudioBuffer::new(RATE, ChannelLayout::Mono, TICK, Timestamp::ZERO).unwrap()
}

/// Pull the tick starting at sample `first`.
fn pull(reader: &mut InputReader, first: u64) -> (PullOutcome, Vec<f32>) {
    let mut out = tick_block();
    let outcome = reader.pull(RATE.pts(first), &mut out).unwrap();
    (outcome, out.channel(0).unwrap().to_vec())
}

fn expected(first: u64) -> Vec<f32> {
    (first..first + TICK as u64).map(value).collect()
}

#[test]
fn jittery_contiguous_blocks_come_out_seamless() {
    let (mut writer, mut reader) = input(InputSpec::DEFAULT_SMOOTHING);
    let base = 48_000 * 3_600; // an hour into the monotonic clock
    let mut first = base;
    for (n, len) in [100usize, 37, 200, 64, 150, 99, 1].iter().enumerate() {
        // ±2 ms of jitter on the claimed time.
        let jitter = Duration::from_millis(2);
        let truth = RATE.pts(first);
        let stamp = if n % 2 == 0 {
            Timestamp::from_nanos(truth.as_nanos() + jitter.as_nanos() as u64)
        } else {
            Timestamp::from_nanos(truth.as_nanos().saturating_sub(jitter.as_nanos() as u64))
        };
        writer
            .push(&block(first, *len, if n == 0 { truth } else { stamp }))
            .unwrap();
        first += *len as u64;
    }
    for tick in 0..(first - base) / TICK as u64 {
        let start = base + tick * TICK as u64;
        assert_eq!(
            pull(&mut reader, start),
            (PullOutcome::Full, expected(start)),
            "tick {tick}"
        );
    }
    let stats = reader.stats();
    assert_eq!(
        (stats.gaps, stats.overlap_samples, stats.discontinuities),
        (0, 0, 0)
    );
    assert_eq!(stats.max_jitter_ns, 2_000_000);
    assert_eq!(stats.blocks, 7);
}

#[test]
fn a_real_gap_is_silence_in_its_place() {
    let (mut writer, mut reader) = input(Duration::ZERO);
    writer.push(&at(10_000, 100)).unwrap();
    // 30 ms later than contiguous: 1440 samples of gap.
    writer.push(&at(10_100 + 1_440, 100)).unwrap();
    let stats = writer.stats();
    assert_eq!((stats.gaps, stats.gap_samples), (1, 1_440));

    let mut stream = Vec::new();
    for tick in 0..(100 + 1_440 + 100) / TICK as u64 {
        let (outcome, samples) = pull(&mut reader, 10_000 + tick * TICK as u64);
        assert_eq!(outcome, PullOutcome::Full);
        stream.extend(samples);
    }
    assert_eq!(
        &stream[..100],
        &(10_000..10_100).map(value).collect::<Vec<_>>()[..]
    );
    assert!(stream[100..1_540].iter().all(|&s| s == 0.0));
    assert_eq!(
        stream[1_540],
        value(11_540),
        "the sample after the gap lands exactly"
    );
}

#[test]
fn an_overlap_keeps_what_was_written_first() {
    let (mut writer, mut reader) = input(Duration::ZERO);
    writer.push(&at(20_000, 100)).unwrap();
    // Claims to start 50 samples back, with different contents.
    let mut late_claim = block(20_050, 100, RATE.pts(20_050));
    late_claim.channel_mut(0).unwrap().fill(-1.0);
    writer.push(&late_claim).unwrap();
    assert_eq!(writer.stats().overlap_samples, 50);

    let (_, first) = pull(&mut reader, 20_000);
    let (_, second) = pull(&mut reader, 20_064);
    let stream: Vec<f32> = first.into_iter().chain(second).collect();
    assert_eq!(
        &stream[..100],
        &(20_000..20_100).map(value).collect::<Vec<_>>()[..]
    );
    assert!(
        stream[100..128].iter().all(|&s| s == -1.0),
        "the rest follows on"
    );
}

#[test]
fn samples_whose_moment_was_mixed_are_late() {
    let (mut writer, mut reader) = input(Duration::ZERO);
    // The mix passes 30_000..30_064 before anything arrives.
    assert_eq!(pull(&mut reader, 30_000).0, PullOutcome::Empty);
    writer.push(&at(29_950, 150)).unwrap();
    assert_eq!(writer.stats().late_samples, 114, "29_950 .. 30_064");

    let (outcome, samples) = pull(&mut reader, 30_064);
    assert_eq!(outcome, PullOutcome::Partial);
    assert_eq!(
        &samples[..36],
        &(30_064..30_100).map(value).collect::<Vec<_>>()[..]
    );
    assert!(samples[36..].iter().all(|&s| s == 0.0));

    // That pull released up to 30_128, so 30_100 .. 30_128 of the next block
    // arrive late too; and what then waits in the ring while the mix jumps to
    // 30_192 is skipped: 30_128 .. 30_192.
    writer.push(&at(30_100, 100)).unwrap();
    assert_eq!(pull(&mut reader, 30_192).0, PullOutcome::Partial);
    assert_eq!(reader.stats().late_samples, 114 + 28 + 64);
}

#[test]
fn a_source_too_far_ahead_overflows() {
    let mut spec = InputSpec::new(RATE, ChannelLayout::Mono, 200, mix());
    spec.capacity = Duration::from_millis(1);
    let (mut writer, _reader) = audio_input(spec).unwrap();
    // The ring rounds up to 1024 samples; nobody reads. Ten blocks fit.
    let mut first = 40_000;
    for _ in 0..20 {
        writer.push(&at(first, 100)).unwrap();
        first += 100;
    }
    let stats = writer.stats();
    assert_eq!(stats.overflows, 10, "{stats:?}");
    assert_eq!(stats.blocks, 20);
    assert_eq!(stats.discontinuities, 0, "rounding is not a jump");
}

#[test]
fn ticks_are_empty_partial_or_full() {
    let (mut writer, mut reader) = input(Duration::ZERO);
    let mut out = tick_block();
    out.as_mut_slice().fill(9.0);
    assert_eq!(
        reader.pull(RATE.pts(50_000), &mut out).unwrap(),
        PullOutcome::Empty
    );
    assert!(
        out.as_slice().iter().all(|&s| s == 9.0),
        "an empty tick leaves out alone"
    );

    writer.push(&at(50_096, 32)).unwrap();
    let (outcome, samples) = pull(&mut reader, 50_064);
    assert_eq!(outcome, PullOutcome::Partial, "data starts mid-tick");
    assert!(samples[..32].iter().all(|&s| s == 0.0));
    assert_eq!(samples[32], value(50_096));

    writer.push(&at(50_128, 64)).unwrap();
    let mut out = tick_block();
    assert_eq!(
        reader.pull(RATE.pts(50_128), &mut out).unwrap(),
        PullOutcome::Full
    );
    assert_eq!(out.pts(), RATE.pts(50_128), "stamped with the tick");

    let stats = reader.stats();
    assert_eq!((stats.empty, stats.partial, stats.full), (1, 1, 1));
    assert_eq!(stats.headroom, 0);
    assert!(stats.min_headroom < 0);
}

#[test]
fn a_jump_beyond_the_smoothing_is_a_discontinuity() {
    let (mut writer, _reader) = input(Duration::from_millis(5));
    writer.push(&at(60_000, 100)).unwrap();
    writer
        .push(&block(
            60_100,
            100,
            Timestamp::from_nanos(RATE.pts(60_100).as_nanos() + 4_000_000),
        ))
        .unwrap();
    assert_eq!(writer.stats().discontinuities, 0, "4 ms is jitter");
    writer
        .push(&block(
            60_200,
            100,
            Timestamp::from_nanos(RATE.pts(60_200).as_nanos() + 6_000_000),
        ))
        .unwrap();
    assert_eq!(writer.stats().discontinuities, 1, "6 ms is a jump");
}

#[test]
fn wrong_blocks_are_refused() {
    let (mut writer, mut reader) = input(Duration::ZERO);
    let stereo = AudioBuffer::new(RATE, ChannelLayout::Stereo, 10, Timestamp::ZERO).unwrap();
    assert!(matches!(
        writer.push(&stereo),
        Err(Error::FormatMismatch { .. })
    ));
    let long = at(0, 201);
    assert!(matches!(
        writer.push(&long),
        Err(Error::BlockTooLarge {
            frames: 201,
            max: 200
        })
    ));
    let mut short = AudioBuffer::new(RATE, ChannelLayout::Mono, TICK - 1, Timestamp::ZERO).unwrap();
    assert!(matches!(
        reader.pull(Timestamp::ZERO, &mut short),
        Err(Error::OutputTooSmall { .. })
    ));
    assert_eq!(
        writer.stats().blocks,
        0,
        "refused blocks are not counted as pushed"
    );
}

#[test]
fn a_source_at_another_rate_is_resampled_into_whole_ticks() {
    let mix = MixSpec::new(RATE, ChannelLayout::Mono, 1024).unwrap();
    let source = SampleRate::HZ_44100;
    let (mut writer, mut reader) =
        audio_input(InputSpec::new(source, ChannelLayout::Mono, 441, mix)).unwrap();
    let base = 44_100 * 60;
    for n in 0..100u64 {
        let first = base + n * 441;
        let mut block =
            AudioBuffer::new(source, ChannelLayout::Mono, 441, source.pts(first)).unwrap();
        block.as_mut_slice().fill(0.5);
        writer.push(&block).unwrap();
    }
    // A second at 44.1 kHz is 48_000 samples at the mix rate: index 2_880_000.
    let mut out = AudioBuffer::new(RATE, ChannelLayout::Mono, 1024, Timestamp::ZERO).unwrap();
    let mut full = 0;
    for tick in 1..40u64 {
        let first = 48_000 * 60 + tick * 1024;
        if reader.pull(RATE.pts(first), &mut out).unwrap() == PullOutcome::Full {
            full += 1;
            assert!(out.as_slice().iter().all(|&s| (s - 0.5).abs() < 1e-4));
        }
    }
    assert!(full >= 38, "{full} full ticks: {:?}", reader.stats());
}
