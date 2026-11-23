//! AAC through libavcodec: blocks in, packets out, priming accounted for.

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

use super::{FfmpegAudioEncoder, sample_index};
use crate::{AudioEncoder, AudioSettings, EncodedPacket, Error, Result};

const RATE: SampleRate = SampleRate::HZ_48000;

fn settings() -> AudioSettings {
    AudioSettings::new(RATE, ChannelLayout::Stereo)
}

/// `frames` samples of a tone, starting at absolute sample `first`.
fn block(first: u64, frames: usize) -> AudioBuffer {
    let mut block = AudioBuffer::new(RATE, ChannelLayout::Stereo, frames, RATE.pts(first)).unwrap();
    for samples in block.channels_mut() {
        for (k, sample) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let t = (first + k as u64) as f64 / 48_000.0;
            #[allow(clippy::cast_possible_truncation)]
            {
                *sample = (std::f64::consts::TAU * 440.0 * t).sin() as f32 * 0.5;
            }
        }
    }
    block
}

/// Encode `total` samples from a second in, in blocks of `size`.
fn encode(total: usize, size: usize) -> (FfmpegAudioEncoder, Vec<Timestamp>) {
    let mut encoder = FfmpegAudioEncoder::aac(settings()).unwrap();
    let mut times = Vec::new();
    let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
        assert!(packet.keyframe, "every AAC packet decodes on its own");
        assert!(!packet.data.is_empty());
        times.push(packet.pts);
        Ok(())
    };
    let mut done = 0;
    while done < total {
        let len = size.min(total - done);
        encoder
            .encode(&block(48_000 + done as u64, len), &mut sink)
            .unwrap();
        done += len;
    }
    encoder.flush(&mut sink).unwrap();
    (encoder, times)
}

#[test]
fn aac_frames_are_1024_samples_with_1024_of_priming() {
    let encoder = FfmpegAudioEncoder::aac(settings()).unwrap();
    assert_eq!(encoder.info().frame_size, 1024);
    assert_eq!(encoder.info().delay, 1024);
    // AAC-LC (2), 48 kHz (index 3), two channels: 00010 0011 0010 000 — then
    // FFmpeg's backward-compatible extension saying "no SBR" (`56 E5 00`), the
    // same five bytes its own Matroska muxer stores.
    let headers = encoder.headers().unwrap();
    assert_eq!(headers, [0x11, 0x90, 0x56, 0xE5, 0x00]);
}

#[test]
fn packets_start_a_frame_before_the_input_and_cover_all_of_it() {
    // 10 000 samples in uneven blocks: 9 full frames and a short one.
    let (_, times) = encode(10_000, 700);
    // Each packet at its absolute sample: the first is the priming, a frame
    // before the input.
    for (k, &time) in times.iter().enumerate() {
        assert_eq!(
            time,
            RATE.pts(48_000 - 1024 + 1024 * k as u64),
            "packet {k}"
        );
    }
    // Priming plus ten frames of input, the last one short.
    assert_eq!(times.len(), 11, "{times:?}");
}

#[test]
fn block_sizes_do_not_change_the_packets() {
    let (_, small) = encode(6_000, 100);
    let (_, large) = encode(6_000, 4_096);
    assert_eq!(small, large);
}

#[test]
fn wrong_blocks_and_a_flushed_encoder_are_refused() {
    let mut encoder = FfmpegAudioEncoder::aac(settings()).unwrap();
    let mut sink = |_: EncodedPacket<'_>| -> Result<()> { Ok(()) };
    let mono = AudioBuffer::new(RATE, ChannelLayout::Mono, 64, Timestamp::ZERO).unwrap();
    assert!(matches!(
        encoder.encode(&mono, &mut sink),
        Err(Error::Settings(_))
    ));
    encoder.flush(&mut sink).unwrap();
    assert!(encoder.encode(&block(48_000, 64), &mut sink).is_err());
    let mut zero = settings();
    zero.bitrate_kbps = 0;
    assert!(FfmpegAudioEncoder::aac(zero).is_err());
}

#[test]
fn sample_indices_round_trip() {
    for index in [0u64, 1, 1_023, 48_000, 172_800_000] {
        assert_eq!(sample_index(RATE, RATE.pts(index)), index);
    }
}
