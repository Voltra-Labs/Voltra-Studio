//! The audio vocabulary: rate, channels, sample formats and the buffer.
//!
//! The counterpart of [`frame`](crate::frame) for sound. Two decisions shape
//! everything here, both taken from libobs and written up in
//! `docs/references/audio.md`:
//!
//! 1. **The internal format is 32-bit float, planar, and it is the type rather
//!    than a value in an enum.** Mixing is addition, and addition in integers
//!    clips irreversibly; in float the sum lives outside `[-1, 1]` until it is
//!    limited exactly once, on the way out. Planar because a filter, a volume
//!    and a pan all work on one channel, and a channel that is a contiguous
//!    slice vectorises on its own.
//! 2. **Depth and interleaving are two axes, not nine enum values.** libobs's
//!    `audio_format` mixes them, so asking whether a format is planar means
//!    enumerating half the list. Here [`SampleFormat`] says how wide a sample
//!    is and [`SampleOrder`] says how samples are arranged.
//!
//! Sample formats exist only at the **edge** — what a device hands over, what an
//! encoder expects. Nothing in the core looks at them.

mod buffer;
mod convert;
mod format;

pub use buffer::AudioBuffer;
pub use format::{ChannelLayout, MAX_CHANNELS, SampleFormat, SampleOrder, SampleRate, SampleSpec};
