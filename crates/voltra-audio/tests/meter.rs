//! A meter written on one thread and read on another loses nothing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use voltra_audio::{Meter, PeakMode};
use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

const FRAMES: usize = 1024;
const BLOCKS: u32 = 10_000;

#[test]
fn concurrent_reads_lose_and_duplicate_nothing() {
    let (mut meter, mut reader) = Meter::new(ChannelLayout::Stereo, PeakMode::Sample);
    let done = Arc::new(AtomicBool::new(false));

    let writer = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut block = AudioBuffer::new(
                SampleRate::HZ_48000,
                ChannelLayout::Stereo,
                FRAMES,
                Timestamp::ZERO,
            )
            .unwrap();
            for index in 0..BLOCKS {
                // A rising peak, with the loudest block somewhere in the middle.
                #[allow(clippy::cast_precision_loss)]
                let level = if index == 6_789 {
                    0.99
                } else {
                    (index % 100) as f32 / 1000.0
                };
                block.as_mut_slice().fill(level);
                assert!(meter.process(&block));
            }
            done.store(true, Ordering::Release);
        })
    };

    let mut frames = 0u64;
    let mut loudest = 0.0f32;
    let mut readings = 0u32;
    loop {
        let finished = done.load(Ordering::Acquire);
        let levels = reader.take();
        frames += levels.frames();
        loudest = loudest.max(levels.channel(0).unwrap().peak);
        readings += 1;
        if finished {
            break;
        }
    }
    writer.join().unwrap();
    // One last take catches anything written after the final loop read.
    frames += reader.take().frames();

    assert_eq!(frames, u64::from(BLOCKS) * FRAMES as u64);
    assert_eq!(loudest, 0.99);
    assert!(readings > 1, "the reader actually raced the writer");
}
