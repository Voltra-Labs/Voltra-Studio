//! The EBML primitives Matroska is made of: element IDs, variable-length
//! sizes and the few value types the muxer writes (RFC 8794).

/// Element IDs used by the writer, with their length-marker bits included as
/// the specification lists them.
pub(crate) mod id {
    pub(crate) const EBML: u32 = 0x1A45_DFA3;
    pub(crate) const EBML_VERSION: u32 = 0x4286;
    pub(crate) const EBML_READ_VERSION: u32 = 0x42F7;
    pub(crate) const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub(crate) const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub(crate) const DOC_TYPE: u32 = 0x4282;
    pub(crate) const DOC_TYPE_VERSION: u32 = 0x4287;
    pub(crate) const DOC_TYPE_READ_VERSION: u32 = 0x4285;

    pub(crate) const SEGMENT: u32 = 0x1853_8067;
    pub(crate) const INFO: u32 = 0x1549_A966;
    pub(crate) const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub(crate) const DURATION: u32 = 0x4489;
    pub(crate) const MUXING_APP: u32 = 0x4D80;
    pub(crate) const WRITING_APP: u32 = 0x5741;

    pub(crate) const TRACKS: u32 = 0x1654_AE6B;
    pub(crate) const TRACK_ENTRY: u32 = 0xAE;
    pub(crate) const TRACK_NUMBER: u32 = 0xD7;
    pub(crate) const TRACK_UID: u32 = 0x73C5;
    pub(crate) const TRACK_TYPE: u32 = 0x83;
    pub(crate) const FLAG_LACING: u32 = 0x9C;
    pub(crate) const CODEC_ID: u32 = 0x86;
    pub(crate) const CODEC_PRIVATE: u32 = 0x63A2;
    pub(crate) const DEFAULT_DURATION: u32 = 0x23_E383;
    pub(crate) const VIDEO: u32 = 0xE0;
    pub(crate) const PIXEL_WIDTH: u32 = 0xB0;
    pub(crate) const PIXEL_HEIGHT: u32 = 0xBA;
    pub(crate) const COLOUR_SPACE: u32 = 0x2E_B524;
    pub(crate) const AUDIO: u32 = 0xE1;
    pub(crate) const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub(crate) const CHANNELS: u32 = 0x9F;
    pub(crate) const BIT_DEPTH: u32 = 0x6264;

    pub(crate) const CLUSTER: u32 = 0x1F43_B675;
    pub(crate) const TIMESTAMP: u32 = 0xE7;
    pub(crate) const SIMPLE_BLOCK: u32 = 0xA3;
}

/// "Unknown size": a one-byte length marker of 8 followed by all ones. The
/// element ends where the next element of its level begins.
pub(crate) const UNKNOWN_SIZE: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Append an element ID: its bytes big-endian, without leading zeros. IDs
/// carry their own length marker, so the value is written as is.
pub(crate) fn put_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|&&byte| byte == 0).count().min(3);
    out.extend_from_slice(&bytes[skip..]);
}

/// Append `value` as a variable-length size in the fewest bytes.
///
/// A length of `n` bytes holds values up to `2^(7n) − 2`: all ones is reserved
/// for "unknown", which is why 127 needs two bytes.
pub(crate) fn put_size(out: &mut Vec<u8>, value: u64) {
    let mut length = 1;
    while length < 8 && value >= (1u64 << (7 * length)) - 1 {
        length += 1;
    }
    let marked = value | (1u64 << (7 * length));
    out.extend_from_slice(&marked.to_be_bytes()[8 - length..]);
}

/// An unsigned integer element in the fewest bytes (at least one).
pub(crate) fn put_uint(out: &mut Vec<u8>, id: u32, value: u64) {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take_while(|&&byte| byte == 0).count().min(7);
    put_id(out, id);
    put_size(out, (8 - skip) as u64);
    out.extend_from_slice(&bytes[skip..]);
}

/// A 64-bit float element. Returns where its 8 value bytes start in `out`, so
/// a placeholder can be patched later.
pub(crate) fn put_float(out: &mut Vec<u8>, id: u32, value: f64) -> usize {
    put_id(out, id);
    put_size(out, 8);
    let at = out.len();
    out.extend_from_slice(&value.to_be_bytes());
    at
}

/// A string or binary element.
pub(crate) fn put_bytes(out: &mut Vec<u8>, id: u32, value: &[u8]) {
    put_id(out, id);
    put_size(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// A master element of known size around `children`.
pub(crate) fn put_master(out: &mut Vec<u8>, id: u32, children: &[u8]) {
    put_bytes(out, id, children);
}

#[cfg(test)]
mod tests {
    use super::{put_bytes, put_float, put_id, put_size, put_uint};

    fn size(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_size(&mut out, value);
        out
    }

    #[test]
    fn sizes_take_the_fewest_bytes_and_never_look_unknown() {
        assert_eq!(size(0), [0x80]);
        assert_eq!(size(126), [0xFE]);
        assert_eq!(size(127), [0x40, 0x7F], "0xFF alone would mean unknown");
        assert_eq!(size(16_382), [0x7F, 0xFE]);
        assert_eq!(size(16_383), [0x20, 0x3F, 0xFF]);
        // A 1080p I420 frame plus a block header: past 2^21 − 2, four bytes.
        assert_eq!(size(3_110_400 + 4), [0x10, 0x2F, 0x76, 0x04]);
        assert_eq!(size((1 << 56) - 2).len(), 8);
    }

    #[test]
    fn ids_keep_their_marker_and_drop_leading_zeros() {
        let mut out = Vec::new();
        put_id(&mut out, 0xA3);
        put_id(&mut out, 0x4286);
        put_id(&mut out, 0x2A_D7B1);
        put_id(&mut out, 0x1A45_DFA3);
        assert_eq!(
            out,
            [0xA3, 0x42, 0x86, 0x2A, 0xD7, 0xB1, 0x1A, 0x45, 0xDF, 0xA3]
        );
    }

    #[test]
    fn values_are_encoded_compactly() {
        let mut out = Vec::new();
        put_uint(&mut out, 0xD7, 1);
        put_uint(&mut out, 0xD7, 0);
        put_uint(&mut out, 0x2A_D7B1, 1_000_000);
        assert_eq!(
            out,
            [
                0xD7, 0x81, 0x01, 0xD7, 0x81, 0x00, 0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40
            ]
        );

        let mut out = Vec::new();
        put_bytes(&mut out, 0x86, b"A_PCM/FLOAT/IEEE");
        assert_eq!(out[..2], [0x86, 0x90]);
        let at = put_float(&mut out, 0x4489, 2.5);
        assert_eq!(&out[at..], &2.5f64.to_be_bytes());
    }
}
