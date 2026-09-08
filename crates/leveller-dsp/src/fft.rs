//! Fourier transforms, sized for what the pipeline actually asks for.
//!
//! Two entry points, because there are two kinds of caller. Spectral
//! measurement chooses its own frame size and always wants a power of two, and
//! gets [`power_spectrum`]. A trained model does not get to choose: DeepFilterNet3
//! was trained on a 960-point transform (20 ms at 48 kHz), and 960 = 2⁶·3·5, so
//! [`FftPlan`] handles any size at all.
//!
//! Both are backed by `rustfft`, whose plans hold their twiddle tables and
//! scratch space, so a plan reused across every frame of a file allocates
//! nothing per frame. Plans are not `Sync`-friendly to share mutably, so each
//! worker builds its own.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex64;
use rustfft::{Fft, FftDirection, FftPlanner};

/// Periodic Hann window, the right variant for overlapped spectral averaging.
///
/// Periodic rather than symmetric: with 50% overlap the periodic window sums to
/// a constant, and the symmetric one does not.
pub fn hann_window(size: usize) -> Vec<f64> {
    (0..size)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / size as f64).cos())
        .collect()
}

/// A reusable complex FFT of one fixed size.
///
/// Cloning is cheap and shares the underlying twiddle tables; the scratch
/// buffer is per-clone, which is what makes a clone safe to hand to another
/// thread.
#[derive(Clone)]
pub struct FftPlan {
    size: usize,
    forward: Arc<dyn Fft<f64>>,
    inverse: Arc<dyn Fft<f64>>,
    scratch: Vec<Complex64>,
}

impl FftPlan {
    /// Plan a transform of `size` points. Any size is accepted; sizes with
    /// large prime factors fall back to Bluestein's algorithm rather than
    /// failing.
    ///
    /// # Panics
    /// If `size` is zero.
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "FFT size must be a positive integer");
        let mut planner = FftPlanner::<f64>::new();
        let forward = planner.plan_fft(size, FftDirection::Forward);
        let inverse = planner.plan_fft(size, FftDirection::Inverse);
        let scratch_len = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        Self {
            size,
            forward,
            inverse,
            scratch: vec![Complex64::default(); scratch_len],
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// In-place forward transform.
    ///
    /// # Panics
    /// If `data` is not exactly [`size`](Self::size) points long.
    pub fn forward(&mut self, data: &mut [Complex64]) {
        assert_eq!(data.len(), self.size, "plan is for {} points", self.size);
        self.forward.process_with_scratch(data, &mut self.scratch);
    }

    /// In-place inverse transform, normalised by 1/size.
    ///
    /// rustfft's inverse is unnormalised — the scaling here is what makes
    /// `inverse(forward(x)) == x`, which is the contract every caller assumes.
    ///
    /// # Panics
    /// If `data` is not exactly [`size`](Self::size) points long.
    pub fn inverse(&mut self, data: &mut [Complex64]) {
        assert_eq!(data.len(), self.size, "plan is for {} points", self.size);
        self.inverse.process_with_scratch(data, &mut self.scratch);
        let scale = 1.0 / self.size as f64;
        for v in data {
            *v *= scale;
        }
    }
}

/// A real-to-complex transform of one fixed size, for the analysis paths that
/// only ever see real audio.
///
/// Half the work of running real frames through a complex transform with a zero
/// imaginary part, which is what the spectral stages do frame after frame.
#[derive(Clone)]
pub struct RealFftPlan {
    size: usize,
    fft: Arc<dyn RealToComplex<f64>>,
    input: Vec<f64>,
    scratch: Vec<Complex64>,
}

impl RealFftPlan {
    /// # Panics
    /// If `size` is not even — a real transform has no half-spectrum otherwise.
    pub fn new(size: usize) -> Self {
        assert!(size >= 2 && size % 2 == 0, "real FFT size must be even");
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
        let scratch = fft.make_scratch_vec();
        Self {
            size,
            fft,
            input: vec![0.0; size],
            scratch,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Number of bins a spectrum from this plan has: `size / 2 + 1`.
    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// Transform one real frame into `spectrum`, which must hold
    /// [`bins`](Self::bins) values.
    ///
    /// # Panics
    /// If `frame` or `spectrum` is the wrong length.
    pub fn forward(&mut self, frame: &[f64], spectrum: &mut [Complex64]) {
        assert_eq!(frame.len(), self.size, "plan is for {} points", self.size);
        assert_eq!(spectrum.len(), self.bins(), "spectrum is {} bins", self.bins());
        self.input.copy_from_slice(frame);
        self.fft
            .process_with_scratch(&mut self.input, spectrum, &mut self.scratch)
            .expect("lengths checked above");
    }
}

/// Power spectrum of one real windowed frame: |X[k]|² for k = 0..=n/2,
/// normalised by the window's energy so overlapping frames average cleanly.
///
/// `frame` is multiplied by `window` on the way in; it must be at least as long
/// as the window.
///
/// This is the convenience form, planning a transform per call. Anything
/// running frame after frame should hold a [`RealFftPlan`] and call
/// [`power_spectrum_with`] instead.
pub fn power_spectrum(frame: &[f32], window: &[f64]) -> Vec<f64> {
    let mut plan = RealFftPlan::new(window.len());
    let mut out = vec![0.0; plan.bins()];
    power_spectrum_with(&mut plan, frame, window, &mut out);
    out
}

/// [`power_spectrum`] against a plan the caller keeps, writing into `out`.
///
/// # Panics
/// If `frame` is shorter than `window`, or `out` is not `window.len() / 2 + 1`.
pub fn power_spectrum_with(plan: &mut RealFftPlan, frame: &[f32], window: &[f64], out: &mut [f64]) {
    let n = window.len();
    assert_eq!(n, plan.size(), "window does not match the plan");
    assert!(frame.len() >= n, "frame is shorter than the window");

    let mut windowed = vec![0.0f64; n];
    let mut window_energy = 0.0;
    for i in 0..n {
        windowed[i] = f64::from(frame[i]) * window[i];
        window_energy += window[i] * window[i];
    }

    let mut spectrum = vec![Complex64::default(); plan.bins()];
    plan.forward(&windowed, &mut spectrum);

    // A window of all zeros would divide by zero; treat it as unity rather than
    // handing back a spectrum of NaN.
    let scale = 1.0 / if window_energy == 0.0 { 1.0 } else { window_energy };
    for (o, x) in out.iter_mut().zip(spectrum.iter()) {
        *o = x.norm_sqr() * scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// Direct DFT, the definition — slow, obviously correct, and the only
    /// honest reference for a transform.
    fn dft(input: &[Complex64]) -> Vec<Complex64> {
        let n = input.len();
        (0..n)
            .map(|k| {
                (0..n).fold(Complex64::default(), |acc, j| {
                    let angle = -TAU * (k * j) as f64 / n as f64;
                    acc + input[j] * Complex64::new(angle.cos(), angle.sin())
                })
            })
            .collect()
    }

    fn ramp(n: usize) -> Vec<Complex64> {
        (0..n)
            .map(|i| {
                let t = i as f64;
                Complex64::new((t * 0.37).sin() + t * 0.01, (t * 0.11).cos())
            })
            .collect()
    }

    #[test]
    fn forward_matches_a_direct_dft() {
        // 960 is DeepFilterNet3's frame; 240 and 96 are its relatives; 64 and
        // 1024 are the power-of-two sizes the analysis stages use.
        for &n in &[8usize, 64, 96, 240, 960, 1024] {
            let input = ramp(n);
            let expected = dft(&input);
            let mut got = input.clone();
            FftPlan::new(n).forward(&mut got);
            for (i, (a, b)) in got.iter().zip(expected.iter()).enumerate() {
                assert!(
                    (a - b).norm() < 1e-8 * (n as f64),
                    "size {n} bin {i}: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn inverse_undoes_forward() {
        for &n in &[8usize, 96, 960] {
            let input = ramp(n);
            let mut data = input.clone();
            let mut plan = FftPlan::new(n);
            plan.forward(&mut data);
            plan.inverse(&mut data);
            for (a, b) in data.iter().zip(input.iter()) {
                assert!((a - b).norm() < 1e-10, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn a_hann_window_is_periodic_and_sums_flat_at_half_overlap() {
        let w = hann_window(8);
        assert!(w[0].abs() < 1e-12, "a periodic Hann starts at zero");
        // Symmetric would put a 1.0 at index 4 of 8 and mirror around it; the
        // periodic one peaks there too but never repeats the endpoint.
        assert!((w[4] - 1.0).abs() < 1e-12);
        for i in 0..4 {
            assert!((w[i] + w[i + 4] - 1.0).abs() < 1e-12, "half-overlap sum at {i}");
        }
    }

    #[test]
    fn a_sine_puts_its_power_in_one_bin() {
        let n = 1024;
        let bin = 64;
        let window = hann_window(n);
        let frame: Vec<f32> = (0..n)
            .map(|i| (TAU * bin as f64 * i as f64 / n as f64).sin() as f32)
            .collect();
        let spectrum = power_spectrum(&frame, &window);

        let peak = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, _)| k)
            .unwrap();
        assert_eq!(peak, bin);
        // Everything more than two bins away is leakage, and a Hann window
        // keeps that far below the peak.
        let leakage = spectrum
            .iter()
            .enumerate()
            .filter(|(k, _)| k.abs_diff(bin) > 2)
            .fold(0.0f64, |m, (_, v)| m.max(*v));
        assert!(leakage < spectrum[peak] * 1e-4, "leakage {leakage}");
    }

    #[test]
    fn a_real_spectrum_matches_the_complex_one() {
        let n = 256;
        let window = hann_window(n);
        let frame: Vec<f32> = (0..n).map(|i| ((i as f64) * 0.21).sin() as f32).collect();

        let got = power_spectrum(&frame, &window);

        let mut complex: Vec<Complex64> = (0..n)
            .map(|i| Complex64::new(f64::from(frame[i]) * window[i], 0.0))
            .collect();
        FftPlan::new(n).forward(&mut complex);
        let energy: f64 = window.iter().map(|w| w * w).sum();

        for k in 0..=n / 2 {
            let expected = complex[k].norm_sqr() / energy;
            assert!((got[k] - expected).abs() < 1e-12 * expected.max(1.0), "bin {k}");
        }
    }
}
