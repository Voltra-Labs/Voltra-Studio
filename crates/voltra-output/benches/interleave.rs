//! What the interleaver costs per packet: a copy into a pooled buffer and the
//! choice of the earliest head.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};

use voltra_core::Timestamp;
use voltra_output::Interleaver;

fn interleave(c: &mut Criterion) {
    // Two tracks, packets the size of 6 Mbit/s video at 60 fps and of AAC.
    let video = vec![0u8; 12_500];
    let audio = vec![0u8; 400];
    let mut interleaver = Interleaver::new(2);
    let mut n = 0u64;
    c.bench_function("interleave a video and an audio packet", |b| {
        b.iter(|| {
            interleaver
                .push(0, Timestamp::from_millis(n * 16), n == 0, &video)
                .unwrap();
            interleaver
                .push(1, Timestamp::from_millis(n * 16 + 5), true, &audio)
                .unwrap();
            interleaver
                .drain(|_, _, _, data| {
                    std::hint::black_box(data.len());
                    Ok(())
                })
                .unwrap();
            n += 1;
        });
    });
}

criterion_group!(benches, interleave);
criterion_main!(benches);
