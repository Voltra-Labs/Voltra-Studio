//! Level meters: peak, true peak and RMS, read lock-free from another thread.
//!
//! The audio thread **accumulates** what happened since the last read; the
//! reader takes it and resets. No peak is lost however late the reader is, and
//! neither side ever waits for the other. libobs gets the same guarantee by
//! calling the interface from the audio thread under three mutexes
//! (`docs/references/audio-meters.md` §2); here it costs a few atomics per
//! channel and block.

mod cell;
mod levels;
mod standalone;
mod stats;
mod true_peak;

pub use levels::{ChannelLevels, Levels};
pub use standalone::{Meter, MeterReader, PeakMode};
