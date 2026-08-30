//! Converting between the edge formats and the internal float buffer.
//!
//! The core only ever sees `f32` planar. These are the two crossings: what a
//! device or a file hands over on the way in, and what an encoder or a container
//! wants on the way out.
//!
//! # The scale is asymmetric, on purpose
//!
//! Signed integer audio is not symmetric: `i16` runs from −32768 to +32767, so
//! there is one more code below zero than above. Converting **in** divides by
//! 32768, which maps the whole range into `[-1, 1]` with −1.0 exactly
//! representable. Converting **out** multiplies by 32767 and saturates, so
//! +1.0 lands on +32767 rather than wrapping to −32768.
//!
//! Getting that backwards produces a full-scale negative spike at the peak of
//! every loud waveform — a click you hear and cannot explain. libsndfile,
//! `libswresample` and every DAW use these same two constants.
//!
//! # Shape
//!
//! Each depth is a separate monomorphic loop reached through a trait, so the
//! per-sample path carries no `match` (CLAUDE.md §4.4) — the same shape the
//! colour converter of plan 004 uses.

use crate::audio::buffer::AudioBuffer;
use crate::audio::format::{SampleFormat, SampleOrder, SampleSpec};
use crate::{Error, Result};

/// Divisor turning `i16` into `[-1, 1]`. See the module note on asymmetry.
const I16_SCALE_IN: f32 = 32_768.0;
/// Multiplier turning `[-1, 1]` back into `i16`, saturating at the top.
const I16_SCALE_OUT: f32 = 32_767.0;
const I32_SCALE_IN: f32 = 2_147_483_648.0;
const I32_SCALE_OUT: f32 = 2_147_483_647.0;

/// One edge sample depth, as a monomorphic pair of conversions.
trait Depth {
    /// Bytes this depth occupies.
    const BYTES: usize;
    /// Decode one sample into `[-1, 1]`.
    fn decode(bytes: &[u8]) -> f32;
    /// Encode one sample, clamping to the representable range.
    fn encode(sample: f32, out: &mut [u8]);
}

struct U8Depth;
struct I16Depth;
struct I32Depth;
struct F32Depth;

impl Depth for U8Depth {
    const BYTES: usize = 1;

    fn decode(bytes: &[u8]) -> f32 {
        // Unsigned, with 128 as silence.
        (f32::from(bytes[0]) - 128.0) / 128.0
    }

    fn encode(sample: f32, out: &mut [u8]) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=255 immediately before the cast"
        )]
        {
            out[0] = sample.mul_add(127.0, 128.0).clamp(0.0, 255.0) as u8;
        }
    }
}

impl Depth for I16Depth {
    const BYTES: usize = 2;

    fn decode(bytes: &[u8]) -> f32 {
        f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / I16_SCALE_IN
    }

    fn encode(sample: f32, out: &mut [u8]) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped into the i16 range immediately before the cast"
        )]
        let value = (sample * I16_SCALE_OUT).clamp(-I16_SCALE_IN, I16_SCALE_OUT) as i16;
        out[..2].copy_from_slice(&value.to_le_bytes());
    }
}

impl Depth for I32Depth {
    const BYTES: usize = 4;

    #[allow(
        clippy::cast_precision_loss,
        reason = "24 bits of audio resolution is more than anyone can hear; f32 carries them"
    )]
    fn decode(bytes: &[u8]) -> f32 {
        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / I32_SCALE_IN
    }

    fn encode(sample: f32, out: &mut [u8]) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped into the i32 range immediately before the cast"
        )]
        let value = (sample * I32_SCALE_OUT).clamp(-I32_SCALE_IN, I32_SCALE_OUT) as i32;
        out[..4].copy_from_slice(&value.to_le_bytes());
    }
}

impl Depth for F32Depth {
    const BYTES: usize = 4;

    fn decode(bytes: &[u8]) -> f32 {
        f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    fn encode(sample: f32, out: &mut [u8]) {
        // Not clamped: float is the one format that can carry a sum above unity,
        // and clamping here would limit twice for anyone writing float out.
        out[..4].copy_from_slice(&sample.to_le_bytes());
    }
}

impl AudioBuffer {
    /// Fill this buffer from an edge buffer.
    ///
    /// `bytes` must hold exactly `frames × channels` samples in `spec`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `bytes` is not the length the shape implies.
    /// Reading a short buffer is how a decoder ends up playing whatever was next
    /// in memory.
    ///
    /// # Examples
    ///
    /// ```
    /// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, SampleSpec, Timestamp};
    ///
    /// // Two interleaved stereo frames: hard left, then hard right.
    /// let bytes = [0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40];
    /// let mut block =
    ///     AudioBuffer::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 2, Timestamp::ZERO)?;
    /// block.fill_from(&bytes, SampleSpec::I16_INTERLEAVED)?;
    ///
    /// assert_eq!(block.channel(0), Some(&[0.5, 0.0][..]));
    /// assert_eq!(block.channel(1), Some(&[0.0, 0.5][..]));
    /// # Ok::<(), voltra_core::Error>(())
    /// ```
    pub fn fill_from(&mut self, bytes: &[u8], spec: SampleSpec) -> Result<()> {
        let expected = spec.byte_len(self.frames(), self.channel_count());
        if bytes.len() != expected {
            return Err(Error::config(format!(
                "{} needs {expected} bytes for {} frames of {}, got {}",
                spec,
                self.frames(),
                self.layout(),
                bytes.len()
            )));
        }

        match spec.format {
            SampleFormat::U8 => self.fill_with::<U8Depth>(bytes, spec.order),
            SampleFormat::I16 => self.fill_with::<I16Depth>(bytes, spec.order),
            SampleFormat::I32 => self.fill_with::<I32Depth>(bytes, spec.order),
            SampleFormat::F32 => self.fill_with::<F32Depth>(bytes, spec.order),
        }
        Ok(())
    }

    /// Write this buffer out in an edge format.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `bytes` is not the length the shape implies.
    pub fn write_to(&self, bytes: &mut [u8], spec: SampleSpec) -> Result<()> {
        let expected = spec.byte_len(self.frames(), self.channel_count());
        if bytes.len() != expected {
            return Err(Error::config(format!(
                "{} needs {expected} bytes for {} frames of {}, got {}",
                spec,
                self.frames(),
                self.layout(),
                bytes.len()
            )));
        }

        match spec.format {
            SampleFormat::U8 => self.write_with::<U8Depth>(bytes, spec.order),
            SampleFormat::I16 => self.write_with::<I16Depth>(bytes, spec.order),
            SampleFormat::I32 => self.write_with::<I32Depth>(bytes, spec.order),
            SampleFormat::F32 => self.write_with::<F32Depth>(bytes, spec.order),
        }
        Ok(())
    }

    /// The monomorphic read loop for one depth.
    fn fill_with<D: Depth>(&mut self, bytes: &[u8], order: SampleOrder) {
        let frames = self.frames();
        let channels = self.channel_count();

        match order {
            SampleOrder::Planar => {
                // Plane for plane: the cheap direction, one contiguous run each.
                let plane = frames * D::BYTES;
                for (index, channel) in self.channels_mut().enumerate() {
                    let source = &bytes[index * plane..(index + 1) * plane];
                    for (sample, raw) in channel.iter_mut().zip(source.chunks_exact(D::BYTES)) {
                        *sample = D::decode(raw);
                    }
                }
            }
            SampleOrder::Interleaved => {
                // The transposition. Walking the input once and scattering costs
                // one pass; walking it per channel would cost `channels` passes
                // over the same memory.
                let stride = channels * D::BYTES;
                for (index, channel) in self.channels_mut().enumerate() {
                    let offset = index * D::BYTES;
                    for (sample, group) in channel.iter_mut().zip(bytes.chunks_exact(stride)) {
                        *sample = D::decode(&group[offset..offset + D::BYTES]);
                    }
                }
            }
        }
    }

    /// The monomorphic write loop for one depth.
    fn write_with<D: Depth>(&self, bytes: &mut [u8], order: SampleOrder) {
        let frames = self.frames();
        let channels = self.channel_count();

        match order {
            SampleOrder::Planar => {
                let plane = frames * D::BYTES;
                for (index, channel) in self.channels().enumerate() {
                    let target = &mut bytes[index * plane..(index + 1) * plane];
                    for (sample, raw) in channel.iter().zip(target.chunks_exact_mut(D::BYTES)) {
                        D::encode(*sample, raw);
                    }
                }
            }
            SampleOrder::Interleaved => {
                let stride = channels * D::BYTES;
                for (index, channel) in self.channels().enumerate() {
                    let offset = index * D::BYTES;
                    for (sample, group) in channel.iter().zip(bytes.chunks_exact_mut(stride)) {
                        D::encode(*sample, &mut group[offset..offset + D::BYTES]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::audio::buffer::AudioBuffer;
    use crate::audio::format::{ChannelLayout, SampleFormat, SampleOrder, SampleRate, SampleSpec};
    use crate::time::Timestamp;

    fn block(frames: usize) -> AudioBuffer {
        AudioBuffer::new(
            SampleRate::HZ_48000,
            ChannelLayout::Stereo,
            frames,
            Timestamp::ZERO,
        )
        .unwrap()
    }

    /// A ramp per channel, distinct between channels so a transposition error
    /// cannot pass for correct.
    fn ramped(frames: usize) -> AudioBuffer {
        let mut buffer = block(frames);
        for (index, channel) in buffer.channels_mut().enumerate() {
            for (position, sample) in channel.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let value = (position as f32 / 64.0).sin() * 0.8;
                *sample = if index == 0 { value } else { -value };
            }
        }
        buffer
    }

    const EVERY_SPEC: [SampleSpec; 8] = [
        SampleSpec::new(SampleFormat::U8, SampleOrder::Interleaved),
        SampleSpec::new(SampleFormat::U8, SampleOrder::Planar),
        SampleSpec::new(SampleFormat::I16, SampleOrder::Interleaved),
        SampleSpec::new(SampleFormat::I16, SampleOrder::Planar),
        SampleSpec::new(SampleFormat::I32, SampleOrder::Interleaved),
        SampleSpec::new(SampleFormat::I32, SampleOrder::Planar),
        SampleSpec::new(SampleFormat::F32, SampleOrder::Interleaved),
        SampleSpec::new(SampleFormat::F32, SampleOrder::Planar),
    ];

    /// Out and back for every combination, within that format's own quantum.
    #[test]
    fn every_format_round_trips_within_its_resolution() {
        let original = ramped(256);

        for spec in EVERY_SPEC {
            let mut bytes = vec![0u8; spec.byte_len(256, 2)];
            original.write_to(&mut bytes, spec).unwrap();

            let mut recovered = block(256);
            recovered.fill_from(&bytes, spec).unwrap();

            // One quantum of the format, plus a little for the asymmetric scale.
            let tolerance = match spec.format {
                SampleFormat::U8 => 1.0 / 64.0,
                SampleFormat::I16 => 1.0 / 16_384.0,
                SampleFormat::I32 | SampleFormat::F32 => 1.0 / 1_048_576.0,
            };
            for channel in 0..2 {
                let before = original.channel(channel).unwrap();
                let after = recovered.channel(channel).unwrap();
                for (index, (a, b)) in before.iter().zip(after.iter()).enumerate() {
                    assert!(
                        (a - b).abs() <= tolerance,
                        "{spec}: channel {channel} sample {index}: {a} became {b}"
                    );
                }
            }
        }
    }

    /// Float out and back has no quantum at all: it must be bit-identical.
    #[test]
    fn float_round_trips_exactly() {
        let original = ramped(128);
        for order in [SampleOrder::Interleaved, SampleOrder::Planar] {
            let spec = SampleSpec::new(SampleFormat::F32, order);
            let mut bytes = vec![0u8; spec.byte_len(128, 2)];
            original.write_to(&mut bytes, spec).unwrap();

            let mut recovered = block(128);
            recovered.fill_from(&bytes, spec).unwrap();
            assert_eq!(original.as_slice(), recovered.as_slice());
        }
    }

    /// The two orders are two arrangements of the same sound, so reading either
    /// has to land the same samples in the same channels.
    #[test]
    fn interleaved_and_planar_agree() {
        let original = ramped(64);

        let interleaved = SampleSpec::new(SampleFormat::I16, SampleOrder::Interleaved);
        let planar = SampleSpec::new(SampleFormat::I16, SampleOrder::Planar);

        let mut a = vec![0u8; interleaved.byte_len(64, 2)];
        let mut b = vec![0u8; planar.byte_len(64, 2)];
        original.write_to(&mut a, interleaved).unwrap();
        original.write_to(&mut b, planar).unwrap();

        let mut from_interleaved = block(64);
        let mut from_planar = block(64);
        from_interleaved.fill_from(&a, interleaved).unwrap();
        from_planar.fill_from(&b, planar).unwrap();

        assert_eq!(from_interleaved.as_slice(), from_planar.as_slice());
    }

    /// The test that separates a limiter from a noise generator: full scale must
    /// land at the end of the integer range, and beyond it must saturate rather
    /// than wrap into a full-scale spike of the opposite sign.
    #[test]
    fn full_scale_saturates_instead_of_wrapping() {
        let mut buffer = block(4);
        buffer
            .as_mut_slice()
            .copy_from_slice(&[1.0, -1.0, 2.0, -2.0, 1.0, -1.0, 2.0, -2.0]);

        let spec = SampleSpec::new(SampleFormat::I16, SampleOrder::Planar);
        let mut bytes = vec![0u8; spec.byte_len(4, 2)];
        buffer.write_to(&mut bytes, spec).unwrap();

        let values: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        assert_eq!(values[0], 32_767, "+1.0 must reach the top, not wrap");
        assert_eq!(values[1], -32_767);
        assert_eq!(values[2], 32_767, "+2.0 must saturate");
        assert_eq!(values[3], -32_768, "-2.0 clamps at the representable floor");
    }

    /// −1.0 has to be exactly representable, which is why the inbound divisor is
    /// 32768 and not 32767.
    #[test]
    fn negative_full_scale_is_exact() {
        let spec = SampleSpec::new(SampleFormat::I16, SampleOrder::Planar);
        let mut bytes = vec![0u8; spec.byte_len(1, 2)];
        bytes[..2].copy_from_slice(&(-32_768i16).to_le_bytes());
        bytes[2..].copy_from_slice(&(-32_768i16).to_le_bytes());

        let mut buffer = block(1);
        buffer.fill_from(&bytes, spec).unwrap();
        assert_eq!(buffer.channel(0), Some(&[-1.0][..]));
    }

    /// Unsigned 8-bit has silence at 128, not at 0.
    #[test]
    fn unsigned_eight_bit_is_centred_on_128() {
        let spec = SampleSpec::new(SampleFormat::U8, SampleOrder::Planar);
        let mut buffer = block(2);
        buffer.fill_from(&[128, 128, 128, 128], spec).unwrap();
        assert!(buffer.as_slice().iter().all(|&s| s == 0.0));

        let mut bytes = vec![0u8; 4];
        buffer.write_to(&mut bytes, spec).unwrap();
        assert_eq!(bytes, vec![128, 128, 128, 128]);
    }

    /// A short buffer must be refused, not read past. This is how a decoder ends
    /// up playing whatever happened to be next in memory.
    #[test]
    fn a_wrong_sized_edge_buffer_is_refused() {
        let mut buffer = block(16);
        let spec = SampleSpec::I16_INTERLEAVED;
        let mut short = [0u8; 10];
        let long = [0u8; 1000];
        assert!(buffer.fill_from(&short, spec).is_err());
        assert!(buffer.fill_from(&long, spec).is_err());
        assert!(buffer.write_to(&mut short, spec).is_err());
    }
}
