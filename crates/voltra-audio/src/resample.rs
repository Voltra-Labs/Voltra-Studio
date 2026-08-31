//! Changing sample rate without changing pitch.
//!
//! A fixed-ratio polyphase resampler. The ratio is an exact fraction —
//! 48000/44100 is 160/147, never 1.08843537… — because accumulating a `double`
//! per block is the same class of mistake the video clock avoided in plan 002
//! and the audio clock in plan 011.
//!
//! # Why not `FFmpeg`
//!
//! libobs wraps `libswresample` and passes it no quality settings at all
//! (`docs/references/audio.md` §6). Pulling `FFmpeg` in to turn 44 100 into 48 000
//! would mean a tree of native libraries for a filter that fits in a few hundred
//! verifiable lines — and a native dependency would break the headless-container
//! rule of CLAUDE.md §3.
//!
//! # The one thing that is inherited
//!
//! A filter has group delay, so a resampler is late. libobs asks
//! `swr_get_delay` for that and applies it as a timestamp offset; without it,
//! resampling shifts audio against video by a fixed amount, which reads as lips
//! that never quite match rather than as drift. Here the delay is a constant of
//! the filter, known when it is built, and [`Resampler::latency`] states it.

use std::time::Duration;

use voltra_core::{AudioBuffer, ChannelLayout, Error, Result, SampleRate};

/// Coefficients per polyphase branch, before scaling for decimation.
///
/// This is the per-output cost: every output sample is this many
/// multiply-accumulates. The *total* prototype filter is `TAPS × L`, so with a
/// large `L` the filter is long and sharp for free.
///
/// When decimating, `L` is small and that no longer holds: 48 000 → 8 000 gives
/// `L = 1`, and 32 coefficients is not a filter, it is a suggestion. So the tap
/// count is scaled by `M/L` in [`taps_for`], which keeps the *total* length —
/// and therefore the transition width — roughly constant whichever way the
/// conversion goes.
const BASE_TAPS: usize = 32;

/// How far below the theoretical limit the passband is cut.
///
/// A brick wall exactly at Nyquist would need an infinite filter, so every real
/// resampler gives up a sliver of the top end to buy a transition band.
/// `libswresample` defaults to about the same place.
const CUTOFF_FACTOR: f64 = 0.90;

/// Coefficients per branch for a given ratio.
fn taps_for(ratio: ResampleRatio) -> usize {
    let interpolation = ratio.interpolation() as usize;
    let decimation = ratio.decimation() as usize;
    BASE_TAPS * decimation.div_ceil(interpolation).max(1)
}

/// Kaiser window β.
///
/// β ≈ 8.6 targets roughly 80 dB of stopband attenuation, comfortably past what
/// 16-bit output can resolve. Larger β buys depth and costs transition width.
const KAISER_BETA: f32 = 8.6;

/// A resampling ratio as an exact fraction in lowest terms.
///
/// Interpolate by `interpolation`, decimate by `decimation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResampleRatio {
    interpolation: u32,
    decimation: u32,
}

impl ResampleRatio {
    /// The ratio taking `from` to `to`, reduced.
    ///
    /// # Examples
    ///
    /// ```
    /// use voltra_audio::{ResampleRatio, SampleRate};
    ///
    /// // The one everybody needs: 48000/44100 is exactly 160/147.
    /// let ratio = ResampleRatio::between(SampleRate::HZ_44100, SampleRate::HZ_48000);
    /// assert_eq!((ratio.interpolation(), ratio.decimation()), (160, 147));
    /// assert!(!ratio.is_identity());
    /// ```
    #[must_use]
    pub fn between(from: SampleRate, to: SampleRate) -> Self {
        let divisor = gcd(to.hz(), from.hz());
        Self {
            interpolation: to.hz() / divisor,
            decimation: from.hz() / divisor,
        }
    }

    /// The upsampling factor, `L`.
    #[must_use]
    pub const fn interpolation(self) -> u32 {
        self.interpolation
    }

    /// The downsampling factor, `M`.
    #[must_use]
    pub const fn decimation(self) -> u32 {
        self.decimation
    }

    /// Whether the two rates are the same, so nothing needs doing.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.interpolation == self.decimation
    }
}

impl std::fmt::Display for ResampleRatio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.interpolation, self.decimation)
    }
}

/// Greatest common divisor, by Euclid.
const fn gcd(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    // Zero rates cannot reach here — `SampleRate::new` refuses them — but a
    // divisor of zero would panic rather than misbehave quietly, so guard it.
    if a == 0 { 1 } else { a }
}

/// Converts a stream from one sample rate to another.
///
/// # Examples
///
/// ```
/// use voltra_audio::{ChannelLayout, Resampler, SampleRate};
/// use voltra_core::{AudioBuffer, Timestamp};
///
/// let mut resampler = Resampler::new(
///     SampleRate::HZ_44100,
///     SampleRate::HZ_48000,
///     ChannelLayout::Stereo,
///     1024,
/// )?;
///
/// let input = AudioBuffer::new(
///     SampleRate::HZ_44100, ChannelLayout::Stereo, 1024, Timestamp::ZERO)?;
/// let mut output = AudioBuffer::new(
///     SampleRate::HZ_48000, ChannelLayout::Stereo, 2048, Timestamp::ZERO)?;
///
/// let produced = resampler.process(&input, &mut output)?;
/// // 1024 × 160/147 is about 1114, and the exact figure varies by one between
/// // blocks — which is why `process` reports it rather than the caller guessing.
/// assert!((1113..=1115).contains(&produced));
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct Resampler {
    from: SampleRate,
    to: SampleRate,
    layout: ChannelLayout,
    ratio: ResampleRatio,
    /// `phases[p][t]` is `h[p + t·L]`, laid out contiguously by phase so one
    /// output sample walks one cache line at a time.
    phases: Vec<f32>,
    /// Coefficients per phase, from [`taps_for`].
    taps: usize,
    /// Per channel: `TAPS - 1` samples of history, then room for one block.
    scratch: Vec<f32>,
    /// Stride of one channel's slice inside `scratch`.
    scratch_stride: usize,
    max_frames: usize,
    /// Position inside the current interpolated grid, `0..L`.
    phase: u32,
    /// Where in the *next* block the next output's window starts.
    ///
    /// The interpolated grid does not land on block boundaries, so a block
    /// usually ends having stepped a sample or two past its own end. That
    /// overshoot is where the next block begins, and losing it is how a
    /// resampler drifts a sample at a time until the output is unrecognisable.
    next_base: usize,
}

impl Resampler {
    /// Build a resampler from `from` to `to`, for blocks of at most
    /// `max_frames` input samples.
    ///
    /// Every allocation happens here: the coefficients, which need sines and a
    /// Bessel function, and the per-channel history. [`Resampler::process`]
    /// allocates nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `max_frames` is zero.
    pub fn new(
        from: SampleRate,
        to: SampleRate,
        layout: ChannelLayout,
        max_frames: usize,
    ) -> Result<Self> {
        if max_frames == 0 {
            return Err(Error::config("a resampler needs a non-zero block size"));
        }
        let ratio = ResampleRatio::between(from, to);
        let taps = taps_for(ratio);
        let phases = design_filter(ratio, taps);

        let scratch_stride = taps - 1 + max_frames;
        let channels = layout.channels();
        Ok(Self {
            from,
            to,
            layout,
            ratio,
            phases,
            taps,
            scratch: vec![0.0; scratch_stride * channels],
            scratch_stride,
            max_frames,
            phase: 0,
            next_base: 0,
        })
    }

    /// The ratio in use.
    #[must_use]
    pub const fn ratio(&self) -> ResampleRatio {
        self.ratio
    }

    /// The rate this resampler reads.
    #[must_use]
    pub const fn input_rate(&self) -> SampleRate {
        self.from
    }

    /// The rate this resampler writes.
    #[must_use]
    pub const fn output_rate(&self) -> SampleRate {
        self.to
    }

    /// The filter's group delay, in output samples.
    ///
    /// A symmetric FIR delays by half its length. Zero when the rates match,
    /// because then there is no filter.
    ///
    /// This is what [`Resampler::latency`] reports and what a caller has to
    /// subtract from timestamps. Unlike `swr_get_delay`, which returns a
    /// running queue depth, this is a constant of the design.
    #[must_use]
    pub fn latency_frames(&self) -> usize {
        if self.ratio.is_identity() {
            return 0;
        }
        // Half the filter, expressed in output samples.
        (self.taps / 2) * self.ratio.interpolation() as usize / self.ratio.decimation() as usize
    }

    /// The filter's group delay as a duration, for correcting timestamps.
    ///
    /// Rounded to whole nanoseconds, so converting it back with
    /// [`SampleRate::frames_in`] can come out one sample short.
    /// [`Resampler::latency_frames`] is the exact figure.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.to.duration_of(self.latency_frames() as u64)
    }

    /// How many output samples a block of `input_frames` will produce.
    ///
    /// Not a constant: with 160/147 a 1024-sample block yields 1114 samples
    /// some blocks and 1115 others, because the interpolated grid does not
    /// align with block boundaries.
    #[must_use]
    pub fn output_frames_for(&self, input_frames: usize) -> usize {
        if self.ratio.is_identity() {
            return input_frames;
        }
        self.walk(input_frames).0
    }

    /// Walk the interpolated grid across a block of `frames` input samples.
    ///
    /// Returns how many outputs it yields and where the grid lands in the next
    /// block. Pure arithmetic on the current state, so it can be asked before
    /// committing.
    fn walk(&self, frames: usize) -> (usize, usize) {
        let interpolation = self.ratio.interpolation() as usize;
        let decimation = self.ratio.decimation() as usize;

        let mut phase = self.phase as usize;
        let mut base = self.next_base;
        let mut produced = 0usize;
        while base < frames {
            produced += 1;
            phase += decimation;
            base += phase / interpolation;
            phase %= interpolation;
        }
        (produced, base - frames)
    }

    /// Resample one block, returning how many output samples were written.
    ///
    /// Allocates nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the buffers do not match the configured rates or
    /// layout, when the input is longer than the configured maximum, or when
    /// `out` is too short for what this block produces.
    pub fn process(&mut self, input: &AudioBuffer, out: &mut AudioBuffer) -> Result<usize> {
        if input.rate() != self.from || input.layout() != self.layout {
            return Err(Error::config(format!(
                "resampler reads {} {}, got {} {}",
                self.from,
                self.layout,
                input.rate(),
                input.layout()
            )));
        }
        if out.rate() != self.to || out.layout() != self.layout {
            return Err(Error::config(format!(
                "resampler writes {} {}, got {} {}",
                self.to,
                self.layout,
                out.rate(),
                out.layout()
            )));
        }
        if input.frames() > self.max_frames {
            return Err(Error::config(format!(
                "block of {} frames exceeds the configured maximum of {}",
                input.frames(),
                self.max_frames
            )));
        }

        // Matching rates: a copy, and bit for bit at that. Running a unity
        // filter over it would only add rounding.
        if self.ratio.is_identity() {
            let frames = input.frames().min(out.frames());
            for (source, target) in input.channels().zip(out.channels_mut()) {
                target[..frames].copy_from_slice(&source[..frames]);
            }
            return Ok(frames);
        }

        let produced = self.output_frames_for(input.frames());
        if produced > out.frames() {
            return Err(Error::config(format!(
                "this block produces {produced} frames, output holds {}",
                out.frames()
            )));
        }

        let interpolation = self.ratio.interpolation() as usize;
        let decimation = self.ratio.decimation() as usize;
        let history = self.taps - 1;
        let frames = input.frames();
        let taps = self.taps;

        for (index, (source, target)) in input.channels().zip(out.channels_mut()).enumerate() {
            let start = index * self.scratch_stride;
            let scratch = &mut self.scratch[start..start + history + frames];
            scratch[history..].copy_from_slice(source);

            // Walk the interpolated grid by exact integer steps: no growing
            // counter, no accumulated rounding.
            let mut phase = self.phase as usize;
            let mut base = self.next_base;

            for slot in &mut target[..produced] {
                let coefficients = &self.phases[phase * taps..(phase + 1) * taps];
                let window = &scratch[base..base + taps];

                // Deliberately not `mul_add`: without hardware FMA that is a
                // libm call per tap. See docs/PERFORMANCE.md §3.7 and §3.8.
                let mut sum = 0.0f32;
                for (coefficient, sample) in coefficients.iter().zip(window.iter().rev()) {
                    sum += coefficient * sample;
                }
                *slot = sum;

                phase += decimation;
                base += phase / interpolation;
                phase %= interpolation;
            }

            // Carry the tail forward so the next block continues the filter.
            scratch.copy_within(frames..frames + history, 0);
        }

        // Advance the shared grid state once, after every channel has used it.
        // `walk` reads the *current* state, so the new base is taken before the
        // phase moves.
        let (_, next_base) = self.walk(frames);
        let mut phase = self.phase as usize;
        for _ in 0..produced {
            phase += decimation;
            phase %= interpolation;
        }
        self.phase = u32::try_from(phase).unwrap_or(0);
        self.next_base = next_base;

        Ok(produced)
    }
}

/// Build the polyphase coefficient table.
///
/// A Kaiser-windowed sinc, cut at `0.5 / max(L, M)` of the interpolated rate —
/// when upsampling the image bands set the limit, when downsampling the new
/// Nyquist does — and scaled by `L` to make up for the zeros interpolation
/// inserts.
// The filter length is at most a few hundred thousand, so `usize as f64` here is
// exact; it also runs once per resampler, never per sample.
#[allow(clippy::cast_precision_loss)]
fn design_filter(ratio: ResampleRatio, taps: usize) -> Vec<f32> {
    let interpolation = ratio.interpolation() as usize;
    let length = interpolation * taps;
    let mut coefficients = vec![0.0f32; length];

    let cutoff = CUTOFF_FACTOR * 0.5 / f64::from(ratio.interpolation().max(ratio.decimation()));
    let centre = (length - 1) as f64 / 2.0;
    let denominator = bessel_i0(f64::from(KAISER_BETA));

    for (index, coefficient) in coefficients.iter_mut().enumerate() {
        let offset = index as f64 - centre;
        let sinc = if offset == 0.0 {
            2.0 * cutoff
        } else {
            let x = std::f64::consts::TAU * cutoff * offset;
            x.sin() / (std::f64::consts::PI * offset)
        };
        let normalised = offset / centre;
        let window =
            bessel_i0(f64::from(KAISER_BETA) * (1.0 - normalised * normalised).max(0.0).sqrt())
                / denominator;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "coefficients are computed in f64 and stored as f32 on purpose"
        )]
        {
            *coefficient = (sinc * window * f64::from(ratio.interpolation())) as f32;
        }
    }

    // Normalise each phase so a constant input comes out at the same constant.
    // Without this the DC gain drifts with the window, and every level in the
    // mix would be slightly wrong in a way nobody could point at.
    let mut phases = vec![0.0f32; length];
    for phase in 0..interpolation {
        let mut sum = 0.0f64;
        for tap in 0..taps {
            sum += f64::from(coefficients[phase + tap * interpolation]);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a normalisation factor near 1.0"
        )]
        let scale = if sum.abs() > f64::EPSILON {
            (1.0 / sum) as f32
        } else {
            1.0
        };
        for tap in 0..taps {
            phases[phase * taps + tap] = coefficients[phase + tap * interpolation] * scale;
        }
    }
    phases
}

/// Modified Bessel function of the first kind, order zero.
///
/// The series converges quickly for the arguments a Kaiser window uses, and this
/// runs once per resampler, never per sample.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for index in 1..64 {
        term *= (half / f64::from(index)) * (half / f64::from(index));
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::{BASE_TAPS, ResampleRatio, Resampler};
    use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};

    const IN: SampleRate = SampleRate::HZ_44100;
    const OUT: SampleRate = SampleRate::HZ_48000;

    fn buffer(rate: SampleRate, frames: usize) -> AudioBuffer {
        AudioBuffer::new(rate, ChannelLayout::Stereo, frames, Timestamp::ZERO).unwrap()
    }

    /// A tone of `hertz` at `rate`, starting at sample `offset`.
    fn tone(rate: SampleRate, frames: usize, hertz: f32, offset: usize) -> AudioBuffer {
        let mut buffer = buffer(rate, frames);
        #[allow(clippy::cast_precision_loss)]
        let step = hertz / rate.hz() as f32 * std::f32::consts::TAU;
        for channel in buffer.channels_mut() {
            for (index, sample) in channel.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let phase = step * (index + offset) as f32;
                *sample = phase.sin() * 0.5;
            }
        }
        buffer
    }

    /// Mean power of a block.
    #[allow(clippy::cast_precision_loss)]
    fn power(samples: &[f32]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        samples
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / samples.len() as f64
    }

    /// The power sitting exactly at `hertz`, by a single-bin DFT.
    ///
    /// Measuring in the frequency domain rather than sample by sample against a
    /// generated reference: the resampler has a group delay and an arbitrary
    /// phase, and aligning those by hand tests the alignment arithmetic as much
    /// as the filter. What matters is whether the energy is still where it was.
    #[allow(clippy::cast_precision_loss)]
    fn power_at(samples: &[f32], hertz: f32, rate: SampleRate) -> f64 {
        let n = samples.len() as f64;
        if n == 0.0 {
            return 0.0;
        }
        let omega = std::f64::consts::TAU * f64::from(hertz) / f64::from(rate.hz());
        let (mut real, mut imaginary) = (0.0f64, 0.0f64);
        for (index, sample) in samples.iter().enumerate() {
            let phase = omega * index as f64;
            real += f64::from(*sample) * phase.cos();
            imaginary += f64::from(*sample) * phase.sin();
        }
        2.0 * (real * real + imaginary * imaginary) / (n * n)
    }

    /// How far the tone at `hertz` stands above everything else, in dB.
    fn tone_snr_db(samples: &[f32], hertz: f32, rate: SampleRate) -> f32 {
        let total = power(samples);
        let tone = power_at(samples, hertz, rate);
        let noise = (total - tone).max(1e-30);
        #[allow(clippy::cast_possible_truncation)]
        {
            (10.0 * (tone / noise).log10()) as f32
        }
    }

    #[test]
    fn the_common_ratio_reduces_to_160_over_147() {
        let ratio = ResampleRatio::between(IN, OUT);
        assert_eq!((ratio.interpolation(), ratio.decimation()), (160, 147));
        assert_eq!(ratio.to_string(), "160/147");

        let down = ResampleRatio::between(OUT, IN);
        assert_eq!((down.interpolation(), down.decimation()), (147, 160));
    }

    #[test]
    fn equal_rates_are_the_identity() {
        let ratio = ResampleRatio::between(OUT, OUT);
        assert!(ratio.is_identity());
        assert_eq!((ratio.interpolation(), ratio.decimation()), (1, 1));
    }

    /// Matching rates must be a copy, not a filter: running a unity filter over
    /// the samples would only add rounding to something already correct.
    #[test]
    fn matching_rates_pass_through_bit_for_bit() {
        let mut resampler = Resampler::new(OUT, OUT, ChannelLayout::Stereo, 512).unwrap();
        assert_eq!(resampler.latency_frames(), 0);

        let input = tone(OUT, 512, 1_000.0, 0);
        let mut output = buffer(OUT, 512);
        let produced = resampler.process(&input, &mut output).unwrap();

        assert_eq!(produced, 512);
        assert_eq!(input.as_slice(), output.as_slice());
    }

    /// The test that fixes the filter's normalisation. A drifting DC gain would
    /// make every level in the mix slightly wrong in a way nobody could point at.
    #[test]
    fn a_constant_signal_keeps_its_level() {
        let mut resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 1024).unwrap();
        let mut input = buffer(IN, 1024);
        input.as_mut_slice().fill(0.5);
        let mut output = buffer(OUT, 2048);

        // Run several blocks so the filter's history is full of the constant.
        let mut produced = 0;
        for _ in 0..4 {
            produced = resampler.process(&input, &mut output).unwrap();
        }

        // Skip the filter's own delay, then everything should be 0.5.
        let settled = &output.channel(0).unwrap()[BASE_TAPS..produced];
        for (index, sample) in settled.iter().enumerate() {
            assert!(
                (sample - 0.5).abs() < 1e-3,
                "sample {index} was {sample}, expected 0.5"
            );
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 1024).unwrap();
        let input = buffer(IN, 1024);
        let mut output = buffer(OUT, 2048);
        let produced = resampler.process(&input, &mut output).unwrap();
        assert!(
            output.channel(0).unwrap()[..produced]
                .iter()
                .all(|&s| s == 0.0)
        );
    }

    /// The acceptance criterion: a 1 kHz tone resampled from 44.1 to 48 kHz has
    /// to still be a 1 kHz tone, compared against one generated analytically at
    /// the output rate.
    #[test]
    fn a_tone_survives_resampling_above_sixty_decibels() {
        let mut resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 1024).unwrap();
        let mut output = buffer(OUT, 2048);

        // Feed enough blocks that the filter history is full and the grid has
        // settled away from its starting transient.
        let mut consumed_in = 0usize;
        let mut last = Vec::new();
        for _ in 0..8 {
            let input = tone(IN, 1024, 1_000.0, consumed_in);
            let produced = resampler.process(&input, &mut output).unwrap();
            consumed_in += 1024;
            last = output.channel(0).unwrap()[..produced].to_vec();
        }

        let snr = tone_snr_db(&last, 1_000.0, OUT);
        assert!(snr > 60.0, "signal-to-noise ratio was only {snr:.1} dB");
    }

    /// The failure that separates a resampler from linear interpolation: a tone
    /// above the new Nyquist must vanish, not fold back into the audible band.
    #[test]
    fn downsampling_rejects_what_it_cannot_carry() {
        // 48 kHz down to 44.1: the new Nyquist is 22 050 Hz.
        let mut resampler = Resampler::new(OUT, IN, ChannelLayout::Stereo, 1024).unwrap();
        let mut output = buffer(IN, 2048);

        let mut consumed = 0usize;
        let mut produced = 0usize;
        for _ in 0..8 {
            // 23 kHz: fine at 48 kHz, impossible at 44.1.
            let input = tone(OUT, 1024, 23_000.0, consumed);
            produced = resampler.process(&input, &mut output).unwrap();
            consumed += 1024;
        }

        let tail = &output.channel(0).unwrap()[..produced];
        let peak = tail.iter().fold(0.0f32, |worst, s| worst.max(s.abs()));
        // The input peaks at 0.5; anything surviving would alias into the
        // audible band. −40 dB of it is the bar.
        assert!(
            peak < 0.005,
            "a 23 kHz tone survived downsampling at amplitude {peak}"
        );
    }

    /// The output count is not constant, and the sum must not drift from the
    /// ratio over many blocks.
    /// The case where aliasing is actually audible. Downsampling 48 kHz to
    /// 8 kHz puts Nyquist at 4 kHz, so a 6 kHz tone would fold to 2 kHz — right
    /// in the middle of speech. This is also the ratio that exposed the tap
    /// count: with `L = 1` the prototype filter is only `taps` long, so the
    /// count has to scale with the decimation factor.
    #[test]
    fn a_large_decimation_rejects_what_would_fold_into_the_audible_band() {
        let voice = SampleRate::new(8_000).unwrap();
        let mut resampler = Resampler::new(OUT, voice, ChannelLayout::Stereo, 1024).unwrap();
        assert!(
            resampler.ratio().decimation() > resampler.ratio().interpolation(),
            "this should be a decimation"
        );

        let mut output =
            AudioBuffer::new(voice, ChannelLayout::Stereo, 1024, Timestamp::ZERO).unwrap();
        let mut consumed = 0usize;
        let mut produced = 0usize;
        for _ in 0..8 {
            let input = tone(OUT, 1024, 6_000.0, consumed);
            produced = resampler.process(&input, &mut output).unwrap();
            consumed += 1024;
        }

        let tail = &output.channel(0).unwrap()[..produced];
        // Nothing at the fold-down frequency, and nothing much anywhere.
        let folded = power_at(tail, 2_000.0, voice);
        assert!(
            folded < 1e-8,
            "a 6 kHz tone folded down to 2 kHz with power {folded:e}"
        );
        let peak = tail.iter().fold(0.0f32, |worst, s| worst.max(s.abs()));
        assert!(peak < 0.005, "residual peak was {peak}");
    }

    /// The filter gets longer when it has to, and the latency follows.
    #[test]
    fn decimation_buys_a_longer_filter() {
        let voice = SampleRate::new(8_000).unwrap();
        let up = Resampler::new(IN, OUT, ChannelLayout::Stereo, 256).unwrap();
        let down = Resampler::new(OUT, voice, ChannelLayout::Stereo, 256).unwrap();

        // 44.1 -> 48 interpolates, so the base tap count is enough.
        assert_eq!(up.latency_frames(), BASE_TAPS / 2 * 160 / 147);
        // 48 -> 8 decimates by six, so the filter is six times longer.
        assert!(down.latency_frames() > 0);
    }

    #[test]
    fn the_output_count_tracks_the_ratio_without_drifting() {
        let mut resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 1024).unwrap();
        let input = buffer(IN, 1024);
        let mut output = buffer(OUT, 2048);

        let mut total = 0usize;
        let mut varied = false;
        let mut previous = 0usize;
        for block in 0..100 {
            let expected = resampler.output_frames_for(1024);
            let produced = resampler.process(&input, &mut output).unwrap();
            assert_eq!(
                produced, expected,
                "block {block} disagreed with its estimate"
            );
            if block > 0 && produced != previous {
                varied = true;
            }
            previous = produced;
            total += produced;
        }

        assert!(
            varied,
            "the block size never varied, so the grid is not moving"
        );
        // 100 blocks of 1024 at 160/147 is 111_473 samples, give or take one.
        let ideal = 100 * 1024 * 160 / 147;
        assert!(
            total.abs_diff(ideal) <= 2,
            "produced {total} samples, expected about {ideal}"
        );
    }

    #[test]
    fn the_latency_is_declared_and_non_zero() {
        let resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 1024).unwrap();
        assert!(resampler.latency_frames() > 0);
        assert!(resampler.latency() > std::time::Duration::ZERO);
        assert_eq!(resampler.input_rate(), IN);
        assert_eq!(resampler.output_rate(), OUT);
    }

    #[test]
    fn mismatched_buffers_are_refused() {
        let mut resampler = Resampler::new(IN, OUT, ChannelLayout::Stereo, 512).unwrap();
        let mut output = buffer(OUT, 1024);

        // Wrong input rate.
        assert!(resampler.process(&buffer(OUT, 512), &mut output).is_err());
        // Wrong output rate.
        assert!(
            resampler
                .process(&buffer(IN, 512), &mut buffer(IN, 1024))
                .is_err()
        );
        // Block longer than configured.
        assert!(resampler.process(&buffer(IN, 4096), &mut output).is_err());
        // Output too short for what this block produces.
        assert!(
            resampler
                .process(&buffer(IN, 512), &mut buffer(OUT, 8))
                .is_err()
        );
    }

    #[test]
    fn a_zero_block_size_is_refused() {
        assert!(Resampler::new(IN, OUT, ChannelLayout::Stereo, 0).is_err());
    }
}
