//! H.264 bitstream tools: from what an encoder produces to what a container
//! stores.
//!
//! Encoders like x264, NVENC and VAAPI write **Annex B**: each NAL unit after a
//! start code. Matroska and MP4 want each unit after its **length** and the
//! parameter sets in an `AVCDecoderConfigurationRecord` (`avcC`). These follow
//! libobs's `obs-avc.c` (`docs/references/encoding.md` §3), except that
//! rewriting a packet reuses the caller's buffer instead of allocating a new
//! one per frame.

mod config;
mod nal;

pub use config::decoder_config;
pub use nal::{NalType, is_keyframe, nal_units, to_length_prefixed};
