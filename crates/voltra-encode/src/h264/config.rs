//! The `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §5.3.3.1), from the
//! parameter sets.

use super::nal::{NalType, nal_units};
use crate::{Error, Result};

/// Build the `avcC` record a Matroska `CodecPrivate` or an MP4 `avcC` box
/// carries, from Annex B parameter sets.
///
/// Version 1; profile, compatibility and level copied from the first SPS;
/// four-byte lengths; every SPS and PPS with their lengths; and, for the High
/// profiles, the chroma format and bit depths read from the SPS, which the
/// specification requires and libobs's `obs_parse_avc_header` also writes.
///
/// # Errors
///
/// [`Error::Bitstream`] when there is no SPS or no PPS, or the SPS is too
/// short to read.
pub fn decoder_config(headers: &[u8]) -> Result<Vec<u8>> {
    let sps: Vec<&[u8]> = nal_units(headers)
        .filter(|unit| NalType::of(unit) == Some(NalType::Sps))
        .collect();
    let pps: Vec<&[u8]> = nal_units(headers)
        .filter(|unit| NalType::of(unit) == Some(NalType::Pps))
        .collect();
    let first = *sps
        .first()
        .ok_or(Error::Bitstream("no SPS in the headers"))?;
    if pps.is_empty() {
        return Err(Error::Bitstream("no PPS in the headers"));
    }
    let [_, profile, compatibility, level, ..] = *first else {
        return Err(Error::Bitstream("SPS shorter than its fixed header"));
    };
    if sps.len() > 31 || pps.len() > 255 {
        return Err(Error::Bitstream("more parameter sets than avcC can list"));
    }

    let mut out = vec![1, profile, compatibility, level, 0xFF];
    #[allow(clippy::cast_possible_truncation, reason = "checked above: at most 31")]
    out.push(0xE0 | sps.len() as u8);
    for unit in &sps {
        put_unit(&mut out, unit)?;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "checked above: at most 255"
    )]
    out.push(pps.len() as u8);
    for unit in &pps {
        put_unit(&mut out, unit)?;
    }

    if matches!(profile, 100 | 110 | 122 | 144) {
        let (chroma, luma_depth, chroma_depth) = high_profile_params(first)?;
        out.extend_from_slice(&[0xFC | chroma, 0xF8 | luma_depth, 0xF8 | chroma_depth, 0]);
    }
    Ok(out)
}

fn put_unit(out: &mut Vec<u8>, unit: &[u8]) -> Result<()> {
    let length =
        u16::try_from(unit.len()).map_err(|_| Error::Bitstream("parameter set over 64 KiB"))?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(unit);
    Ok(())
}

/// `chroma_format_idc`, `bit_depth_luma_minus8` and `bit_depth_chroma_minus8`
/// from a High-profile SPS.
fn high_profile_params(sps: &[u8]) -> Result<(u8, u8, u8)> {
    // Undo emulation prevention: `00 00 03` carries `00 00`.
    let mut rbsp = Vec::with_capacity(sps.len());
    let mut zeros = 0;
    for &byte in sps.get(1..).unwrap_or(&[]) {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        rbsp.push(byte);
    }
    let mut bits = Bits::new(&rbsp);
    bits.skip(24)?; // profile_idc, constraint flags, level_idc
    bits.golomb()?; // seq_parameter_set_id
    let chroma = bits.golomb()?;
    if chroma == 3 {
        bits.skip(1)?; // separate_colour_plane_flag
    }
    let luma_depth = bits.golomb()?;
    let chroma_depth = bits.golomb()?;
    let narrow =
        |value: u32| u8::try_from(value).map_err(|_| Error::Bitstream("SPS value out of range"));
    Ok((
        narrow(chroma)? & 0x03,
        narrow(luma_depth)? & 0x07,
        narrow(chroma_depth)? & 0x07,
    ))
}

/// A most-significant-bit-first reader.
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bits<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn bit(&mut self) -> Result<u32> {
        let byte = self
            .data
            .get(self.position / 8)
            .ok_or(Error::Bitstream("SPS ended early"))?;
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;
        Ok(u32::from(bit))
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        for _ in 0..count {
            self.bit()?;
        }
        Ok(())
    }

    /// An unsigned Exp-Golomb code, `ue(v)`.
    fn golomb(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(Error::Bitstream("Exp-Golomb code too long"));
            }
        }
        let mut value = 0;
        for _ in 0..zeros {
            value = (value << 1) | self.bit()?;
        }
        Ok((1 << zeros) - 1 + value)
    }
}

#[cfg(test)]
mod tests {
    use super::decoder_config;
    use crate::Error;

    const SPS: [u8; 22] = [
        0x67, 0x64, 0x00, 0x0A, 0xAC, 0xB4, 0x23, 0xD8, 0x08, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80,
        0x00, 0x00, 0x1E, 0x07, 0x89, 0x13, 0x50,
    ];
    const PPS: [u8; 4] = [0x68, 0xEF, 0x0F, 0xCB];
    /// What `FFmpeg` 6.1 wrote as `CodecPrivate` when remuxing the same x264
    /// stream to Matroska.
    const FFMPEG_AVCC: &str =
        "0164000affe100166764000aacb423d80880000003008000001e0789135001000468ef0fcbfdf8f800";

    fn headers() -> Vec<u8> {
        let mut headers = vec![0, 0, 0, 1];
        headers.extend_from_slice(&SPS);
        headers.extend_from_slice(&[0, 0, 0, 1]);
        headers.extend_from_slice(&PPS);
        headers
    }

    #[test]
    fn matches_what_ffmpeg_writes_byte_for_byte() {
        let config = decoder_config(&headers()).unwrap();
        let hex = config.iter().fold(String::new(), |mut hex, byte| {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        assert_eq!(hex, FFMPEG_AVCC);
    }

    #[test]
    fn a_baseline_sps_has_no_high_profile_extension() {
        let mut sps = SPS;
        sps[1] = 66; // Baseline
        let mut headers = vec![0, 0, 1];
        headers.extend_from_slice(&sps);
        headers.extend_from_slice(&[0, 0, 1]);
        headers.extend_from_slice(&PPS);
        let config = decoder_config(&headers).unwrap();
        assert_eq!(config.len(), 5 + 1 + 2 + 22 + 1 + 2 + 4);
    }

    #[test]
    fn missing_parameter_sets_are_errors() {
        let pps_alone = [0, 0, 1, 0x68, 0xEF];
        assert!(matches!(
            decoder_config(&pps_alone),
            Err(Error::Bitstream(_))
        ));
        let sps_alone = [0, 0, 1, 0x67, 0x64, 0x00, 0x0A];
        assert!(matches!(
            decoder_config(&sps_alone),
            Err(Error::Bitstream(_))
        ));
        assert!(
            decoder_config(&[0, 0, 1, 0x67, 0x64]).is_err(),
            "truncated SPS"
        );
    }
}
