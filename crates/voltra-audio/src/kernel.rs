//! The per-sample loops. Everything here runs on the audio thread.
//!
//! Monomorphic, slice-to-slice, `zip` instead of indexing, so the compiler can
//! drop bounds checks and vectorise (CLAUDE.md §4.4). No `mul_add`: without a
//! target FMA feature it lowers to a libm call per sample.

/// Whether every sample is finite.
///
/// `x - x` is `0.0` for any finite `x` and NaN for NaN and both infinities, and
/// NaN survives addition. Summing it in eight independent lanes, without an
/// early exit, keeps the loop branch-free so it vectorises: one pass, no
/// per-sample decision.
#[allow(
    clippy::eq_op,
    reason = "`x - x` is the test itself: zero when finite, NaN otherwise"
)]
pub(crate) fn all_finite(samples: &[f32]) -> bool {
    const LANES: usize = 8;
    let mut lanes = [0.0f32; LANES];
    let chunks = samples.chunks_exact(LANES);
    let rest = chunks.remainder();
    for chunk in chunks {
        for (lane, &sample) in lanes.iter_mut().zip(chunk) {
            *lane += sample - sample;
        }
    }
    let mut total: f32 = lanes.iter().sum();
    for &sample in rest {
        total += sample - sample;
    }
    total == 0.0
}

/// `out += input × gain`.
pub(crate) fn accumulate(out: &mut [f32], input: &[f32], gain: f32) {
    for (sum, &sample) in out.iter_mut().zip(input) {
        *sum += sample * gain;
    }
}

/// `out += input × (from + (to − from) × ramp[k])`.
///
/// `ramp` is `(k + 1) / frames`, built once by the mixer, so the last sample
/// lands on `to` and the first is already one step away from `from` — the
/// previous block ended on `from`, and repeating it would be a one-sample hold.
pub(crate) fn accumulate_ramp(out: &mut [f32], input: &[f32], from: f32, to: f32, ramp: &[f32]) {
    let delta = to - from;
    for ((sum, &sample), &position) in out.iter_mut().zip(input).zip(ramp) {
        *sum += sample * (from + delta * position);
    }
}

#[cfg(test)]
// Exact values are the point of these tests.
#[allow(clippy::float_cmp)]
mod tests {
    use super::{accumulate, accumulate_ramp, all_finite};

    #[test]
    fn finite_check_catches_nan_and_both_infinities_anywhere() {
        let clean: Vec<f32> = (0..37u16).map(|i| f32::from(i) - 18.0).collect();
        assert!(all_finite(&clean));
        assert!(all_finite(&[]));
        assert!(all_finite(&[f32::MAX, f32::MIN, -0.0]));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            // In a full lane, and in the remainder.
            for position in [0, 5, 31, 36] {
                let mut samples = clean.clone();
                samples[position] = bad;
                assert!(!all_finite(&samples), "{bad} at {position}");
            }
        }
    }

    #[test]
    fn accumulate_adds_scaled() {
        let mut out = [1.0, 2.0, 3.0];
        accumulate(&mut out, &[1.0, 1.0, 2.0], 0.5);
        assert_eq!(out, [1.5, 2.5, 4.0]);
    }

    #[test]
    fn ramp_ends_on_target() {
        let ramp = [0.25, 0.5, 0.75, 1.0];
        let mut out = [0.0; 4];
        accumulate_ramp(&mut out, &[1.0; 4], 1.0, 0.0, &ramp);
        assert_eq!(out, [0.75, 0.5, 0.25, 0.0]);
    }
}
