//! FFT convolution by overlap-add.
//!
//! Needed for room impulse responses, where direct convolution is hopeless: a
//! half-second tail at 48 kHz is 24 000 taps, and multiplying that by every
//! sample of a ten-second file is 10¹⁰ operations. Transforming blocks instead
//! turns it into a few thousand transforms.

use crate::fft::{RealFftPlan, hann_window};
use realfft::RealFftPlanner;
use rustfft::num_complex::Complex64;

/// Convolve `signal` with `impulse`, returning `signal.len()` samples.
///
/// Truncated to the input length rather than the full `n + m − 1`: for an
/// impulse response that means the tail running past the end of the recording
/// is discarded, which is what applying a room to a finite recording should do.
pub fn convolve(signal: &[f32], impulse: &[f32]) -> Vec<f32> {
    if signal.is_empty() || impulse.is_empty() {
        return vec![0.0; signal.len()];
    }

    // A block with at least as much room for the tail as the impulse needs;
    // four times the impulse keeps the number of transforms sensible.
    let fft_size = (impulse.len() * 4).max(1024).next_power_of_two();
    let block_size = fft_size - impulse.len() + 1;
    let bins = fft_size / 2 + 1;

    let mut forward = RealFftPlan::new(fft_size);
    let inverse = RealFftPlanner::<f64>::new().plan_fft_inverse(fft_size);
    let mut inverse_scratch = inverse.make_scratch_vec();

    // Transform of the impulse, reused for every block.
    let mut padded = vec![0.0f64; fft_size];
    for (p, h) in padded.iter_mut().zip(impulse) {
        *p = f64::from(*h);
    }
    let mut kernel = vec![Complex64::default(); bins];
    forward.forward(&padded, &mut kernel);

    let mut out = vec![0.0f32; signal.len()];
    let mut spectrum = vec![Complex64::default(); bins];
    let mut block = vec![0.0f64; fft_size];
    let scale = 1.0 / fft_size as f64;

    for start in (0..signal.len()).step_by(block_size) {
        padded.fill(0.0);
        let end = (start + block_size).min(signal.len());
        for (p, s) in padded.iter_mut().zip(&signal[start..end]) {
            *p = f64::from(*s);
        }

        forward.forward(&padded, &mut spectrum);
        for (x, k) in spectrum.iter_mut().zip(&kernel) {
            *x *= *k;
        }
        inverse
            .process_with_scratch(&mut spectrum, &mut block, &mut inverse_scratch)
            .expect("buffers are the plan's own sizes");

        // Overlap-add, discarding anything past the end of the signal.
        for (i, value) in block.iter().enumerate() {
            let Some(at) = out.get_mut(start + i) else {
                break;
            };
            *at += (value * scale) as f32;
        }
    }

    out
}

/// A short exponentially-decaying noise burst: a crude but serviceable room.
///
/// Used by the tests and the evaluation corpus, where what is wanted is
/// something with a plausible decay rather than a real measured room.
pub fn synthetic_impulse(sample_rate: u32, decay_sec: f64, seed: u32) -> Vec<f32> {
    let n = (decay_sec * f64::from(sample_rate)) as usize;
    let mut state = seed | 1;
    let fade = hann_window(2 * n.max(1));

    (0..n)
        .map(|i| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = f64::from(state >> 8) / f64::from(1u32 << 23) - 1.0;
            // −60 dB over the decay time, with a Hann taper on the tail so the
            // impulse ends rather than being cut off.
            let envelope = 10f64.powf(-3.0 * i as f64 / n as f64);
            (noise * envelope * fade[n + i]) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The definition: slow, obviously right, and the only honest reference.
    fn direct(signal: &[f32], impulse: &[f32]) -> Vec<f32> {
        (0..signal.len())
            .map(|n| {
                impulse
                    .iter()
                    .enumerate()
                    .filter_map(|(k, h)| {
                        n.checked_sub(k)
                            .map(|at| f64::from(*h) * f64::from(signal[at]))
                    })
                    .sum::<f64>() as f32
            })
            .collect()
    }

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| ((i as f32) * 0.37).sin() * 0.5).collect()
    }

    #[test]
    fn it_agrees_with_a_direct_convolution() {
        for impulse_len in [1usize, 7, 300, 2_000] {
            let signal = ramp(5_000);
            let impulse = ramp(impulse_len);
            let got = convolve(&signal, &impulse);
            let want = direct(&signal, &impulse);
            let worst = got
                .iter()
                .zip(&want)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "impulse of {impulse_len}: worst {worst}");
        }
    }

    #[test]
    fn it_spans_more_than_one_block() {
        // A short impulse means a 1024-point transform and a ~1024-sample
        // block, so 5000 samples exercises the overlap-add rather than one
        // block that happens to cover everything.
        let signal = ramp(5_000);
        let impulse = ramp(9);
        assert_eq!(convolve(&signal, &impulse).len(), 5_000);
        let worst = convolve(&signal, &impulse)
            .iter()
            .zip(direct(&signal, &impulse))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "seam at a block boundary: {worst}");
    }

    #[test]
    fn a_unit_impulse_leaves_the_signal_alone() {
        let signal = ramp(3_000);
        let out = convolve(&signal, &[1.0]);
        let worst = out
            .iter()
            .zip(&signal)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{worst}");
    }

    #[test]
    fn a_delayed_impulse_delays_the_signal() {
        let signal = ramp(2_000);
        let mut impulse = vec![0.0f32; 65];
        impulse[64] = 1.0;
        let out = convolve(&signal, &impulse);
        for i in 64..2_000 {
            assert!((out[i] - signal[i - 64]).abs() < 1e-6, "at {i}");
        }
        assert!(out[..64].iter().all(|s| s.abs() < 1e-6), "before the delay");
    }

    #[test]
    fn the_output_is_the_length_of_the_input() {
        assert_eq!(convolve(&ramp(100), &ramp(4_000)).len(), 100);
        assert_eq!(convolve(&ramp(4_000), &ramp(100)).len(), 4_000);
    }

    #[test]
    fn nothing_convolved_with_anything_is_nothing() {
        assert!(convolve(&[], &ramp(10)).is_empty());
        assert_eq!(convolve(&ramp(10), &[]), vec![0.0; 10]);
    }

    #[test]
    fn a_synthetic_room_decays() {
        let impulse = synthetic_impulse(48_000, 0.4, 7);
        assert_eq!(impulse.len(), 19_200);
        let energy = |x: &[f32]| x.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>();
        let head = energy(&impulse[..4_800]);
        let tail = energy(&impulse[14_400..]);
        assert!(head > 20.0 * tail, "head {head}, tail {tail}");
        assert!(
            impulse.last().unwrap().abs() < 1e-4,
            "it should end quietly"
        );
    }

    #[test]
    fn a_synthetic_room_is_reproducible_and_seed_dependent() {
        assert_eq!(
            synthetic_impulse(48_000, 0.1, 7),
            synthetic_impulse(48_000, 0.1, 7)
        );
        assert_ne!(
            synthetic_impulse(48_000, 0.1, 7),
            synthetic_impulse(48_000, 0.1, 8)
        );
    }
}
