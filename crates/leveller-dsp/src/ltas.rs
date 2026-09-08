//! Long-term average spectrum, measured the way the EQ needs it.
//!
//! Two decisions matter more here than the Welch mechanics:
//!
//! - **Speech frames only.** Averaging over the whole file lets the noise floor
//!   in the pauses drag the spectrum down between syllables, and the curve
//!   stops describing the voice. The caller passes the speech ranges — the
//!   silence analysis already knows them — and only frames lying wholly inside
//!   one are averaged.
//!
//! - **Fractional-octave smoothing on a log grid.** Raw FFT bins are linearly
//!   spaced, so at the bottom one bin spans a third of an octave and at the top
//!   hundreds fit inside one. Ears work in octaves, and the EQ reasons on a
//!   log-spaced curve where every point averages a constant fraction of one.

use crate::fft::{RealFftPlan, hann_window, power_spectrum_with};

#[derive(Clone, Copy, Debug)]
pub struct LtasOptions {
    /// FFT size. 8192 at 48 kHz is about 5.9 Hz of raw resolution.
    pub fft_size: usize,
    pub min_freq: f64,
    pub max_freq: f64,
    /// Grid points per octave.
    pub points_per_octave: f64,
    /// Smoothing bandwidth in octaves, total and centred.
    pub smoothing_octaves: f64,
    /// Floor on the smoothing bandwidth, in Hz.
    ///
    /// Fractional-octave smoothing alone is too fine at the bottom: a third of
    /// an octave at 100 Hz is 23 Hz, narrower than the spacing between a
    /// voice's harmonics. The curve then resolves the harmonic comb rather than
    /// the spectral envelope, and an EQ fitted to it would chase peaks and
    /// troughs that are the talker's pitch, not the room. Holding the bandwidth
    /// to at least ~120 Hz down low — roughly the shape auditory filters have,
    /// constant-Hz below 500 Hz and constant-octave above — measures the
    /// envelope instead.
    pub min_bandwidth_hz: f64,
}

impl Default for LtasOptions {
    fn default() -> Self {
        Self {
            fft_size: 8192,
            min_freq: 50.0,
            max_freq: 16_000.0,
            points_per_octave: 12.0,
            smoothing_octaves: 1.0 / 3.0,
            min_bandwidth_hz: 120.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ltas {
    /// Grid frequencies, log-spaced.
    pub freqs: Vec<f64>,
    /// Smoothed level at each grid point, in dB against an arbitrary reference.
    pub db: Vec<f64>,
    /// How many frames went into the average.
    pub frames: usize,
    /// The unsmoothed mean power spectrum, in linear bins.
    ///
    /// Kept because the smoothed grid deliberately blurs the bottom octaves
    /// (see [`LtasOptions::min_bandwidth_hz`]), which makes it useless for any
    /// question that needs sub-bass separated from a voice's fundamental — the
    /// rumble decision being exactly that.
    pub psd: Vec<f64>,
    pub sample_rate: u32,
}

impl Ltas {
    /// The reference the corrective EQ pulls deviations toward: this same curve
    /// smoothed much more broadly.
    ///
    /// Correcting toward a heavily smoothed version of *itself* removes
    /// resonances and notches while leaving the voice's broad tilt — its actual
    /// character — alone. An absolute target curve would impose one announcer's
    /// timbre on every voice that came through.
    pub fn broad_target(&self, smoothing_octaves: f64) -> Vec<f64> {
        let half = smoothing_octaves / 2.0;
        (0..self.db.len())
            .map(|i| {
                // Average in power, over the grid points within the half-width.
                let mut acc = 0.0;
                let mut count = 0usize;
                for j in 0..self.db.len() {
                    if (self.freqs[j] / self.freqs[i]).log2().abs() > half {
                        continue;
                    }
                    acc += 10f64.powf(self.db[j] / 10.0);
                    count += 1;
                }
                10.0 * (acc / count.max(1) as f64 + 1e-30).log10()
            })
            .collect()
    }

    /// Mean power in [lo, hi) Hz of the raw PSD, in dB.
    ///
    /// Bin 0 is skipped throughout: DC carries any offset the recording has and
    /// none of its sound.
    pub fn band_energy_db(&self, lo: f64, hi: f64) -> f64 {
        band_energy_db(&self.psd, self.sample_rate, lo, hi)
    }
}

/// Mean power in [lo, hi) Hz of a raw PSD, in dB.
pub fn band_energy_db(psd: &[f64], sample_rate: u32, lo: f64, hi: f64) -> f64 {
    let bin_hz = f64::from(sample_rate) / ((psd.len() - 1) * 2) as f64;
    let from = ((lo / bin_hz).round() as usize).max(1);
    let to = ((hi / bin_hz).round() as usize).min(psd.len() - 1);
    if to < from {
        return f64::NEG_INFINITY;
    }
    let acc: f64 = psd[from..=to].iter().sum();
    10.0 * (acc / (to - from + 1) as f64 + 1e-30).log10()
}

/// Mean PSD over the frames lying wholly inside the given ranges, smoothed onto
/// the log grid. Frames hop by half the FFT size.
///
/// Returns `None` when the ranges hold fewer than a handful of frames: an LTAS
/// from two frames is an anecdote, and the caller should decline to EQ rather
/// than fit a curve to it.
pub fn compute(
    samples: &[f32],
    sample_rate: u32,
    ranges: &[std::ops::Range<usize>],
    options: &LtasOptions,
) -> Option<Ltas> {
    let fft_size = options.fft_size;
    let hop = fft_size / 2;
    let window = hann_window(fft_size);
    let mut plan = RealFftPlan::new(fft_size);

    let mut sum = vec![0.0f64; fft_size / 2 + 1];
    let mut spectrum = vec![0.0f64; sum.len()];
    let mut frames = 0usize;

    for range in ranges {
        let mut at = range.start;
        while at + fft_size <= range.end.min(samples.len()) {
            power_spectrum_with(&mut plan, &samples[at..], &window, &mut spectrum);
            for (s, v) in sum.iter_mut().zip(&spectrum) {
                *s += v;
            }
            frames += 1;
            at += hop;
        }
    }

    if frames < 4 {
        return None;
    }
    for s in &mut sum {
        *s /= frames as f64;
    }

    Some(smooth_to_log_grid(sum, sample_rate, options, frames))
}

/// Fold a linear-bin PSD onto the smoothed log grid.
pub fn smooth_to_log_grid(
    psd: Vec<f64>,
    sample_rate: u32,
    options: &LtasOptions,
    frames: usize,
) -> Ltas {
    let max_freq = options.max_freq.min(f64::from(sample_rate) / 2.0 - 1.0);
    let bin_hz = f64::from(sample_rate) / ((psd.len() - 1) * 2) as f64;

    let octaves = (max_freq / options.min_freq).log2();
    let points = ((octaves * options.points_per_octave).round() as usize + 1).max(2);

    let half = 2f64.powf(options.smoothing_octaves / 2.0);
    let mut freqs = Vec::with_capacity(points);
    let mut db = Vec::with_capacity(points);

    for p in 0..points {
        let f = options.min_freq * 2f64.powf(p as f64 / (points - 1) as f64 * octaves);
        freqs.push(f);

        // Fractional-octave bandwidth, widened to the Hz floor wherever it is
        // too narrow to span a voice's harmonic spacing.
        let (mut lo_hz, mut hi_hz) = (f / half, f * half);
        if hi_hz - lo_hz < options.min_bandwidth_hz {
            lo_hz = f - options.min_bandwidth_hz / 2.0;
            hi_hz = f + options.min_bandwidth_hz / 2.0;
        }

        let lo = ((lo_hz / bin_hz).floor().max(1.0) as usize).min(psd.len() - 1);
        let hi = ((hi_hz / bin_hz).ceil() as usize).clamp(lo, psd.len() - 1);

        let acc: f64 = psd[lo..=hi].iter().sum();
        db.push(10.0 * (acc / (hi - lo + 1) as f64 + 1e-30).log10());
    }

    Ltas {
        freqs,
        db,
        frames,
        psd,
        sample_rate,
    }
}

#[cfg(test)]
// `&[0..n]` here is a one-element slice of ranges, which is what `compute`
// takes; clippy reads it as a mistyped `vec![0; n]`.
#[expect(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    fn tone(freq: f64, n: usize, amplitude: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (amplitude * (TAU * freq * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    fn mix(parts: &[(f64, f64)], n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                parts
                    .iter()
                    .map(|(f, a)| a * (TAU * f * i as f64 / f64::from(SR)).sin())
                    .sum::<f64>() as f32
            })
            .collect()
    }

    /// Level of the grid point nearest `freq`.
    fn at(ltas: &Ltas, freq: f64) -> f64 {
        let i = ltas
            .freqs
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - freq).abs().total_cmp(&(b.1 - freq).abs()))
            .map(|(i, _)| i)
            .unwrap();
        ltas.db[i]
    }

    #[test]
    fn a_tone_shows_up_at_its_own_frequency() {
        let samples = tone(1_000.0, 100_000, 0.5);
        let ltas = compute(&samples, SR, &[0..100_000], &LtasOptions::default()).unwrap();
        assert!(at(&ltas, 1_000.0) > at(&ltas, 4_000.0) + 40.0);
        assert!(at(&ltas, 1_000.0) > at(&ltas, 250.0) + 40.0);
    }

    #[test]
    fn a_louder_tone_measures_louder_by_the_same_amount() {
        let options = LtasOptions::default();
        let loud = compute(&tone(1_000.0, 100_000, 0.5), SR, &[0..100_000], &options).unwrap();
        let quiet = compute(&tone(1_000.0, 100_000, 0.05), SR, &[0..100_000], &options).unwrap();
        let difference = at(&loud, 1_000.0) - at(&quiet, 1_000.0);
        assert!((difference - 20.0).abs() < 0.2, "{difference}");
    }

    #[test]
    fn only_the_ranges_given_are_averaged() {
        // First half a 1 kHz tone, second half a 4 kHz one. Ask for the first
        // half and the curve must not know about the second.
        let mut samples = tone(1_000.0, 100_000, 0.5);
        samples.extend(tone(4_000.0, 100_000, 0.5));

        let options = LtasOptions::default();
        let first = compute(&samples, SR, &[0..100_000], &options).unwrap();
        assert!(at(&first, 1_000.0) > at(&first, 4_000.0) + 40.0);

        let second = compute(&samples, SR, &[100_000..200_000], &options).unwrap();
        assert!(at(&second, 4_000.0) > at(&second, 1_000.0) + 40.0);
    }

    #[test]
    fn too_little_material_returns_nothing_rather_than_an_anecdote() {
        let options = LtasOptions::default();
        // Two frames' worth: fewer than the four the average demands.
        assert!(compute(&tone(1_000.0, 20_000, 0.5), SR, &[0..20_000], &options).is_none());
        assert!(compute(&tone(1_000.0, 20_000, 0.5), SR, &[], &options).is_none());
    }

    #[test]
    fn the_grid_is_log_spaced_across_the_range_asked_for() {
        let ltas = compute(
            &tone(1_000.0, 100_000, 0.5),
            SR,
            &[0..100_000],
            &LtasOptions::default(),
        )
        .unwrap();

        assert!((ltas.freqs[0] - 50.0).abs() < 1e-9);
        assert!((ltas.freqs.last().unwrap() - 16_000.0).abs() < 1e-6);
        assert_eq!(ltas.freqs.len(), ltas.db.len());
        // Log-spaced: the ratio between neighbours is constant.
        let first = ltas.freqs[1] / ltas.freqs[0];
        for pair in ltas.freqs.windows(2) {
            assert!((pair[1] / pair[0] - first).abs() < 1e-9);
        }
    }

    #[test]
    fn the_grid_stops_below_nyquist_whatever_it_was_asked_for() {
        let samples = tone(1_000.0, 40_000, 0.5);
        let ltas = compute(&samples, 22_050, &[0..40_000], &LtasOptions::default()).unwrap();
        assert!(*ltas.freqs.last().unwrap() < 11_025.0);
    }

    #[test]
    fn a_broad_target_flattens_a_narrow_resonance_but_keeps_the_tilt() {
        // A resonance at 1 kHz on a spectrum that is otherwise tilted down.
        let mut parts: Vec<(f64, f64)> = (1..40).map(|h| (100.0 * f64::from(h), 0.02)).collect();
        parts.push((1_000.0, 0.4));
        let ltas = compute(
            &mix(&parts, 200_000),
            SR,
            &[0..200_000],
            &LtasOptions::default(),
        )
        .unwrap();
        let target = ltas.broad_target(1.5);

        let i = ltas.freqs.iter().position(|f| *f > 1_000.0).unwrap();
        assert!(
            ltas.db[i] > target[i] + 3.0,
            "the resonance should stand above its own broad average: {} vs {}",
            ltas.db[i],
            target[i]
        );
        // Away from it the two curves should agree closely — the target is not
        // a flat line, it is the same spectrum seen less sharply.
        let j = ltas.freqs.iter().position(|f| *f > 3_000.0).unwrap();
        assert!((ltas.db[j] - target[j]).abs() < 6.0);
    }

    #[test]
    fn band_energy_finds_the_band_the_tone_is_in() {
        let ltas = compute(
            &tone(200.0, 200_000, 0.5),
            SR,
            &[0..200_000],
            &LtasOptions::default(),
        )
        .unwrap();
        assert!(ltas.band_energy_db(150.0, 250.0) > ltas.band_energy_db(1_000.0, 2_000.0) + 40.0);
    }

    #[test]
    fn an_empty_band_is_minus_infinity_rather_than_a_panic() {
        let ltas = compute(
            &tone(1_000.0, 100_000, 0.5),
            SR,
            &[0..100_000],
            &LtasOptions::default(),
        )
        .unwrap();
        assert_eq!(ltas.band_energy_db(9_000.0, 8_000.0), f64::NEG_INFINITY);
    }

    #[test]
    fn a_range_running_past_the_signal_is_clamped() {
        let samples = tone(1_000.0, 100_000, 0.5);
        let full = compute(&samples, SR, &[0..100_000], &LtasOptions::default()).unwrap();
        let over = compute(&samples, SR, &[0..usize::MAX], &LtasOptions::default()).unwrap();
        assert_eq!(full.frames, over.frames);
    }
}
