//! The counts, the latest-wins handover, and the order across threads.

use voltra_core::{FrameSize, PixelFormat, Timestamp};

use super::{CaptureStats, frame_channel};

const FORMAT: PixelFormat = PixelFormat::Bgra8;

fn size() -> FrameSize {
    FrameSize::new(16, 8).unwrap()
}

#[test]
fn nothing_is_shown_before_the_first_frame() {
    let (_writer, mut reader) = frame_channel(FORMAT, size()).unwrap();
    assert!(!reader.update());
    assert!(reader.frame().is_none());
    assert_eq!(reader.stats(), CaptureStats::default(), "no repeat yet");
}

#[test]
fn the_latest_wins_and_the_rest_are_counted_as_dropped() {
    let (mut writer, mut reader) = frame_channel(FORMAT, size()).unwrap();
    for (n, value) in [1u8, 2, 3].into_iter().enumerate() {
        writer
            .frame_mut(FORMAT, size())
            .unwrap()
            .as_bytes_mut()
            .fill(value);
        writer.publish(Timestamp::from_millis(n as u64 * 16));
    }
    assert!(reader.update());
    let frame = reader.frame().unwrap();
    assert_eq!(frame.pts(), Timestamp::from_millis(32));
    assert!(frame.as_bytes().iter().all(|&byte| byte == 3));

    // Nothing new: the same frame again, counted as a repeat.
    assert!(!reader.update());
    assert_eq!(reader.frame().unwrap().pts(), Timestamp::from_millis(32));
    let stats = reader.stats();
    assert_eq!(
        stats,
        CaptureStats {
            captured: 3,
            dropped: 2,
            shown: 1,
            repeated: 1,
            reallocated: 0,
        }
    );
    assert_eq!(stats.pending(), 0);
}

#[test]
fn a_published_frame_not_read_yet_is_pending() {
    let (mut writer, mut reader) = frame_channel(FORMAT, size()).unwrap();
    writer.frame_mut(FORMAT, size()).unwrap();
    writer.publish(Timestamp::ZERO);
    assert_eq!(writer.stats().pending(), 1);
    reader.update();
    assert_eq!(writer.stats().pending(), 0);
}

#[test]
fn a_change_of_size_reallocates_each_buffer_once() {
    let (mut writer, mut reader) = frame_channel(FORMAT, size()).unwrap();
    let larger = FrameSize::new(32, 16).unwrap();
    for n in 0..6u64 {
        let frame = writer.frame_mut(FORMAT, larger).unwrap();
        assert_eq!(frame.size(), larger);
        writer.publish(Timestamp::from_millis(n));
        assert!(reader.update());
        assert_eq!(reader.frame().unwrap().size(), larger);
    }
    // Three buffers, each reallocated the first time it came back to the
    // writer; never again after that.
    assert_eq!(reader.stats().reallocated, 3);
}

#[test]
fn across_threads_frames_never_go_back_and_every_one_is_accounted_for() {
    const FRAMES: u64 = 20_000;
    let (mut writer, mut reader) = frame_channel(FORMAT, size()).unwrap();
    let producer = std::thread::spawn(move || {
        for n in 1..=FRAMES {
            writer.frame_mut(FORMAT, size()).unwrap();
            writer.publish(Timestamp::from_nanos(n));
        }
    });
    let mut last = 0;
    while last < FRAMES {
        if reader.update() {
            let pts = reader.frame().unwrap().pts().as_nanos();
            assert!(pts > last, "frame {pts} after frame {last}");
            last = pts;
        }
    }
    producer.join().unwrap();
    let stats = reader.stats();
    assert_eq!(stats.captured, FRAMES);
    assert_eq!(stats.shown + stats.dropped, FRAMES, "{stats:?}");
    assert_eq!(stats.pending(), 0);
}
