//! The writer's rules, checked on the bytes it produces.

// Exact values are the point: the duration is an integer number of milliseconds.
#![allow(clippy::float_cmp)]

use std::io::Cursor;

use voltra_core::{
    AudioBuffer, ChannelLayout, Fps, FrameSize, PixelFormat, SampleRate, Timestamp, VideoFrame,
};

use super::{MatroskaWriter, TrackSpec};

const AUDIO: TrackSpec = TrackSpec::PcmAudio {
    rate: SampleRate::HZ_48000,
    layout: ChannelLayout::Stereo,
};

fn video() -> TrackSpec {
    TrackSpec::RawVideo {
        size: FrameSize::new(16, 8).unwrap(),
        fps: Fps::FPS_30,
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

const CLUSTER: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];

#[test]
fn the_header_declares_matroska_and_both_codecs() {
    let file = MatroskaWriter::new(Vec::new(), &[video(), AUDIO])
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(&file[..4], &[0x1A, 0x45, 0xDF, 0xA3]);
    for needle in [
        &b"matroska"[..],
        b"V_UNCOMPRESSED",
        b"I420",
        b"A_PCM/FLOAT/IEEE",
    ] {
        assert_eq!(
            count(&file, needle),
            1,
            "{}",
            String::from_utf8_lossy(needle)
        );
    }
    // The segment is open-ended: unknown size right after its ID.
    let segment = file
        .windows(4)
        .position(|w| w == [0x18, 0x53, 0x80, 0x67])
        .unwrap();
    assert_eq!(&file[segment + 4..segment + 12], &super::UNKNOWN_SIZE);
}

#[test]
fn blocks_out_of_order_are_refused() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[AUDIO]).unwrap();
    writer
        .write_block(0, Timestamp::from_millis(100), true, &[0; 8])
        .unwrap();
    writer
        .write_block(0, Timestamp::from_millis(100), true, &[0; 8])
        .unwrap();
    assert!(
        writer
            .write_block(0, Timestamp::from_millis(99), true, &[0; 8])
            .is_err()
    );
    assert!(
        writer
            .write_block(1, Timestamp::from_millis(200), true, &[])
            .is_err(),
        "no track 1"
    );
}

#[test]
fn clusters_open_on_a_keyframe_a_second_in_or_before_the_limit() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[video(), AUDIO]).unwrap();
    let at = |ms| Timestamp::from_millis(ms);
    writer.write_block(0, at(0), true, &[1]).unwrap();
    writer.write_block(0, at(999), true, &[2]).unwrap();
    writer.write_block(0, at(1_500), false, &[3]).unwrap(); // not a keyframe: stays
    writer.write_block(0, at(1_600), true, &[4]).unwrap(); // opens the second
    writer.write_block(0, at(40_000), false, &[5]).unwrap(); // past the limit: forced
    let file = writer.finish().unwrap();
    assert_eq!(count(&file, &CLUSTER), 3);
}

#[test]
fn the_first_block_is_time_zero() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[AUDIO]).unwrap();
    // An hour into a monotonic clock.
    writer
        .write_block(0, Timestamp::from_millis(3_600_000), true, &[0xAB])
        .unwrap();
    let file = writer.finish().unwrap();
    let cluster = file.windows(4).position(|w| w == CLUSTER).unwrap();
    // Cluster ID, unknown size, then Timestamp 0: E7 81 00.
    assert_eq!(&file[cluster + 12..cluster + 15], &[0xE7, 0x81, 0x00]);
    // SimpleBlock: A3, size 5, track 1, relative 0, keyframe, payload.
    assert_eq!(
        &file[cluster + 15..],
        &[0xA3, 0x85, 0x81, 0x00, 0x00, 0x80, 0xAB]
    );
}

#[test]
fn timestamps_round_to_the_nearest_millisecond_from_their_own_value() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[AUDIO]).unwrap();
    // Block k of 1024 at 48 kHz: 21.333… ms apart. Rounded from the absolute
    // time, block 3 is at 64 ms, not 3 × 21 = 63.
    for k in 0..4 {
        writer
            .write_block(0, SampleRate::HZ_48000.pts(k * 1024), true, &[])
            .unwrap();
    }
    let file = writer.finish().unwrap();
    // The last block ends: track, relative timestamp (2 bytes), flags.
    let last = file.len() - 3;
    assert_eq!(&file[last..last + 2], &64i16.to_be_bytes());
}

#[test]
fn video_frames_go_out_without_stride_padding() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[video()]).unwrap();
    let mut frame = VideoFrame::new(
        PixelFormat::I420,
        FrameSize::new(16, 8).unwrap(),
        Timestamp::ZERO,
    )
    .unwrap();
    for (plane, value) in [(0, 10u8), (1, 20), (2, 30)] {
        frame.plane_mut(plane).unwrap().fill(value);
    }
    writer.write_video(0, &frame, Timestamp::ZERO).unwrap();
    let file = writer.finish().unwrap();
    // 16×8 luma, two 8×4 chroma planes: 192 bytes of payload, in order.
    let payload = &file[file.len() - 192..];
    assert!(payload[..128].iter().all(|&b| b == 10));
    assert!(payload[128..160].iter().all(|&b| b == 20));
    assert!(payload[160..].iter().all(|&b| b == 30));

    let mut writer = MatroskaWriter::new(Vec::new(), &[video()]).unwrap();
    let wrong = VideoFrame::new(
        PixelFormat::I420,
        FrameSize::new(32, 8).unwrap(),
        Timestamp::ZERO,
    )
    .unwrap();
    assert!(writer.write_video(0, &wrong, Timestamp::ZERO).is_err());
}

#[test]
fn audio_reuses_its_buffer() {
    let mut writer = MatroskaWriter::new(Vec::new(), &[AUDIO]).unwrap();
    let mut block = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Stereo,
        1024,
        Timestamp::ZERO,
    )
    .unwrap();
    writer.write_audio(0, &block).unwrap();
    let capacity = writer.scratch.capacity();
    for k in 1..100 {
        block.set_pts(SampleRate::HZ_48000.pts(k * 1024));
        writer.write_audio(0, &block).unwrap();
    }
    assert_eq!(
        writer.scratch.capacity(),
        capacity,
        "no reallocation after the first block"
    );
    let mono = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Mono,
        1024,
        block.end_pts(),
    )
    .unwrap();
    assert!(writer.write_audio(0, &mono).is_err());
}

#[test]
fn the_duration_is_patched_in_place_when_the_sink_can_seek() {
    let mut writer = MatroskaWriter::new(Cursor::new(Vec::new()), &[AUDIO]).unwrap();
    let mut block = AudioBuffer::new(
        SampleRate::HZ_48000,
        ChannelLayout::Stereo,
        48_000,
        Timestamp::ZERO,
    )
    .unwrap();
    writer.write_audio(0, &block).unwrap();
    block.set_pts(block.end_pts());
    writer.write_audio(0, &block).unwrap();
    let at = usize::try_from(writer.duration_at).unwrap();
    let length = writer.written;
    let file = writer.finish_with_duration().unwrap().into_inner();
    assert_eq!(file.len() as u64, length, "nothing appended or lost");
    let duration = f64::from_be_bytes(file[at..at + 8].try_into().unwrap());
    assert_eq!(duration, 2_000.0, "two seconds, in milliseconds");
}

#[test]
fn odd_video_sizes_and_empty_track_lists_are_refused() {
    let odd = TrackSpec::RawVideo {
        size: FrameSize::new(15, 8).unwrap(),
        fps: Fps::FPS_30,
    };
    assert!(MatroskaWriter::new(Vec::new(), &[odd]).is_err());
    assert!(MatroskaWriter::new(Vec::new(), &[]).is_err());
}

#[test]
fn an_h264_track_carries_its_decoder_config() {
    let config = vec![0x01, 0x64, 0x00, 0x1E, 0xFF, 0xE1];
    let track = TrackSpec::H264 {
        size: FrameSize::new(16, 8).unwrap(),
        fps: Fps::FPS_30,
        decoder_config: config.clone(),
    };
    let mut writer = MatroskaWriter::new(Vec::new(), &[track, AUDIO]).unwrap();
    writer
        .write_block(0, Timestamp::ZERO, true, &[0, 0, 0, 1, 0x65])
        .unwrap();
    let file = writer.finish().unwrap();
    assert_eq!(count(&file, b"V_MPEG4/ISO/AVC"), 1);
    let private = file.windows(2).position(|w| w == [0x63, 0xA2]).unwrap();
    assert_eq!(file[private + 2], 0x80 | 6, "size of the record");
    assert_eq!(&file[private + 3..private + 9], &config[..]);
}
