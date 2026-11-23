//! x264 through libavcodec, end to end: frames in, packets out.

use voltra_core::{Fps, FrameSize, PixelFormat, Timestamp, VideoFrame};

use super::{FfmpegVideoEncoder, frame_index};
use crate::h264::{NalType, decoder_config, is_keyframe, nal_units};
use crate::{EncodedPacket, EncoderInput, Error, Result, VideoEncoder, VideoSettings};

const SIZE: (u32, u32) = (320, 240);

fn settings() -> VideoSettings {
    VideoSettings::new(FrameSize::new(SIZE.0, SIZE.1).unwrap(), Fps::FPS_30)
}

/// A frame that changes smoothly from one index to the next.
fn frame(n: u64) -> VideoFrame {
    let size = FrameSize::new(SIZE.0, SIZE.1).unwrap();
    let mut frame = VideoFrame::new(PixelFormat::I420, size, Fps::FPS_30.pts(n)).unwrap();
    for (plane, rows) in frame.planes_mut().iter_mut().enumerate().take(3) {
        for (index, byte) in rows.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let value = if plane == 0 {
                (index as u64 + n * 3) % 220 + 16
            } else {
                128
            };
            #[allow(clippy::cast_possible_truncation)]
            {
                *byte = value as u8;
            }
        }
    }
    frame
}

#[derive(Debug, Clone)]
struct Seen {
    pts: Timestamp,
    dts: Timestamp,
    keyframe: bool,
    idr: bool,
    len: usize,
}

fn encode(frames: u64) -> (FfmpegVideoEncoder, Vec<Seen>) {
    let mut encoder = FfmpegVideoEncoder::h264(settings()).unwrap();
    let mut seen = Vec::new();
    let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
        seen.push(Seen {
            pts: packet.pts,
            dts: packet.dts,
            keyframe: packet.keyframe,
            idr: is_keyframe(packet.data),
            len: packet.data.len(),
        });
        Ok(())
    };
    for n in 0..frames {
        encoder
            .encode(EncoderInput::Frame(&frame(n)), &mut sink)
            .unwrap();
    }
    encoder.flush(&mut sink).unwrap();
    (encoder, seen)
}

#[test]
fn every_frame_comes_out_in_order_with_its_own_time() {
    let (_, seen) = encode(90);
    assert_eq!(seen.len(), 90, "one packet per frame after the flush");
    for (n, packet) in seen.iter().enumerate() {
        let expected = Fps::FPS_30.pts(n as u64);
        assert_eq!(packet.pts, expected, "packet {n}");
        assert_eq!(
            packet.dts, expected,
            "no B-frames: decode order is display order"
        );
        assert!(packet.len > 0);
        assert_eq!(
            packet.keyframe, packet.idr,
            "the flag matches the bitstream"
        );
    }
}

#[test]
fn keyframes_come_every_two_seconds() {
    let (_, seen) = encode(90);
    assert!(seen[0].keyframe, "a stream starts on a keyframe");
    assert!(seen[60].keyframe, "two seconds at 30 fps");
    // A scene cut could add more, but this content changes smoothly.
    let count = seen.iter().filter(|packet| packet.keyframe).count();
    assert_eq!(
        count,
        2,
        "{:?}",
        seen.iter().map(|p| p.keyframe).collect::<Vec<_>>()
    );
}

#[test]
fn headers_are_parameter_sets_a_container_can_use() {
    let encoder = FfmpegVideoEncoder::h264(settings()).unwrap();
    let headers = encoder
        .headers()
        .expect("x264 knows its headers on opening");
    let types: Vec<_> = nal_units(headers).filter_map(NalType::of).collect();
    assert!(
        types.contains(&NalType::Sps) && types.contains(&NalType::Pps),
        "{types:?}"
    );
    let config = decoder_config(headers).unwrap();
    assert_eq!(config[0], 1);
    assert_eq!(config[1], 100, "High profile, x264's default");
    assert_eq!(encoder.info().name, "libx264");
}

#[test]
fn wrong_input_is_refused_and_a_flushed_encoder_stays_flushed() {
    let mut encoder = FfmpegVideoEncoder::h264(settings()).unwrap();
    let mut sink = |_: EncodedPacket<'_>| -> Result<()> { Ok(()) };
    let wrong = VideoFrame::new(
        PixelFormat::I420,
        FrameSize::new(64, 48).unwrap(),
        Timestamp::ZERO,
    )
    .unwrap();
    assert!(matches!(
        encoder.encode(EncoderInput::Frame(&wrong), &mut sink),
        Err(Error::Settings(_))
    ));
    encoder.flush(&mut sink).unwrap();
    assert!(
        encoder
            .encode(EncoderInput::Frame(&frame(0)), &mut sink)
            .is_err()
    );
    encoder.flush(&mut sink).unwrap();
}

#[test]
fn a_refusing_sink_stops_encoding() {
    let mut encoder = FfmpegVideoEncoder::h264(settings()).unwrap();
    let mut sink =
        |_: EncodedPacket<'_>| -> Result<()> { Err(Error::Sink("disk full".to_owned())) };
    let mut stopped = false;
    for n in 0..60 {
        if encoder
            .encode(EncoderInput::Frame(&frame(n)), &mut sink)
            .is_err()
        {
            stopped = true;
            break;
        }
    }
    let flushed = encoder.flush(&mut sink);
    assert!(
        stopped || flushed.is_err(),
        "the sink's refusal reaches the caller"
    );
}

#[test]
fn bad_settings_and_unknown_encoders_are_refused() {
    let odd = VideoSettings::new(FrameSize::new(321, 240).unwrap(), Fps::FPS_30);
    assert!(matches!(
        FfmpegVideoEncoder::h264(odd),
        Err(Error::Settings(_))
    ));
    let mut zero = settings();
    zero.bitrate_kbps = 0;
    assert!(matches!(
        FfmpegVideoEncoder::h264(zero),
        Err(Error::Settings(_))
    ));
    assert!(matches!(
        FfmpegVideoEncoder::open("no_such_encoder", settings()),
        Err(Error::Unavailable(_))
    ));
}

#[test]
fn frame_indices_round_trip_at_fractional_rates() {
    let ntsc = Fps::new(30_000, 1_001).unwrap();
    for n in [0u64, 1, 29, 30, 1_799, 107_892] {
        assert_eq!(frame_index(ntsc, ntsc.pts(n)), n);
        assert_eq!(frame_index(Fps::FPS_60, Fps::FPS_60.pts(n)), n);
    }
}

#[test]
fn timestamps_survive_a_stream_that_does_not_start_at_zero() {
    // A monotonic clock is never at zero: start an hour in.
    let mut encoder = FfmpegVideoEncoder::h264(settings()).unwrap();
    let start = 30 * 3_600;
    let mut times = Vec::new();
    let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
        times.push(packet.pts);
        Ok(())
    };
    for n in 0..10 {
        let mut input = frame(n);
        input.set_pts(Fps::FPS_30.pts(start + n));
        encoder
            .encode(EncoderInput::Frame(&input), &mut sink)
            .unwrap();
    }
    encoder.flush(&mut sink).unwrap();
    let expected: Vec<_> = (0..10).map(|n| Fps::FPS_30.pts(start + n)).collect();
    assert_eq!(times, expected);
}

#[test]
fn a_stream_off_the_frame_grid_keeps_its_own_times() {
    // 29.97 fps from one second in: a second is 29.97 frames, so the start is
    // not a whole frame of a grid anchored at zero. Counting frames from zero
    // moved every packet a millisecond; counted from the first frame, each
    // packet keeps its frame's time exactly.
    let ntsc = Fps::new(30_000, 1_001).unwrap();
    let mut settings = settings();
    settings.fps = ntsc;
    let mut encoder = FfmpegVideoEncoder::h264(settings).unwrap();
    let start = Timestamp::from_millis(1_000);
    let at = |n: u64| Timestamp::from_nanos(start.as_nanos() + ntsc.pts(n).as_nanos());
    let mut times = Vec::new();
    let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
        times.push(packet.pts);
        Ok(())
    };
    for n in 0..10 {
        let mut input = frame(n);
        input.set_pts(at(n));
        encoder
            .encode(EncoderInput::Frame(&input), &mut sink)
            .unwrap();
    }
    encoder.flush(&mut sink).unwrap();
    let expected: Vec<_> = (0..10).map(at).collect();
    assert_eq!(times, expected);
}
