//! Summing tracks into one output block.
//!
//! # What happens in `mix`, and what does not
//!
//! Does: read one atomic per track, walk each channel once, multiply and
//! accumulate.
//!
//! Does **not**: allocate, take a lock, make a syscall, or format a string.
//! That is the exit criterion of phase 3 and it is enforced by a test with a
//! counting allocator (`tests/no_allocation.rs`), not by good intentions.
//!
//! # Summing follows libobs, ramping does not
//!
//! `mix_audio()` in libobs is `*(mix++) += *(aud++)` — plain addition, no
//! normalisation by track count, no clipping. Both are adopted here:
//! normalising by count would make raising one track lower the others, and
//! clipping belongs at the single point where the mix leaves float, which is the
//! edge conversion of plan 011.
//!
//! What is *not* adopted is applying a gain change as a step. A gain that jumps
//! between blocks leaves a discontinuity in the waveform, and a discontinuity is
//! energy at every frequency — a click. At 48 kHz with 1024-sample blocks,
//! dragging a fader would click every 21 ms. Interpolating across the block
//! costs a multiply-accumulate per sample instead of a multiply.

use std::sync::Arc;

use voltra_core::{AudioBuffer, ChannelLayout, Error, Result, SampleRate};

use crate::gain::Gain;
use crate::track::{TrackControl, TrackHandle, TrackId};

/// How long a gain change takes to complete, in milliseconds.
///
/// Short enough to feel immediate on a fader, long enough that the step per
/// sample is far below anything audible as a click. Consoles use single-digit
/// to low-double-digit milliseconds; this sits in the middle.
const RAMP_MILLIS: f32 = 10.0;

/// One track the mixer knows about.
#[derive(Debug)]
struct Track {
    id: TrackId,
    control: Arc<TrackControl>,
    /// The gain actually applied to the last sample of the previous block.
    ///
    /// Kept here rather than read from the control so a ramp continues smoothly
    /// across a block boundary instead of restarting.
    current: f32,
}

/// One track's contribution to a block.
#[derive(Debug, Clone, Copy)]
pub struct MixInput<'a> {
    /// Which track these samples belong to.
    pub track: TrackId,
    /// The samples, already aligned to the output block.
    pub samples: &'a AudioBuffer,
}

impl<'a> MixInput<'a> {
    /// Build an input.
    #[must_use]
    pub const fn new(track: TrackId, samples: &'a AudioBuffer) -> Self {
        Self { track, samples }
    }
}

/// What the last mix did.
///
/// Always on, per CLAUDE.md §4.10.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MixStats {
    /// Tracks that contributed samples.
    pub mixed: u32,
    /// Inputs skipped: silent, muted, or naming a track the mixer does not have.
    pub skipped: u32,
    /// Inputs refused because their rate or layout did not match the mixer.
    pub rejected: u32,
    /// Tracks whose gain was still moving toward its target.
    pub ramping: u32,
    /// The largest absolute sample in the output.
    ///
    /// Above 1.0 means the mix will clip when it is converted for output. The
    /// mixer does not clip it — that happens once, at the edge — but staying
    /// quiet about it would leave nobody able to see it coming.
    pub peak: f32,
}

/// Sums tracks into one output block.
///
/// # Examples
///
/// ```
/// use voltra_audio::{ChannelLayout, Gain, MixInput, Mixer, SampleRate};
/// use voltra_core::{AudioBuffer, Timestamp};
///
/// let rate = SampleRate::HZ_48000;
/// let mut mixer = Mixer::new(rate, ChannelLayout::Stereo, 8)?;
/// let (id, fader) = mixer.add_track()?;
/// fader.set_gain(Gain::UNITY);
///
/// let mut source = AudioBuffer::new(rate, ChannelLayout::Stereo, 512, Timestamp::ZERO)?;
/// source.as_mut_slice().fill(0.25);
///
/// let mut out = AudioBuffer::new(rate, ChannelLayout::Stereo, 512, Timestamp::ZERO)?;
/// mixer.mix(&[MixInput::new(id, &source)], &mut out)?;
///
/// assert_eq!(mixer.stats().mixed, 1);
/// // The gain starts at unity, so the tail of the block is the source itself.
/// assert!((out.channel(0).expect("left")[511] - 0.25).abs() < 1e-6);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct Mixer {
    rate: SampleRate,
    layout: ChannelLayout,
    tracks: Vec<Track>,
    next_id: u32,
    stats: MixStats,
}

impl Mixer {
    /// Build a mixer with room for `capacity` tracks.
    ///
    /// The capacity is reserved up front so that adding tracks later does not
    /// reallocate while the audio thread might be reading. Going past it still
    /// works — [`add_track`](Mixer::add_track) is control-plane code and may
    /// allocate — but it is the caller saying how many they expect.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `capacity` is zero.
    pub fn new(rate: SampleRate, layout: ChannelLayout, capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(Error::config("a mixer needs room for at least one track"));
        }
        Ok(Self {
            rate,
            layout,
            tracks: Vec::with_capacity(capacity),
            next_id: 0,
            stats: MixStats::default(),
        })
    }

    /// The rate every track must arrive at.
    #[must_use]
    pub const fn rate(&self) -> SampleRate {
        self.rate
    }

    /// The layout every track must arrive in.
    #[must_use]
    pub const fn layout(&self) -> ChannelLayout {
        self.layout
    }

    /// How many tracks the mixer has.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// What the last mix did.
    #[must_use]
    pub const fn stats(&self) -> MixStats {
        self.stats
    }

    /// Add a track, returning its identifier and the interface's handle.
    ///
    /// **Control plane**: this allocates and is called from the interface
    /// thread, never from the audio callback.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the identifier space is exhausted, which takes
    /// four billion tracks and means something has gone very wrong.
    pub fn add_track(&mut self) -> Result<(TrackId, TrackHandle)> {
        let id = TrackId::from_raw(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| Error::config("ran out of track identifiers"))?;

        let (handle, control) = TrackHandle::new(id, Gain::UNITY);
        self.tracks.push(Track {
            id,
            control,
            // Start at the target rather than at zero, so a track added
            // mid-session does not fade in unasked.
            current: Gain::UNITY.linear(),
        });
        Ok((id, handle))
    }

    /// Remove a track.
    ///
    /// **Control plane**: called from the interface thread. Returns whether the
    /// track was there.
    pub fn remove_track(&mut self, id: TrackId) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|track| track.id != id);
        self.tracks.len() != before
    }

    /// Sum `inputs` into `out`.
    ///
    /// **Data plane**: allocates nothing, locks nothing. `out` is silenced
    /// first, so the caller hands over any buffer and gets the mix, not the mix
    /// plus whatever was there.
    ///
    /// An input naming a track the mixer does not have is counted and skipped
    /// rather than refused: one source disappearing must not take the broadcast
    /// with it (CLAUDE.md §5).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `out` does not match the mixer's rate or layout.
    /// Inputs that do not match are counted in
    /// [`MixStats::rejected`] instead, for the same reason as unknown tracks.
    pub fn mix(&mut self, inputs: &[MixInput<'_>], out: &mut AudioBuffer) -> Result<()> {
        if out.rate() != self.rate || out.layout() != self.layout {
            return Err(Error::config(format!(
                "mixer is {} {}, output is {} {}",
                self.rate,
                self.layout,
                out.rate(),
                out.layout()
            )));
        }

        let mut stats = MixStats::default();
        out.silence();
        let frames = out.frames();

        // One atomic read per track per block — 47 reads a second at 48 kHz,
        // not one per sample.
        let ramp_frames = self.ramp_frames();

        for track in &mut self.tracks {
            let target = track.control.target().linear();

            let Some(input) = inputs.iter().find(|input| input.track == track.id) else {
                // No samples this block. The gain still moves, so a fader
                // dragged while a track is silent is already where the user put
                // it when audio comes back.
                track.current = target;
                continue;
            };
            if input.samples.rate() != self.rate || input.samples.layout() != self.layout {
                stats.rejected += 1;
                track.current = target;
                continue;
            }

            let usable = frames.min(input.samples.frames());
            if settled(track.current, target) {
                if target == 0.0 {
                    stats.skipped += 1;
                    continue;
                }
                add_scaled(input.samples, out, usable, target);
            } else {
                track.current = add_ramped(
                    input.samples,
                    out,
                    usable,
                    track.current,
                    target,
                    ramp_frames,
                );
                if !settled(track.current, target) {
                    stats.ramping += 1;
                }
            }
            stats.mixed += 1;
        }

        stats.skipped += u32::try_from(
            inputs
                .iter()
                .filter(|input| !self.tracks.iter().any(|track| track.id == input.track))
                .count(),
        )
        .unwrap_or(u32::MAX);

        stats.peak = peak_of(out);
        self.stats = stats;
        Ok(())
    }

    /// How many samples a full gain change is spread over.
    fn ramp_frames(&self) -> f32 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "sample rates are far below 2^24, so this is exact"
        )]
        {
            (self.rate.hz() as f32 * RAMP_MILLIS / 1000.0).max(1.0)
        }
    }
}

/// Whether a track's gain has arrived at its target.
///
/// An exact comparison, and deliberately so: this is not two numbers that
/// happen to be close, it is one value that was *assigned* from the other.
/// [`add_ramped`] returns `target` itself the moment the ramp completes, so the
/// two are the same bits. Comparing within a margin instead would leave the
/// mixer permanently in the ramping branch, a few multiplies short of the fast
/// path, for no benefit.
#[allow(
    clippy::float_cmp,
    reason = "current is assigned from target, not computed toward it"
)]
fn settled(current: f32, target: f32) -> bool {
    current == target
}

/// `out += input * gain`, with the gain constant. The common case.
fn add_scaled(input: &AudioBuffer, out: &mut AudioBuffer, frames: usize, gain: f32) {
    for (source, target) in input.channels().zip(out.channels_mut()) {
        for (sample, slot) in source[..frames].iter().zip(target[..frames].iter_mut()) {
            *slot += sample * gain;
        }
    }
}

/// `out += input * gain`, with the gain walking toward `target`.
///
/// The block is split in two: the samples the ramp actually spans, and the
/// settled remainder. That is not only faster — neither loop carries the
/// "have I arrived yet" branch, so both vectorise — it is also more honest about
/// what happens. A 10 ms ramp inside a 21.3 ms block means more than half the
/// block is already at the target, and running it through ramp arithmetic was
/// pretending otherwise.
///
/// Returns the gain reached, so the next block continues from here rather than
/// restarting the ramp.
fn add_ramped(
    input: &AudioBuffer,
    out: &mut AudioBuffer,
    frames: usize,
    start: f32,
    target: f32,
    ramp_frames: f32,
) -> f32 {
    let step = (target - start) / ramp_frames;
    if step == 0.0 {
        add_scaled(input, out, frames, target);
        return target;
    }

    // How many samples until the gain arrives. `step` was chosen to cross the
    // whole distance in `ramp_frames` samples, so that is the span by
    // construction; the ceiling makes the last step land on the target rather
    // than one short of it.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ramp_frames is positive and small, and the result is clamped to the block"
    )]
    let span = (ramp_frames.ceil() as usize).min(frames);

    // Where the ramp left off. Taken from the loop rather than recomputed as
    // `start + step * span`, which would need a cast and could disagree with
    // what the loop actually applied.
    let mut reached = start;

    for (source, slot) in input.channels().zip(out.channels_mut()) {
        // Every channel walks the same ramp, so each starts from the same place.
        let mut gain = start;
        for (sample, out_sample) in source[..span].iter().zip(slot[..span].iter_mut()) {
            *out_sample += sample * gain;
            gain += step;
        }
        // The settled tail: no ramp arithmetic, no branch.
        for (sample, out_sample) in source[span..frames]
            .iter()
            .zip(slot[span..frames].iter_mut())
        {
            *out_sample += sample * target;
        }
        reached = gain;
    }

    if span < frames { target } else { reached }
}

/// The largest absolute sample in `buffer`.
fn peak_of(buffer: &AudioBuffer) -> f32 {
    buffer
        .as_slice()
        .iter()
        .fold(0.0f32, |worst, sample| worst.max(sample.abs()))
}

#[cfg(test)]
mod tests {
    use super::{MixInput, Mixer, RAMP_MILLIS};
    use crate::gain::Gain;
    use crate::track::TrackId;
    use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

    const RATE: SampleRate = SampleRate::HZ_48000;

    fn buffer(frames: usize, value: f32) -> AudioBuffer {
        let mut buffer =
            AudioBuffer::new(RATE, ChannelLayout::Stereo, frames, Timestamp::ZERO).unwrap();
        buffer.as_mut_slice().fill(value);
        buffer
    }

    fn mixer() -> Mixer {
        Mixer::new(RATE, ChannelLayout::Stereo, 8).unwrap()
    }

    /// How far the gain moves per sample, which bounds every step in a ramp.
    fn ramp_step() -> f32 {
        1.0 / (48_000.0 * RAMP_MILLIS / 1000.0)
    }

    #[test]
    fn a_single_unity_track_is_copied_through() {
        let mut mixer = mixer();
        let (id, _fader) = mixer.add_track().unwrap();
        let source = buffer(64, 0.25);
        let mut out = buffer(64, 999.0);

        mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();

        assert!(out.as_slice().iter().all(|s| (s - 0.25).abs() < 1e-6));
        assert_eq!(mixer.stats().mixed, 1);
        assert_eq!(mixer.stats().ramping, 0);
    }

    /// The output is silenced first, so a reused buffer contributes nothing.
    #[test]
    fn the_output_is_the_mix_and_not_what_was_there_before() {
        let mut mixer = mixer();
        let mut out = buffer(32, 0.9);
        mixer.mix(&[], &mut out).unwrap();
        assert!(out.as_slice().iter().all(|&s| s == 0.0));
    }

    #[test]
    fn opposite_signals_cancel_exactly() {
        let mut mixer = mixer();
        let (a, _fa) = mixer.add_track().unwrap();
        let (b, _fb) = mixer.add_track().unwrap();

        let positive = buffer(64, 0.5);
        let negative = buffer(64, -0.5);
        let mut out = buffer(64, 0.0);

        mixer
            .mix(
                &[MixInput::new(a, &positive), MixInput::new(b, &negative)],
                &mut out,
            )
            .unwrap();

        assert!(out.as_slice().iter().all(|&s| s == 0.0));
        assert_eq!(mixer.stats().mixed, 2);
        assert_eq!(mixer.stats().peak, 0.0);
    }

    /// libobs does not clip in the mix and neither do we: clipping belongs at
    /// the single point where the mix leaves float.
    #[test]
    fn the_sum_is_not_clipped() {
        let mut mixer = mixer();
        let (a, _fa) = mixer.add_track().unwrap();
        let (b, _fb) = mixer.add_track().unwrap();

        let loud = buffer(32, 0.8);
        let mut out = buffer(32, 0.0);
        mixer
            .mix(
                &[MixInput::new(a, &loud), MixInput::new(b, &loud)],
                &mut out,
            )
            .unwrap();

        assert!(out.as_slice().iter().all(|s| (s - 1.6).abs() < 1e-6));
        assert!(
            (mixer.stats().peak - 1.6).abs() < 1e-6,
            "the peak has to report the overshoot even though nothing clips it"
        );
    }

    /// Ten tracks at unity are ten times as loud. Normalising by count would
    /// make raising one track lower the others.
    #[test]
    fn tracks_are_not_normalised_by_count() {
        let mut mixer = Mixer::new(RATE, ChannelLayout::Stereo, 16).unwrap();
        let ids: Vec<TrackId> = (0..10).map(|_| mixer.add_track().unwrap().0).collect();
        let source = buffer(16, 0.1);
        let inputs: Vec<MixInput<'_>> = ids.iter().map(|id| MixInput::new(*id, &source)).collect();

        let mut out = buffer(16, 0.0);
        mixer.mix(&inputs, &mut out).unwrap();
        assert!(out.as_slice().iter().all(|s| (s - 1.0).abs() < 1e-5));
    }

    #[test]
    fn a_muted_track_contributes_nothing_and_is_counted() {
        let mut mixer = mixer();
        let (id, fader) = mixer.add_track().unwrap();
        fader.set_muted(true);

        let source = buffer(2048, 0.5);
        let mut out = buffer(2048, 0.0);

        // The first block ramps down to silence; by the second it is settled and
        // counted as skipped rather than mixed.
        mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();
        mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();

        assert!(out.as_slice().iter().all(|&s| s == 0.0));
        assert_eq!(mixer.stats().mixed, 0);
        assert_eq!(mixer.stats().skipped, 1);
    }

    /// The test that justifies ramping at all. Without it, the last sample of
    /// one block and the first of the next differ by the whole gain change —
    /// a step, which is a click.
    #[test]
    fn a_gain_change_leaves_no_discontinuity() {
        let mut mixer = mixer();
        let (id, fader) = mixer.add_track().unwrap();

        // Constant input, so the output *is* the gain curve.
        let source = buffer(1024, 1.0);
        let mut out = buffer(1024, 0.0);

        mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();
        let last_of_first_block = out.channel(0).unwrap()[1023];
        assert!((last_of_first_block - 1.0).abs() < 1e-6);

        // Slam the fader shut between blocks.
        fader.set_gain(Gain::SILENT);
        mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();
        let left = out.channel(0).unwrap();

        // The block boundary itself is continuous: the ramp starts where the
        // previous block ended, not at the new target.
        assert!(
            (left[0] - last_of_first_block).abs() <= ramp_step() * 2.0,
            "block boundary jumped from {last_of_first_block} to {}",
            left[0]
        );

        // And nothing inside the block jumps either.
        let tolerance = ramp_step() * 1.5;
        for window in left.windows(2) {
            assert!(
                (window[1] - window[0]).abs() <= tolerance,
                "step of {} exceeds the ramp step {tolerance}",
                (window[1] - window[0]).abs()
            );
        }
    }

    /// A ramp that never arrived would leave the steady-state gain wrong for
    /// ever while the sum tests still passed by a hair.
    #[test]
    fn a_ramp_reaches_its_target_without_overshooting() {
        for (from, to) in [(Gain::UNITY, Gain::SILENT), (Gain::SILENT, Gain::UNITY)] {
            let mut mixer = mixer();
            let (id, fader) = mixer.add_track().unwrap();
            fader.set_gain(from);

            let source = buffer(1024, 1.0);
            let mut out = buffer(1024, 0.0);
            // Settle at the starting gain.
            mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();

            fader.set_gain(to);
            // 10 ms of ramp is 480 samples, so one 1024-sample block is plenty.
            mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();
            let reached = out.channel(0).unwrap()[1023];

            assert!(
                (reached - to.linear()).abs() < 1e-6,
                "ramp from {from} to {to} reached {reached}"
            );
            assert_eq!(mixer.stats().ramping, 0, "the ramp should have finished");

            // And the block after is steady.
            mixer.mix(&[MixInput::new(id, &source)], &mut out).unwrap();
            assert!(
                out.as_slice()
                    .iter()
                    .all(|s| (s - to.linear()).abs() < 1e-6)
            );
        }
    }

    /// One source vanishing must not take the mix with it (CLAUDE.md §5).
    #[test]
    fn an_unknown_track_is_counted_not_fatal() {
        let mut mixer = mixer();
        let (known, _fader) = mixer.add_track().unwrap();
        let source = buffer(32, 0.5);
        let mut out = buffer(32, 0.0);

        mixer
            .mix(
                &[
                    MixInput::new(known, &source),
                    MixInput::new(TrackId::from_raw(999), &source),
                ],
                &mut out,
            )
            .unwrap();

        assert_eq!(mixer.stats().mixed, 1);
        assert_eq!(mixer.stats().skipped, 1);
        assert!(out.as_slice().iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    /// A track at the wrong rate would play at the wrong pitch. Refusing it is
    /// better than resampling it silently, which is a plan of its own.
    #[test]
    fn a_mismatched_input_is_rejected_not_mixed() {
        let mut mixer = mixer();
        let (id, _fader) = mixer.add_track().unwrap();

        let wrong_rate = AudioBuffer::new(
            SampleRate::HZ_44100,
            ChannelLayout::Stereo,
            32,
            Timestamp::ZERO,
        )
        .unwrap();
        let mut out = buffer(32, 0.0);
        mixer
            .mix(&[MixInput::new(id, &wrong_rate)], &mut out)
            .unwrap();

        assert_eq!(mixer.stats().rejected, 1);
        assert_eq!(mixer.stats().mixed, 0);
        assert!(out.as_slice().iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_mismatched_output_is_an_error() {
        let mut mixer = mixer();
        let mut mono = AudioBuffer::new(RATE, ChannelLayout::Mono, 32, Timestamp::ZERO).unwrap();
        assert!(mixer.mix(&[], &mut mono).is_err());
    }

    #[test]
    fn a_shorter_input_fills_what_it_can() {
        let mut mixer = mixer();
        let (id, _fader) = mixer.add_track().unwrap();
        let short = buffer(16, 1.0);
        let mut out = buffer(64, 0.0);

        mixer.mix(&[MixInput::new(id, &short)], &mut out).unwrap();
        let left = out.channel(0).unwrap();
        assert!(left[..16].iter().all(|s| (s - 1.0).abs() < 1e-6));
        assert!(left[16..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn tracks_can_be_added_and_removed() {
        let mut mixer = mixer();
        let (a, _fa) = mixer.add_track().unwrap();
        let (b, _fb) = mixer.add_track().unwrap();
        assert_eq!(mixer.track_count(), 2);

        assert!(mixer.remove_track(a));
        assert!(!mixer.remove_track(a));
        assert_eq!(mixer.track_count(), 1);
        assert_ne!(a, b);
    }

    #[test]
    fn a_mixer_needs_room_for_a_track() {
        assert!(Mixer::new(RATE, ChannelLayout::Stereo, 0).is_err());
        let mixer = mixer();
        assert_eq!(mixer.rate(), RATE);
        assert_eq!(mixer.layout(), ChannelLayout::Stereo);
    }
}
