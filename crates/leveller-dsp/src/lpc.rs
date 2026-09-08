//! Linear prediction: fitting an all-pole model to a short window of audio, and
//! using it to rebuild samples that were destroyed.
//!
//! Over 20–50 ms, speech is well described by an autoregressive model — each
//! sample is roughly a fixed linear combination of the ones before it, because
//! that is what a resonant vocal tract does. Two things follow, and the
//! de-clicker uses both:
//!
//! - A sample that badly violates the model is suspicious. That is detection.
//! - Missing samples can be recovered by choosing the values that make the
//!   signal fit the model best. That is repair.
//!
//! Coefficient convention throughout: `a[0] == 1`, and the prediction error is
//! `e[n] = Σ(k = 0..p) a[k]·x[n−k]`, so the classic predictor coefficients are
//! `−a[1..p]`. Keeping the leading 1 explicit makes the interpolation below far
//! easier to follow.

/// An all-pole model of a window of audio.
#[derive(Clone, Debug, PartialEq)]
pub struct ArModel {
    /// `a[0] == 1`, then the p coefficients.
    coefficients: Vec<f64>,
}

impl ArModel {
    /// Fit a model of `order` poles to `samples[range]`.
    pub fn fit(samples: &[f32], range: std::ops::Range<usize>, order: usize) -> Self {
        Self::from_autocorrelation(&autocorrelation(samples, range, order), order)
    }

    /// Levinson–Durbin recursion: autocorrelation to coefficients.
    pub fn from_autocorrelation(r: &[f64], order: usize) -> Self {
        let mut a = vec![0.0; order + 1];
        a[0] = 1.0;
        let mut error = r[0];
        if error <= 0.0 {
            return Self { coefficients: a };
        }

        let mut temp = vec![0.0; order + 1];
        for i in 1..=order {
            let mut acc = r[i];
            for j in 1..i {
                acc += a[j] * r[i - j];
            }

            let reflection = -acc / error;
            // A reflection coefficient outside (−1, 1) means the recursion has
            // gone unstable, which in practice means a degenerate window. Stop
            // with the poles found so far rather than returning nonsense.
            if !reflection.is_finite() || reflection.abs() >= 1.0 {
                break;
            }

            for j in 1..i {
                temp[j] = a[j] + reflection * a[i - j];
            }
            // From 1: a[0] is the leading 1 and temp never holds it.
            a[1..i].copy_from_slice(&temp[1..i]);
            a[i] = reflection;

            error *= 1.0 - reflection * reflection;
            if error <= 0.0 {
                break;
            }
        }

        Self { coefficients: a }
    }

    /// Number of poles.
    pub fn order(&self) -> usize {
        self.coefficients.len() - 1
    }

    pub fn coefficients(&self) -> &[f64] {
        &self.coefficients
    }

    /// Prediction error at `index`: how far the sample is from what the model
    /// expected, given the ones before it.
    ///
    /// Samples before the start of the buffer count as zero, so the first
    /// `order` residuals are not meaningful and callers skip them.
    pub fn residual_at(&self, samples: &[f32], index: usize) -> f64 {
        self.coefficients
            .iter()
            .enumerate()
            .map(|(k, a)| match index.checked_sub(k) {
                Some(at) => a * f64::from(samples[at]),
                None => 0.0,
            })
            .sum()
    }

    /// The residual over a range, running forwards.
    pub fn residual(&self, samples: &[f32], range: std::ops::Range<usize>) -> Vec<f64> {
        range.map(|i| self.residual_at(samples, i)).collect()
    }

    /// Autocorrelation of the coefficient vector: `ra[m] = Σ_k a[k]·a[k+m]`.
    ///
    /// This is what collapses the interpolation's normal equations down to
    /// p + 1 distinct values.
    pub fn coefficient_autocorrelation(&self) -> Vec<f64> {
        let p = self.order();
        (0..=p)
            .map(|m| {
                (0..=p - m)
                    .map(|k| self.coefficients[k] * self.coefficients[k + m])
                    .sum()
            })
            .collect()
    }
}

/// Autocorrelation of a window, lags 0..=order.
///
/// `r[0]` gets a small ridge so a silent or near-constant window still yields a
/// solvable — if meaningless — system rather than dividing by zero.
pub fn autocorrelation(samples: &[f32], range: std::ops::Range<usize>, order: usize) -> Vec<f64> {
    let from = range.start;
    let to = range.end.min(samples.len());

    let mut r: Vec<f64> = (0..=order)
        .map(|lag| {
            if from + lag >= to {
                return 0.0;
            }
            (from + lag..to)
                .map(|i| f64::from(samples[i]) * f64::from(samples[i - lag]))
                .sum()
        })
        .collect();

    // White-noise correction: lifts the model off a singular solution and stops
    // it chasing an exact fit to one window's noise.
    r[0] = r[0] * 1.0001 + 1e-12;
    r
}

/// Solve a small dense system by Gaussian elimination with partial pivoting.
///
/// Returns `None` if the matrix is singular, which the caller reads as "leave
/// this gap alone".
fn solve(matrix: &mut [f64], rhs: &mut [f64], n: usize) -> Option<Vec<f64>> {
    for col in 0..n {
        let mut pivot_row = col;
        let mut best = matrix[col * n + col].abs();
        for row in col + 1..n {
            let value = matrix[row * n + col].abs();
            if value > best {
                best = value;
                pivot_row = row;
            }
        }
        if best < 1e-18 {
            return None;
        }

        if pivot_row != col {
            for k in 0..n {
                matrix.swap(col * n + k, pivot_row * n + k);
            }
            rhs.swap(col, pivot_row);
        }

        let pivot = matrix[col * n + col];
        for row in col + 1..n {
            let factor = matrix[row * n + col] / pivot;
            if factor == 0.0 {
                continue;
            }
            for k in col..n {
                matrix[row * n + k] -= factor * matrix[col * n + k];
            }
            rhs[row] -= factor * rhs[col];
        }
    }

    let mut x = rhs.to_vec();
    for row in (0..n).rev() {
        let mut sum = x[row];
        for k in row + 1..n {
            sum -= matrix[row * n + k] * x[k];
        }
        x[row] = sum / matrix[row * n + row];
    }
    Some(x)
}

/// Least-squares AR interpolation (Janssen): replace `samples[gap]` with the
/// values that minimise the model's total prediction error, given the samples
/// around them.
///
/// Minimising `J = Σ_n e[n]²` over the unknowns gives, for each unknown index m,
/// `Σ_j ra[|m − j|]·x[j] = 0` over every j within p of m. Splitting that into
/// unknowns on the left and known neighbours on the right is the system solved
/// here. Unlike a fade or a straight line across the hole, this reconstructs the
/// resonance that was there — over a few milliseconds of speech the repair is
/// inaudible.
///
/// Writes in place. Returns false, having touched nothing, if the system is
/// singular or the gap runs past the model's reach at either end of the buffer.
pub fn interpolate_gap(samples: &mut [f32], gap: std::ops::Range<usize>, model: &ArModel) -> bool {
    let p = model.order();
    let gap_length = gap.end.saturating_sub(gap.start);
    if gap_length == 0 {
        return false;
    }
    // The solve needs p known samples of context on each side.
    if gap.start < p || gap.end + p > samples.len() {
        return false;
    }

    let ra = model.coefficient_autocorrelation();
    let mut matrix = vec![0.0; gap_length * gap_length];
    let mut rhs = vec![0.0; gap_length];

    for i in 0..gap_length {
        let m = gap.start + i;

        for j in 0..gap_length {
            let lag = m.abs_diff(gap.start + j);
            matrix[i * gap_length + j] = if lag <= p { ra[lag] } else { 0.0 };
        }

        // Known neighbours move to the right-hand side, negated.
        let mut sum = 0.0;
        for j in m.saturating_sub(p)..=(m + p).min(samples.len() - 1) {
            if gap.contains(&j) {
                continue; // still unknown; it stays on the left
            }
            sum += ra[m.abs_diff(j)] * f64::from(samples[j]);
        }
        rhs[i] = -sum;
    }

    let Some(solution) = solve(&mut matrix, &mut rhs, gap_length) else {
        return false;
    };
    if solution.iter().any(|v| !v.is_finite()) {
        return false;
    }

    for (i, value) in solution.into_iter().enumerate() {
        samples[gap.start + i] = value as f32;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// A damped resonance — exactly the kind of thing an all-pole model
    /// describes perfectly, and what a vowel is.
    fn resonance(n: usize) -> Vec<f32> {
        let mut x = vec![0.0f32; n];
        // Two poles: y[n] = 1.8·cos(w)·y[n−1] − 0.98²·y[n−2], excited once.
        let w = TAU * 300.0 / 48_000.0;
        let (r, mut y1, mut y2) = (0.995, 0.0f64, 0.0f64);
        for (i, out) in x.iter_mut().enumerate() {
            let excitation = if i.is_multiple_of(160) { 1.0 } else { 0.0 };
            let y = excitation + 2.0 * r * w.cos() * y1 - r * r * y2;
            *out = y as f32;
            y2 = y1;
            y1 = y;
        }
        x
    }

    #[test]
    fn the_model_starts_with_one() {
        let model = ArModel::fit(&resonance(2_000), 0..2_000, 16);
        assert_eq!(model.coefficients()[0], 1.0);
        assert_eq!(model.order(), 16);
        assert_eq!(model.coefficients().len(), 17);
    }

    #[test]
    fn a_resonance_is_predicted_far_better_than_noise_is() {
        // The point of the whole module: an AR model of speech-like material
        // leaves a small residual, and one of white noise cannot.
        let tone = resonance(4_000);
        let tone_model = ArModel::fit(&tone, 0..4_000, 24);
        let tone_residual = rms(&tone_model.residual(&tone, 100..4_000));
        let tone_signal = rms_f32(&tone[100..4_000]);

        let mut seed = 12_345u32;
        let noise: Vec<f32> = (0..4_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as f32 / 32_768.0) - 1.0
            })
            .collect();
        let noise_model = ArModel::fit(&noise, 0..4_000, 24);
        let noise_residual = rms(&noise_model.residual(&noise, 100..4_000));
        let noise_signal = rms_f32(&noise[100..4_000]);

        assert!(
            tone_residual / tone_signal < 0.1 * (noise_residual / noise_signal),
            "resonance {:.4}, noise {:.4}",
            tone_residual / tone_signal,
            noise_residual / noise_signal
        );
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn rms_f32(x: &[f32]) -> f64 {
        rms(&x.iter().map(|v| f64::from(*v)).collect::<Vec<_>>())
    }

    #[test]
    fn a_silent_window_yields_a_model_rather_than_a_division_by_zero() {
        let model = ArModel::fit(&vec![0.0f32; 1_000], 0..1_000, 16);
        assert!(model.coefficients().iter().all(|c| c.is_finite()));
        assert_eq!(model.coefficients()[0], 1.0);
    }

    #[test]
    fn interpolation_rebuilds_a_hole_it_cannot_see() {
        let original = resonance(4_000);
        let mut damaged = original.clone();
        // Punch out 30 samples in the middle and fill them with rubbish.
        let gap = 2_000..2_030;
        for s in &mut damaged[gap.clone()] {
            *s = 5.0;
        }

        // Fit on the surrounding audio, not on the damage.
        let model = ArModel::fit(&original, 1_500..2_500, 32);
        assert!(interpolate_gap(&mut damaged, gap.clone(), &model));

        let error = rms_f32(&damaged[gap.clone()]) - rms_f32(&original[gap.clone()]);
        let worst = damaged[gap.clone()]
            .iter()
            .zip(&original[gap])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let scale = original.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            worst < 0.05 * scale,
            "worst {worst} against a peak of {scale} (level error {error})"
        );
    }

    #[test]
    fn interpolation_beats_drawing_a_straight_line_across_the_hole() {
        let original = resonance(4_000);
        let gap = 2_000..2_024;
        let model = ArModel::fit(&original, 1_500..2_500, 32);

        let mut repaired = original.clone();
        for s in &mut repaired[gap.clone()] {
            *s = 0.0;
        }
        assert!(interpolate_gap(&mut repaired, gap.clone(), &model));

        let mut bridged = original.clone();
        let (a, b) = (original[gap.start - 1], original[gap.end]);
        for (i, s) in bridged[gap.clone()].iter_mut().enumerate() {
            let t = (i + 1) as f32 / (gap.len() + 1) as f32;
            *s = a + (b - a) * t;
        }

        let err = |x: &[f32]| -> f64 {
            rms_f32(
                &x[gap.clone()]
                    .iter()
                    .zip(&original[gap.clone()])
                    .map(|(p, q)| p - q)
                    .collect::<Vec<_>>(),
            )
        };
        assert!(
            err(&repaired) < 0.25 * err(&bridged),
            "AR {:.5} vs linear {:.5}",
            err(&repaired),
            err(&bridged)
        );
    }

    #[test]
    fn a_gap_too_near_an_edge_is_declined_rather_than_botched() {
        let mut samples = resonance(1_000);
        let model = ArModel::fit(&samples, 0..1_000, 32);
        let before = samples.clone();

        assert!(
            !interpolate_gap(&mut samples, 4..10, &model),
            "too near the start"
        );
        assert!(
            !interpolate_gap(&mut samples, 980..999, &model),
            "too near the end"
        );
        assert!(
            !interpolate_gap(&mut samples, 500..500, &model),
            "an empty gap"
        );
        assert_eq!(samples, before, "a declined repair must touch nothing");
    }

    #[test]
    fn the_coefficient_autocorrelation_is_the_definition() {
        let model = ArModel::fit(&resonance(2_000), 0..2_000, 8);
        let a = model.coefficients();
        let ra = model.coefficient_autocorrelation();
        assert_eq!(ra.len(), 9);
        for (m, got) in ra.iter().enumerate() {
            let want: f64 = (0..=8 - m).map(|k| a[k] * a[k + m]).sum();
            assert!((got - want).abs() < 1e-15, "lag {m}");
        }
    }

    #[test]
    fn a_residual_range_agrees_with_the_per_sample_form() {
        let samples = resonance(500);
        let model = ArModel::fit(&samples, 0..500, 8);
        let range = model.residual(&samples, 100..110);
        for (i, value) in range.iter().enumerate() {
            assert_eq!(*value, model.residual_at(&samples, 100 + i));
        }
    }
}
