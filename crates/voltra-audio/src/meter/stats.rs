//! One pass over a channel: its peak and its energy.
//!
//! Runs on the audio thread for every channel of every metered block, so it is
//! written to vectorise: eight independent lanes, no early exit, no indexing.

/// `(max |x|, Σ x²)` over `samples`.
///
/// NaN never reaches the peak — a comparison with NaN is false, so the lane
/// keeps its value — but it does reach the energy, as does an infinity. A
/// non-finite energy therefore means a non-finite block, which is how the
/// mixer detects one (plan 012's `x - x` test did the same job in a pass of its
/// own).
pub(crate) fn peak_and_energy(samples: &[f32]) -> (f32, f32) {
    const LANES: usize = 8;
    let mut peaks = [0.0f32; LANES];
    let mut energies = [0.0f32; LANES];
    let chunks = samples.chunks_exact(LANES);
    let rest = chunks.remainder();
    for chunk in chunks {
        for ((peak, energy), &sample) in peaks.iter_mut().zip(energies.iter_mut()).zip(chunk) {
            let magnitude = sample.abs();
            // Not `f32::max`: its NaN rule keeps it from lowering to a vector
            // max. This form does, and ignores NaN by construction.
            *peak = if magnitude > *peak { magnitude } else { *peak };
            *energy += sample * sample;
        }
    }
    let mut peak = peaks.iter().fold(0.0f32, |a, &b| if b > a { b } else { a });
    let mut energy: f32 = energies.iter().sum();
    for &sample in rest {
        let magnitude = sample.abs();
        peak = if magnitude > peak { magnitude } else { peak };
        energy += sample * sample;
    }
    (peak, energy)
}

#[cfg(test)]
// Exact values are the point of these tests.
#[allow(clippy::float_cmp)]
mod tests {
    use super::peak_and_energy;

    #[test]
    fn peak_and_energy_of_known_values() {
        assert_eq!(peak_and_energy(&[]), (0.0, 0.0));
        assert_eq!(peak_and_energy(&[0.5, -0.75, 0.25]), (0.75, 0.875));
        // Lanes and remainder both count.
        let samples: Vec<f32> = (0..19u8)
            .map(|i| f32::from(i) * if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let (peak, energy) = peak_and_energy(&samples);
        assert_eq!(peak, 18.0);
        assert_eq!(energy, (0..19u16).map(|i| f32::from(i * i)).sum::<f32>());
    }

    #[test]
    fn nan_spares_the_peak_but_poisons_the_energy() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for position in [0, 9, 17] {
                let mut samples = vec![0.25f32; 18];
                samples[position] = bad;
                let (peak, energy) = peak_and_energy(&samples);
                assert!(!energy.is_finite(), "{bad} at {position}");
                if bad.is_nan() {
                    assert_eq!(peak, 0.25);
                }
            }
        }
    }
}
