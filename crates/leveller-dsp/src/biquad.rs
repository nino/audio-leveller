//! Biquad design (RBJ audio-EQ cookbook), application, and analytic response.
//!
//! One coefficient struct serves everything: the EQ stages' bells and shelves,
//! and the ITU-R BS.1770 K-weighting pair, whose coefficients are derived
//! analytically rather than looked up. Always normalised so a0 = 1.

use std::f64::consts::{SQRT_2, TAU};

/// A second-order section, normalised so a0 = 1.
///
/// The difference equation is
/// `y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    /// The identity: passes its input through untouched.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Peaking (bell) EQ: `gain_db` at `freq`, bandwidth set by `q`.
    pub fn peaking(sample_rate: f64, freq: f64, gain_db: f64, q: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = TAU * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();

        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos_w0) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }

    /// Low shelf: `gain_db` below `freq`, S = 1 slope.
    pub fn low_shelf(sample_rate: f64, freq: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = TAU * freq / sample_rate;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / 2.0 * SQRT_2;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let a0 = a + 1.0 + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        Self {
            b0: (a * (a + 1.0 - (a - 1.0) * cos_w0 + two_sqrt_a_alpha)) / a0,
            b1: (2.0 * a * (a - 1.0 - (a + 1.0) * cos_w0)) / a0,
            b2: (a * (a + 1.0 - (a - 1.0) * cos_w0 - two_sqrt_a_alpha)) / a0,
            a1: (-2.0 * (a - 1.0 + (a + 1.0) * cos_w0)) / a0,
            a2: (a + 1.0 + (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
        }
    }

    /// High shelf: `gain_db` above `freq`, S = 1 slope.
    pub fn high_shelf(sample_rate: f64, freq: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = TAU * freq / sample_rate;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / 2.0 * SQRT_2;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let a0 = a + 1.0 - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        Self {
            b0: (a * (a + 1.0 + (a - 1.0) * cos_w0 + two_sqrt_a_alpha)) / a0,
            b1: (-2.0 * a * (a - 1.0 + (a + 1.0) * cos_w0)) / a0,
            b2: (a * (a + 1.0 + (a - 1.0) * cos_w0 - two_sqrt_a_alpha)) / a0,
            a1: (2.0 * (a - 1.0 - (a + 1.0) * cos_w0)) / a0,
            a2: (a + 1.0 - (a - 1.0) * cos_w0 - two_sqrt_a_alpha) / a0,
        }
    }

    /// Second-order Butterworth high-pass (−3 dB at `freq`).
    pub fn butterworth_high_pass(sample_rate: f64, freq: f64) -> Self {
        let w0 = TAU * freq / sample_rate;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / SQRT_2; // Q = 1/√2

        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos_w0) / 2.0 / a0,
            b1: -(1.0 + cos_w0) / a0,
            b2: (1.0 + cos_w0) / 2.0 / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Magnitude response at `freq`, in dB.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        let w = TAU * freq / sample_rate;
        let (cos1, sin1) = (w.cos(), w.sin());
        let (cos2, sin2) = ((2.0 * w).cos(), (2.0 * w).sin());

        let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
        let num_im = -(self.b1 * sin1 + self.b2 * sin2);
        let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
        let den_im = -(self.a1 * sin1 + self.a2 * sin2);

        let num = num_re * num_re + num_im * num_im;
        let den = den_re * den_re + den_im * den_im;
        if den == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (num / den).log10()
        }
    }

    /// Filter one channel in place, Direct Form I, starting from rest.
    pub fn apply_in_place(&self, signal: &mut [f32]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for sample in signal.iter_mut() {
            let x0 = f64::from(*sample);
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            *sample = y0 as f32;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
        }
    }

    /// Filter one channel, returning a new buffer.
    pub fn apply(&self, signal: &[f32]) -> Vec<f32> {
        let mut out = signal.to_vec();
        self.apply_in_place(&mut out);
        out
    }
}

/// Summed magnitude response of a cascade at `freq`, in dB.
pub fn cascade_magnitude_db(cascade: &[Biquad], freq: f64, sample_rate: f64) -> f64 {
    cascade
        .iter()
        .map(|c| c.magnitude_db(freq, sample_rate))
        .sum()
}

/// Run a cascade over one channel, in order, in place.
pub fn apply_cascade_in_place(signal: &mut [f32], cascade: &[Biquad]) {
    for c in cascade {
        c.apply_in_place(signal);
    }
}

/// Run a cascade over one channel, returning a new buffer.
pub fn apply_cascade(signal: &[f32], cascade: &[Biquad]) -> Vec<f32> {
    let mut out = signal.to_vec();
    apply_cascade_in_place(&mut out, cascade);
    out
}

/// ITU-R BS.1770 "K-weighting": a high-shelf head filter followed by an RLB
/// high-pass, applied before the mean-square loudness measurement.
///
/// Derived analytically for an arbitrary sample rate using libebur128's
/// formulation, so measurements are right at 44.1 kHz and 96 kHz and not only
/// at the 48 kHz the standard prints coefficients for.
pub mod kweighting {
    use super::Biquad;
    use std::f64::consts::PI;

    /// Stage 1: high-shelf boost centred at ~1681 Hz, the "head" filter.
    pub fn shelving(sample_rate: f64) -> Biquad {
        let f0 = 1_681.974_450_955_532;
        let g = 3.999_843_853_394_032_4;
        let q = 0.707_175_236_955_419_3;

        let k = (PI * f0 / sample_rate).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);

        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: (2.0 * (k * k - vh)) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: (2.0 * (k * k - 1.0)) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        }
    }

    /// Stage 2: RLB high-pass at ~38 Hz.
    ///
    /// The numerator is the unnormalised `1, −2, 1` on purpose: this is
    /// libebur128's form, and dividing it by a0 as well would shift every
    /// measurement off the standard.
    pub fn high_pass(sample_rate: f64) -> Biquad {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;

        let k = (PI * f0 / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: (2.0 * (k * k - 1.0)) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        }
    }

    /// Both stages, in order.
    pub fn cascade(sample_rate: f64) -> [Biquad; 2] {
        [shelving(sample_rate), high_pass(sample_rate)]
    }

    /// Apply the full two-stage filter to one channel.
    pub fn apply(channel: &[f32], sample_rate: f64) -> Vec<f32> {
        super::apply_cascade(channel, &cascade(sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Impulse response, then a DFT of it at one frequency — an independent
    /// route to the magnitude that shares no code with `magnitude_db`.
    fn measured_db(c: &Biquad, freq: f64, sample_rate: f64) -> f64 {
        let n = 16384;
        let mut impulse = vec![0.0f32; n];
        impulse[0] = 1.0;
        c.apply_in_place(&mut impulse);

        let w = TAU * freq / sample_rate;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, x) in impulse.iter().enumerate() {
            let angle = -w * i as f64;
            re += f64::from(*x) * angle.cos();
            im += f64::from(*x) * angle.sin();
        }
        10.0 * (re * re + im * im).log10()
    }

    #[test]
    fn a_bell_hits_its_gain_at_its_centre_and_nowhere_else() {
        let sr = 48_000.0;
        let c = Biquad::peaking(sr, 1000.0, 6.0, 2.0);
        assert!((c.magnitude_db(1000.0, sr) - 6.0).abs() < 1e-9);
        assert!(c.magnitude_db(100.0, sr).abs() < 0.5);
        assert!(c.magnitude_db(10_000.0, sr).abs() < 0.5);
    }

    #[test]
    fn shelves_reach_their_gain_in_the_band_they_name() {
        let sr = 48_000.0;
        let low = Biquad::low_shelf(sr, 200.0, -4.0);
        assert!((low.magnitude_db(20.0, sr) - -4.0).abs() < 0.3);
        assert!(low.magnitude_db(8_000.0, sr).abs() < 0.1);

        let high = Biquad::high_shelf(sr, 6_000.0, 3.0);
        assert!((high.magnitude_db(20_000.0, sr) - 3.0).abs() < 0.3);
        assert!(high.magnitude_db(100.0, sr).abs() < 0.1);
    }

    #[test]
    fn a_butterworth_high_pass_is_three_db_down_at_its_corner() {
        let sr = 48_000.0;
        let c = Biquad::butterworth_high_pass(sr, 80.0);
        assert!((c.magnitude_db(80.0, sr) - -3.0103).abs() < 0.05);
        assert!(c.magnitude_db(8_000.0, sr).abs() < 0.01);
        assert!(c.magnitude_db(20.0, sr) < -20.0);
    }

    #[test]
    fn the_analytic_response_matches_a_measured_one() {
        let sr = 48_000.0;
        for c in [
            Biquad::peaking(sr, 2_500.0, -5.0, 1.4),
            Biquad::low_shelf(sr, 120.0, 3.0),
            Biquad::high_shelf(sr, 8_000.0, -2.5),
            Biquad::butterworth_high_pass(sr, 60.0),
        ] {
            for &f in &[50.0, 200.0, 1_000.0, 5_000.0, 15_000.0] {
                let (a, m) = (c.magnitude_db(f, sr), measured_db(&c, f, sr));
                assert!(
                    (a - m).abs() < 0.02,
                    "at {f} Hz: analytic {a}, measured {m}"
                );
            }
        }
    }

    #[test]
    fn the_identity_leaves_a_signal_alone() {
        let input: Vec<f32> = (0..64).map(|i| (i as f32 * 0.3).sin()).collect();
        assert_eq!(Biquad::IDENTITY.apply(&input), input);
    }

    #[test]
    fn k_weighting_matches_the_curve_libebur128_produces() {
        // Pinned against the same formulation computed independently, at three
        // rates, so a later "tidy-up" of the coefficients cannot quietly move
        // every loudness number in the project. The 20 Hz and 10 kHz columns
        // are the two landmarks BS.1770 names: the RLB roll-off, and the ~+4 dB
        // head shelf.
        let expected = [
            (44_100.0, [-13.2715, -1.1297, 0.7005, 4.0458]),
            (48_000.0, [-13.2754, -1.1335, 0.6977, 4.0419]),
            (96_000.0, [-13.2970, -1.1551, 0.6804, 4.0195]),
        ];
        for (sr, row) in expected {
            let c = kweighting::cascade(sr);
            for (&freq, want) in [20.0, 100.0, 1_000.0, 10_000.0].iter().zip(row) {
                let got = cascade_magnitude_db(&c, freq, sr);
                assert!(
                    (got - want).abs() < 5e-4,
                    "{sr} Hz at {freq} Hz: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn k_weighting_applied_in_time_agrees_with_its_own_response() {
        let sr = 48_000.0;
        let cascade = kweighting::cascade(sr);
        for &freq in &[100.0, 1_000.0, 10_000.0] {
            let n = 48_000;
            // Skip the first 2000 samples of the output: the filter starts from
            // rest, and its transient is not part of its steady-state gain.
            let input: Vec<f32> = (0..n)
                .map(|i| (TAU * freq * i as f64 / sr).sin() as f32)
                .collect();
            let out = kweighting::apply(&input, sr);
            let rms = |xs: &[f32]| -> f64 {
                let tail = &xs[2_000..];
                (tail
                    .iter()
                    .map(|x| f64::from(*x) * f64::from(*x))
                    .sum::<f64>()
                    / tail.len() as f64)
                    .sqrt()
            };
            let measured = 20.0 * (rms(&out) / rms(&input)).log10();
            let analytic = cascade_magnitude_db(&cascade, freq, sr);
            assert!(
                (measured - analytic).abs() < 0.02,
                "at {freq} Hz: measured {measured}, analytic {analytic}"
            );
        }
    }

    #[test]
    fn a_cascade_sums_in_db_and_composes_in_time() {
        let sr = 48_000.0;
        let cascade = [
            Biquad::peaking(sr, 300.0, 4.0, 1.0),
            Biquad::high_shelf(sr, 5_000.0, -3.0),
        ];
        let summed = cascade_magnitude_db(&cascade, 1_000.0, sr);
        let by_hand: f64 = cascade.iter().map(|c| c.magnitude_db(1_000.0, sr)).sum();
        assert!((summed - by_hand).abs() < 1e-12);

        let input: Vec<f32> = (0..256).map(|i| (i as f32 * 0.17).sin()).collect();
        let stepwise = cascade[1].apply(&cascade[0].apply(&input));
        assert_eq!(apply_cascade(&input, &cascade), stepwise);
    }
}
