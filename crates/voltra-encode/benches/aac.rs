//! What AAC costs per 1024-sample stereo block, through libavcodec's native
//! encoder. Needs `--features ffmpeg`.

// `criterion_group!` expands to an undocumented public function.
#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use criterion::{Criterion, criterion_group, criterion_main};

use voltra_core::{AudioBuffer, ChannelLayout, SampleRate};
use voltra_encode::{AudioEncoder, AudioSettings, EncodedPacket, FfmpegAudioEncoder, Result};

fn aac(c: &mut Criterion) {
    let rate = SampleRate::HZ_48000;
    let mut encoder =
        FfmpegAudioEncoder::aac(AudioSettings::new(rate, ChannelLayout::Stereo)).unwrap();
    let mut block = AudioBuffer::new(rate, ChannelLayout::Stereo, 1024, rate.pts(48_000)).unwrap();
    for samples in block.channels_mut() {
        for (k, sample) in samples.iter_mut().enumerate() {
            *sample = ((k as f32) / 11.0).sin() * 0.4 + ((k as f32) / 3.1).sin() * 0.1;
        }
    }
    let mut k = 0u64;
    let mut sink = |packet: EncodedPacket<'_>| -> Result<()> {
        std::hint::black_box(packet.data.len());
        Ok(())
    };
    c.bench_function("aac 1024 stereo samples", |b| {
        b.iter(|| {
            block.set_pts(rate.pts(48_000 + k * 1024));
            k += 1;
            encoder.encode(&block, &mut sink).unwrap();
        });
    });
}

criterion_group!(benches, aac);
criterion_main!(benches);
