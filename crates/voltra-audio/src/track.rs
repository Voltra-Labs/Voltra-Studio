//! Track identity and the lock-free control shared with the interface.
//!
//! libobs queues every volume change as an `audio_action` behind a mutex that
//! the audio thread takes for each source and block, while the interface may be
//! inside that same lock growing the queue with `realloc`
//! (`docs/references/audio-mixer.md` §2). Here each parameter is a single
//! atomic: the interface stores, the mixer loads once per block, and neither can
//! make the other wait.
//!
//! Every access is `Ordering::Relaxed`. Each parameter stands alone and
//! publishes no other memory, so nothing needs ordering against anything else.
//! The one visible consequence: when the interface changes gain and balance
//! together, the mixer may pick one up a block before the other. With ramps,
//! that is two smooth 21 ms transitions instead of one, which nobody hears.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::gain::{Balance, Gain};

/// Identifies a track within one [`Mixer`](crate::Mixer). Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(u32);

impl TrackId {
    pub(crate) const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw identifier, for logs and interfaces.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "track {}", self.0)
    }
}

/// The parameters of one track, shared between the interface and the mixer.
#[derive(Debug)]
pub(crate) struct TrackParams {
    gain: AtomicU32,
    balance: AtomicU32,
    muted: AtomicBool,
    faults: AtomicU64,
}

impl TrackParams {
    fn new() -> Self {
        Self {
            gain: AtomicU32::new(Gain::UNITY.linear().to_bits()),
            balance: AtomicU32::new(Balance::CENTER.position().to_bits()),
            muted: AtomicBool::new(false),
            faults: AtomicU64::new(0),
        }
    }

    /// The gain the mixer should reach this block, mute folded in.
    pub(crate) fn effective_gain(&self) -> Gain {
        if self.muted.load(Ordering::Relaxed) {
            Gain::SILENT
        } else {
            Gain::from_bits(self.gain.load(Ordering::Relaxed))
        }
    }

    pub(crate) fn balance(&self) -> Balance {
        Balance::from_bits(self.balance.load(Ordering::Relaxed))
    }

    /// Record that this track lost a block.
    pub(crate) fn fault(&self) {
        self.faults.fetch_add(1, Ordering::Relaxed);
    }
}

/// The handle an interface keeps to drive one track.
///
/// Cheap to clone (one reference count), `Send` and `Sync`. Every method is a
/// single atomic operation: calling them from any thread, at any rate, never
/// delays the audio thread. Changes take effect at the next block, as a ramp.
///
/// # Examples
///
/// ```
/// use voltra_audio::{Balance, Gain, MixSpec, Mixer};
/// use voltra_core::{ChannelLayout, SampleRate};
///
/// let mut mixer = Mixer::new(MixSpec::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 1024)?)?;
/// let track = mixer.add_track(ChannelLayout::Stereo)?;
///
/// let fader = track.clone();
/// std::thread::spawn(move || fader.set_gain(Gain::from_db(-6.0).unwrap()))
///     .join()
///     .unwrap();
///
/// track.set_balance(Balance::new(-0.5)?);
/// track.set_muted(true);
/// assert!(track.is_muted());
/// # Ok::<(), voltra_audio::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct TrackControl {
    id: TrackId,
    params: Arc<TrackParams>,
}

impl TrackControl {
    /// A control and the shared parameters the mixer keeps for itself.
    pub(crate) fn new(id: TrackId) -> (Self, Arc<TrackParams>) {
        let params = Arc::new(TrackParams::new());
        (
            Self {
                id,
                params: Arc::clone(&params),
            },
            params,
        )
    }

    /// The track this controls.
    pub const fn id(&self) -> TrackId {
        self.id
    }

    /// Set the volume. Muting is separate and does not lose this value.
    pub fn set_gain(&self, gain: Gain) {
        self.params
            .gain
            .store(gain.linear().to_bits(), Ordering::Relaxed);
    }

    /// The volume last set, regardless of mute.
    pub fn gain(&self) -> Gain {
        Gain::from_bits(self.params.gain.load(Ordering::Relaxed))
    }

    /// Set the balance (stereo track) or pan (mono track in a stereo mix).
    pub fn set_balance(&self, balance: Balance) {
        self.params
            .balance
            .store(balance.position().to_bits(), Ordering::Relaxed);
    }

    /// The balance last set.
    pub fn balance(&self) -> Balance {
        self.params.balance()
    }

    /// Mute or unmute. Fades over one block rather than cutting.
    pub fn set_muted(&self, muted: bool) {
        self.params.muted.store(muted, Ordering::Relaxed);
    }

    /// Whether the track is muted.
    pub fn is_muted(&self) -> bool {
        self.params.muted.load(Ordering::Relaxed)
    }

    /// How many blocks this track has lost to its own faults: a non-finite
    /// sample, the wrong rate or length, or a misaligned timestamp.
    ///
    /// A block that simply did not arrive is not a fault; it is counted in
    /// [`MixReport::missing`](crate::MixReport::missing).
    pub fn faults(&self) -> u64 {
        self.params.faults.load(Ordering::Relaxed)
    }
}
