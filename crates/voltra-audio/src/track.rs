//! Per-track state, and the handle the interface holds.
//!
//! The audio thread must never wait for anything (CLAUDE.md §4.3), so the
//! interface does not hand it settings through a lock — it publishes them into
//! atomics that the mixer reads once per block.
//!
//! `Relaxed` ordering is enough here and is the honest choice rather than a
//! shortcut. There is nothing to order these loads against: each value is
//! independent, none of them guards access to other memory, and a fader whose
//! new value arrives one block late is indistinguishable from the same fader
//! moved 21 ms later. Reaching for `SeqCst` would only add a fence that buys
//! nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::balance::Balance;
use crate::gain::Gain;
use crate::level::{LevelPublisher, Levels};

/// Identifies a track inside one mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(u32);

impl TrackId {
    /// Build an identifier from its raw value.
    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        TrackId(value)
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "track#{}", self.0)
    }
}

/// What one track's controls look like in memory.
///
/// Shared between the interface and the mixer. Every field is atomic, so there
/// is no lock to take and nothing for the audio thread to wait on.
#[derive(Debug)]
pub(crate) struct TrackControl {
    /// The target gain, as the bits of an `f32`.
    ///
    /// There is no `AtomicF32`, and this is the standard way round it: the bit
    /// pattern is what is shared, and both sides agree it is a float.
    gain_bits: AtomicU32,
    /// The target balance position, as the bits of an `f32`.
    balance_bits: AtomicU32,
    muted: AtomicBool,
    /// Where the mixer leaves this track's levels for the interface.
    pub(crate) levels: LevelPublisher,
}

impl TrackControl {
    fn new(gain: Gain) -> Self {
        Self {
            gain_bits: AtomicU32::new(gain.linear().to_bits()),
            balance_bits: AtomicU32::new(Balance::CENTRE.position().to_bits()),
            muted: AtomicBool::new(false),
            levels: LevelPublisher::new(),
        }
    }

    /// The target gain, accounting for mute.
    pub(crate) fn target(&self) -> Gain {
        if self.muted.load(Ordering::Relaxed) {
            return Gain::SILENT;
        }
        let linear = f32::from_bits(self.gain_bits.load(Ordering::Relaxed));
        // The setter only ever stores a validated `Gain`, so this cannot be
        // negative or NaN; falling back to silence keeps the mixer total even if
        // that ever stopped being true.
        Gain::from_linear(linear).unwrap_or(Gain::SILENT)
    }

    /// The target balance.
    pub(crate) fn balance(&self) -> Balance {
        let position = f32::from_bits(self.balance_bits.load(Ordering::Relaxed));
        // As with the gain: the setter only stores a validated `Balance`, and
        // falling back to centre keeps the mix intact if that ever changed.
        Balance::from_position(position).unwrap_or(Balance::CENTRE)
    }
}

/// The interface's end of a track.
///
/// Cloneable and safe to move to another thread: every method here is a single
/// atomic store, so adjusting a fader never blocks the audio thread and never
/// blocks the interface either.
///
/// # Examples
///
/// ```
/// use voltra_audio::{ChannelLayout, Gain, Mixer, SampleRate};
///
/// let mut mixer = Mixer::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 8)?;
/// let (id, fader) = mixer.add_track()?;
///
/// fader.set_gain(Gain::from_db(-6.0));
/// fader.set_muted(true);
/// assert!(fader.is_muted());
/// assert_eq!(id.get(), 0);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct TrackHandle {
    id: TrackId,
    control: Arc<TrackControl>,
}

impl TrackHandle {
    pub(crate) fn new(id: TrackId, gain: Gain) -> (Self, Arc<TrackControl>) {
        let control = Arc::new(TrackControl::new(gain));
        (
            Self {
                id,
                control: Arc::clone(&control),
            },
            control,
        )
    }

    /// Which track this controls.
    #[must_use]
    pub const fn id(&self) -> TrackId {
        self.id
    }

    /// Set the track's gain.
    ///
    /// Takes effect over the next block, ramped rather than jumped — see
    /// [`Mixer`](crate::Mixer).
    pub fn set_gain(&self, gain: Gain) {
        self.control
            .gain_bits
            .store(gain.linear().to_bits(), Ordering::Relaxed);
    }

    /// The track's gain, ignoring mute.
    #[must_use]
    pub fn gain(&self) -> Gain {
        let linear = f32::from_bits(self.control.gain_bits.load(Ordering::Relaxed));
        Gain::from_linear(linear).unwrap_or(Gain::SILENT)
    }

    /// Silence or unsilence the track.
    ///
    /// Separate from setting the gain to zero so that unmuting restores the
    /// fader where the user left it, which is what a mute button means.
    pub fn set_muted(&self, muted: bool) {
        self.control.muted.store(muted, Ordering::Relaxed);
    }

    /// Whether the track is muted.
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.control.muted.load(Ordering::Relaxed)
    }

    /// Set where the track sits between the speakers.
    ///
    /// Ramped rather than jumped, like the gain: the two are folded into one
    /// per-channel gain, so moving either cannot click.
    pub fn set_balance(&self, balance: Balance) {
        self.control
            .balance_bits
            .store(balance.position().to_bits(), Ordering::Relaxed);
    }

    /// Where the track sits between the speakers.
    #[must_use]
    pub fn balance(&self) -> Balance {
        self.control.balance()
    }

    /// What this track contributed to the last block, per channel.
    ///
    /// Measured after gain, balance and mute, which is what a channel meter is
    /// expected to show: the track's contribution to the mix rather than what
    /// arrived at its input.
    ///
    /// Reading never blocks the audio thread and is never blocked by it.
    #[must_use]
    pub fn levels(&self, channels: usize) -> Levels {
        self.control.levels.read(channels)
    }
}

#[cfg(test)]
mod tests {
    use super::{TrackHandle, TrackId};
    use crate::balance::Balance;
    use crate::gain::Gain;
    use crate::level::ChannelLevel;

    #[test]
    fn a_handle_reads_back_what_it_set() {
        let (handle, control) = TrackHandle::new(TrackId::from_raw(3), Gain::UNITY);
        assert_eq!(handle.id(), TrackId::from_raw(3));
        assert_eq!(control.target(), Gain::UNITY);

        handle.set_gain(Gain::from_db(-6.0));
        assert!((handle.gain().db() + 6.0).abs() < 1e-4);
        assert!((control.target().db() + 6.0).abs() < 1e-4);
    }

    /// Mute has to be separate from a zero fader, or unmuting would lose the
    /// level the user set.
    #[test]
    fn mute_hides_the_gain_without_losing_it() {
        let (handle, control) = TrackHandle::new(TrackId::from_raw(0), Gain::UNITY);
        handle.set_gain(Gain::from_db(-3.0));
        handle.set_muted(true);

        assert!(control.target().is_silent());
        assert!(!handle.gain().is_silent(), "the fader itself must not move");

        handle.set_muted(false);
        assert!((control.target().db() + 3.0).abs() < 1e-4);
    }

    /// The handle crosses threads, which is the whole point of the atomics.
    #[test]
    fn a_handle_can_be_moved_to_another_thread() {
        let (handle, control) = TrackHandle::new(TrackId::from_raw(1), Gain::UNITY);
        let far = handle.clone();
        std::thread::spawn(move || far.set_gain(Gain::SILENT))
            .join()
            .unwrap();
        assert!(control.target().is_silent());
    }

    /// Balance travels the same lock-free way the gain does.
    #[test]
    fn balance_crosses_the_handle_too() {
        let (handle, control) = TrackHandle::new(TrackId::from_raw(2), Gain::UNITY);
        assert_eq!(handle.balance(), Balance::CENTRE);

        let hard_left = Balance::from_position(-1.0).unwrap();
        handle.set_balance(hard_left);
        assert_eq!(handle.balance(), hard_left);
        assert_eq!(control.balance(), hard_left);
    }

    /// A handle reads the levels the mixer published, without a lock either way.
    #[test]
    fn levels_come_back_through_the_handle() {
        let (handle, control) = TrackHandle::new(TrackId::from_raw(5), Gain::UNITY);
        assert_eq!(handle.levels(2).peak(), 0.0);

        control.levels.publish(
            0,
            ChannelLevel {
                peak: 0.5,
                rms: 0.25,
            },
        );
        let levels = handle.levels(2);
        assert!((levels.peak() - 0.5).abs() < 1e-6);
        assert!((levels.channel(0).unwrap().rms - 0.25).abs() < 1e-6);
    }

    #[test]
    fn identifiers_read_well() {
        assert_eq!(TrackId::from_raw(7).to_string(), "track#7");
    }
}
