//! The per-sample loops. Everything here runs on the audio thread.
//!
//! Monomorphic, slice-to-slice, `zip` instead of indexing, so the compiler can
//! drop bounds checks and vectorise (CLAUDE.md §4.4). No `mul_add`: without a
//! target FMA feature it lowers to a libm call per sample.

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
    use super::{accumulate, accumulate_ramp};

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
