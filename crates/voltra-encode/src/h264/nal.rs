//! NAL units in Annex B, and their length-prefixed form.

/// The NAL unit types the tools look at (ITU-T H.264 table 7-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NalType {
    /// A slice of a non-IDR picture.
    Slice,
    /// A slice of an IDR picture: a keyframe.
    Idr,
    /// Supplemental enhancement information.
    Sei,
    /// Sequence parameter set.
    Sps,
    /// Picture parameter set.
    Pps,
    /// Access unit delimiter.
    Aud,
    /// Anything else, with its 5-bit type.
    Other(u8),
}

impl NalType {
    /// The type of a NAL unit from its first byte.
    pub const fn of(unit: &[u8]) -> Option<Self> {
        let Some(&header) = unit.first() else {
            return None;
        };
        Some(match header & 0x1F {
            1 => Self::Slice,
            5 => Self::Idr,
            6 => Self::Sei,
            7 => Self::Sps,
            8 => Self::Pps,
            9 => Self::Aud,
            other => Self::Other(other),
        })
    }
}

/// The NAL units of an Annex B stream, without their start codes.
///
/// Accepts three- and four-byte start codes, mixed — x264 writes both in one
/// stream — and drops the trailing zero bytes a four-byte start code leaves at
/// the end of the unit before it. A unit never ends in zero: its RBSP ends in
/// a stop bit.
///
/// # Examples
///
/// ```
/// use voltra_encode::h264::nal_units;
///
/// let stream = [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB];
/// let units: Vec<&[u8]> = nal_units(&stream).collect();
/// assert_eq!(units, [&[0x67, 0xAA][..], &[0x68, 0xBB][..]]);
/// ```
pub fn nal_units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = after_start_code(stream);
    std::iter::from_fn(move || {
        let current = rest?;
        let (unit, next) = match find_start_code(current) {
            Some(at) => (&current[..at], after_start_code(&current[at..])),
            None => (current, None),
        };
        rest = next;
        let end = unit
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |last| last + 1);
        Some(&unit[..end])
    })
    .filter(|unit| !unit.is_empty())
}

/// Whether the packet contains an IDR slice.
pub fn is_keyframe(stream: &[u8]) -> bool {
    nal_units(stream).any(|unit| NalType::of(unit) == Some(NalType::Idr))
}

/// Rewrite an Annex B packet with a four-byte length before each unit, into
/// `out`, which is cleared first. Reusing `out` across frames means no
/// allocation once it has grown to the largest packet.
///
/// # Examples
///
/// ```
/// use voltra_encode::h264::to_length_prefixed;
///
/// let mut out = Vec::new();
/// to_length_prefixed(&[0, 0, 1, 0x65, 0x88, 0x84], &mut out);
/// assert_eq!(out, [0, 0, 0, 3, 0x65, 0x88, 0x84]);
/// ```
pub fn to_length_prefixed(stream: &[u8], out: &mut Vec<u8>) {
    out.clear();
    for unit in nal_units(stream) {
        let length = u32::try_from(unit.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(unit);
    }
}

/// Where the next `00 00 01` begins.
fn find_start_code(data: &[u8]) -> Option<usize> {
    data.windows(3).position(|window| window == [0, 0, 1])
}

/// The bytes after the first start code, if there is one.
fn after_start_code(data: &[u8]) -> Option<&[u8]> {
    find_start_code(data).map(|at| &data[at + 3..])
}

#[cfg(test)]
mod tests {
    use super::{NalType, is_keyframe, nal_units, to_length_prefixed};

    /// The headers and first two packets x264 wrote for a 64×48 High stream:
    /// SPS and PPS with four-byte start codes, then SEI and IDR with three.
    pub(crate) const SPS: [u8; 22] = [
        0x67, 0x64, 0x00, 0x0A, 0xAC, 0xB4, 0x23, 0xD8, 0x08, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80,
        0x00, 0x00, 0x1E, 0x07, 0x89, 0x13, 0x50,
    ];
    pub(crate) const PPS: [u8; 4] = [0x68, 0xEF, 0x0F, 0xCB];

    fn stream() -> Vec<u8> {
        let mut stream = vec![0, 0, 0, 1];
        stream.extend_from_slice(&SPS);
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&PPS);
        stream.extend_from_slice(&[0, 0, 1, 0x06, 0x05, 0x11, 0x80]);
        stream.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x21]);
        stream
    }

    #[test]
    fn units_split_on_both_start_code_lengths() {
        let stream = stream();
        let types: Vec<_> = nal_units(&stream)
            .map(|unit| NalType::of(unit).unwrap())
            .collect();
        assert_eq!(
            types,
            [NalType::Sps, NalType::Pps, NalType::Sei, NalType::Idr]
        );
        let units: Vec<_> = nal_units(&stream).collect();
        assert_eq!(
            units[0], SPS,
            "the SPS keeps its emulation-prevention bytes"
        );
        assert_eq!(
            units[1], PPS,
            "the next start code's leading zero is not part of it"
        );
        assert_eq!(
            units[3],
            [0x65, 0x88, 0x84, 0x00, 0x21],
            "zeros inside a unit stay"
        );
    }

    #[test]
    fn keyframes_are_idr_slices() {
        assert!(is_keyframe(&stream()));
        assert!(!is_keyframe(&[0, 0, 1, 0x41, 0x9A]));
        assert!(!is_keyframe(&[]));
    }

    #[test]
    fn length_prefixes_replace_start_codes_exactly() {
        let mut out = Vec::new();
        to_length_prefixed(&stream(), &mut out);
        let mut expected = vec![0, 0, 0, 22];
        expected.extend_from_slice(&SPS);
        expected.extend_from_slice(&[0, 0, 0, 4]);
        expected.extend_from_slice(&PPS);
        expected.extend_from_slice(&[0, 0, 0, 4, 0x06, 0x05, 0x11, 0x80]);
        expected.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x00, 0x21]);
        assert_eq!(out, expected);
    }

    #[test]
    fn the_output_buffer_is_reused() {
        let mut out = Vec::with_capacity(64);
        let pointer = out.as_ptr();
        for _ in 0..100 {
            to_length_prefixed(&stream(), &mut out);
        }
        assert_eq!(
            out.as_ptr(),
            pointer,
            "no reallocation once it is large enough"
        );
    }
}
