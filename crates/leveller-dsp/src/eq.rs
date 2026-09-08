//! Corrective EQ fitting.
//!
//! The deviation to correct is `LTAS − broad target` (see [`crate::ltas`]): the
//! resonances and notches a room or a microphone stamped on the voice, with the
//! voice's own broad tilt already subtracted out. The fitter places a small
//! number of peaking filters against that deviation, greedily — find the worst
//! remaining deviation, place a band that cancels it, subtract that band's
//! analytic response, repeat.
//!
//! What separates "professionally corrected" from "obviously auto-EQ'd" is the
//! constraint set, not the fitter:
//!
//! - **Few bands** (five by default). A forest of filters is a spectral match,
//!   and spectral matches sound processed.
//! - **Gain clamped, and cut-biased.** Cuts get the full ±6 dB, boosts half
//!   that. Cutting a resonance is nearly always safe; boosting a notch dredges
//!   up whatever lives down there.
//! - **No boosts where the noise floor is close.** Boosting a band whose
//!   signal-to-noise is poor is buying timbre with noise.
//! - **Small deviations left alone.** Under 2 dB is what voices sound like.

use crate::biquad::Biquad;
use crate::ltas::Ltas;

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct EqBand {
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct EqFitOptions {
    /// Most bands the fitter may place.
    pub max_bands: usize,
    /// Leave deviations smaller than this alone, in dB.
    pub min_deviation_db: f64,
    /// Cut limit, as a positive number of dB.
    pub max_cut_db: f64,
    /// Boost limit — deliberately half the cut limit.
    pub max_boost_db: f64,
    /// Q clamp: wide enough to correct, never surgical.
    pub min_q: f64,
    pub max_q: f64,
    /// Fraction of a deviation a band tries to remove. Under-correcting is how
    /// human engineers EQ; full cancellation rings and pumps the fit.
    pub correction: f64,
    /// No boost where the speech-to-noise ratio at that grid point is below
    /// this, in dB.
    pub min_boost_snr_db: f64,
    /// Fit only within this range; outside it the room and microphone data is
    /// not to be trusted.
    pub min_freq: f64,
    pub max_freq: f64,
}

impl Default for EqFitOptions {
    fn default() -> Self {
        Self {
            max_bands: 5,
            min_deviation_db: 2.0,
            max_cut_db: 6.0,
            max_boost_db: 3.0,
            min_q: 0.7,
            max_q: 4.0,
            correction: 0.8,
            min_boost_snr_db: 15.0,
            min_freq: 80.0,
            max_freq: 12_000.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EqFit {
    pub bands: Vec<EqBand>,
    /// Largest absolute deviation before fitting, within the fit range, in dB.
    pub deviation_before_db: f64,
    /// Largest absolute deviation the fitter predicts once its bands apply.
    pub deviation_after_db: f64,
}

/// Estimate a band's Q from how wide the deviation bump is: walk outward from
/// the peak until the deviation falls to half, then convert that width to a Q.
fn estimate_q(deviation: &[f64], freqs: &[f64], peak_index: usize, options: &EqFitOptions) -> f64 {
    let peak = deviation[peak_index];
    let half = (peak / 2.0).abs();
    let same_sign = |v: f64| if peak > 0.0 { v > 0.0 } else { v < 0.0 };

    let mut lo = peak_index;
    while lo > 0 && same_sign(deviation[lo - 1]) && deviation[lo - 1].abs() > half {
        lo -= 1;
    }
    let mut hi = peak_index;
    while hi + 1 < deviation.len() && same_sign(deviation[hi + 1]) && deviation[hi + 1].abs() > half
    {
        hi += 1;
    }

    let octaves = (freqs[hi] / freqs[lo]).log2().max(0.1);
    // The standard octave-bandwidth to Q conversion.
    let width = 2f64.powf(octaves);
    (width.sqrt() / (width - 1.0)).clamp(options.min_q, options.max_q)
}

/// Fit peaking bands to flatten `speech`'s deviation from its own broad shape.
///
/// `noise` — the LTAS of the pauses, on the same grid — gates boosts. Pass
/// `None` to forbid boosting outright.
pub fn fit_corrective(
    speech: &Ltas,
    noise: Option<&Ltas>,
    sample_rate: u32,
    options: &EqFitOptions,
) -> EqFit {
    let freqs = &speech.freqs;
    let target = speech.broad_target(1.5);
    let rate = f64::from(sample_rate);

    // The deviation, masked to the range worth trusting.
    let mut in_range: Vec<bool> = freqs
        .iter()
        .map(|f| *f >= options.min_freq && *f <= options.max_freq)
        .collect();
    let mut deviation: Vec<f64> = (0..freqs.len())
        .map(|i| {
            if in_range[i] {
                speech.db[i] - target[i]
            } else {
                0.0
            }
        })
        .collect();

    let worst_of = |deviation: &[f64], in_range: &[bool]| -> Option<(usize, f64)> {
        deviation
            .iter()
            .enumerate()
            .filter(|(i, _)| in_range[*i])
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, v)| (i, v.abs()))
    };

    let deviation_before_db = worst_of(&deviation, &in_range).map_or(0.0, |(_, v)| v);
    let mut bands = Vec::new();

    // Bounded by bands *placed*, not by attempts. A deviation that gets masked
    // out — a notch the fitter declines to fill — must not consume the band
    // budget, or a few ungated notches early on leave nothing for the
    // resonances that follow. The attempt count is only a runaway guard: each
    // pass either places a band or takes a grid point out of play, so it always
    // terminates.
    let mut attempts = 0usize;
    while bands.len() < options.max_bands && attempts < freqs.len() {
        attempts += 1;
        let Some((index, magnitude)) = worst_of(&deviation, &in_range) else {
            break;
        };
        if magnitude < options.min_deviation_db {
            break;
        }

        let raw_gain = -deviation[index] * options.correction;

        // Boosting into a poor noise floor is buying timbre with noise.
        if raw_gain > 0.0 {
            let snr = noise.map_or(f64::NEG_INFINITY, |n| speech.db[index] - n.db[index]);
            if snr < options.min_boost_snr_db {
                // A notch that may not be filled: take it out of play, so the
                // fitter spends its remaining bands somewhere useful.
                in_range[index] = false;
                deviation[index] = 0.0;
                continue;
            }
        }

        let gain_db = raw_gain.clamp(-options.max_cut_db, options.max_boost_db);
        let q = estimate_q(&deviation, freqs, index, options);
        bands.push(EqBand {
            freq: freqs[index],
            gain_db,
            q,
        });

        // Subtract this band's analytic response from what is left.
        let band = Biquad::peaking(rate, freqs[index], gain_db, q);
        for (i, d) in deviation.iter_mut().enumerate() {
            if in_range[i] {
                *d += band.magnitude_db(freqs[i], rate);
            }
        }
    }

    EqFit {
        bands,
        deviation_before_db,
        deviation_after_db: worst_of(&deviation, &in_range).map_or(0.0, |(_, v)| v),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct RumbleDecision {
    /// Corner frequency of the high-pass, or `None` when none is warranted.
    pub freq: Option<f64>,
    /// The sub-bass excess that triggered it, in dB.
    pub excess_db: f64,
}

/// Decide whether a rumble high-pass is warranted, and where to put it.
///
/// Compares the energy well below the voice — 20 Hz up to the candidate corner
/// — against the fundamental region, 120 to 300 Hz. Speech has no business
/// carrying energy an octave below its own fundamental; when the sub-bass sits
/// within `margin_db` of the fundamentals, something non-vocal is down there —
/// traffic, handling, ventilation — and the high-pass earns its place. On clean
/// material it stays off, because an always-on filter is not corrective.
///
/// This reads the **raw** PSD, not the smoothed grid. The grid's minimum
/// bandwidth deliberately blurs the bottom octaves so the EQ sees an envelope
/// rather than a harmonic comb, and that same blur smears a voice's fundamental
/// down into the sub-bass region — enough to trip this test on perfectly clean
/// speech.
pub fn decide_rumble_filter(
    speech: &Ltas,
    corner_candidate: f64,
    margin_db: f64,
) -> RumbleDecision {
    let sub_db = speech.band_energy_db(20.0, corner_candidate);
    let voice_db = speech.band_energy_db(120.0, 300.0);
    if !sub_db.is_finite() || !voice_db.is_finite() {
        return RumbleDecision {
            freq: None,
            excess_db: 0.0,
        };
    }

    let excess_db = sub_db - voice_db;
    RumbleDecision {
        freq: (excess_db > margin_db).then_some(corner_candidate),
        excess_db,
    }
}

/// [`decide_rumble_filter`] at the thresholds the EQ stage uses.
pub fn decide_rumble(speech: &Ltas) -> RumbleDecision {
    decide_rumble_filter(speech, 80.0, -12.0)
}

/// The filter cascade for a fit: the rumble high-pass if there is one, then the
/// bands.
pub fn build_cascade(fit: &EqFit, rumble: &RumbleDecision, sample_rate: u32) -> Vec<Biquad> {
    let rate = f64::from(sample_rate);
    let mut cascade = Vec::with_capacity(fit.bands.len() + 1);
    if let Some(freq) = rumble.freq {
        cascade.push(Biquad::butterworth_high_pass(rate, freq));
    }
    cascade.extend(
        fit.bands
            .iter()
            .map(|b| Biquad::peaking(rate, b.freq, b.gain_db, b.q)),
    );
    cascade
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ltas::{self, LtasOptions};
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};
    use std::f64::consts::TAU;
    use std::sync::OnceLock;

    const SR: u32 = 48_000;

    /// The corpus voice, whose long-term average is smooth by construction —
    /// which is exactly what a corrective fitter needs to be judged against.
    /// A harmonic stack at fixed phases would leave a comb in the spectrum, and
    /// the fitter would dutifully chase it.
    fn voice() -> &'static [f32] {
        static VOICE: OnceLock<Vec<f32>> = OnceLock::new();
        VOICE.get_or_init(|| {
            let speech = synthetic_speech(&SpeechOptions {
                sample_rate: SR,
                spurts: vec![Spurt::new(12.0, -23.0)],
                pause_sec: 0.0,
                floor_dbfs: -80.0,
                seed: 4242,
                channels: 1,
            });
            speech.signal.channel(0).to_vec()
        })
    }

    /// The voice, optionally with a resonance stamped on it.
    fn material(resonance: Option<(f64, f64, f64)>) -> Vec<f32> {
        let mut samples = voice().to_vec();
        if let Some((freq, gain, q)) = resonance {
            Biquad::peaking(f64::from(SR), freq, gain, q).apply_in_place(&mut samples);
        }
        samples
    }

    fn ltas_of(samples: &[f32]) -> Ltas {
        ltas::compute(
            samples,
            SR,
            std::slice::from_ref(&(0..samples.len())),
            &LtasOptions::default(),
        )
        .expect("enough frames")
    }

    #[test]
    fn a_resonance_is_cut_at_roughly_the_right_place() {
        let speech = ltas_of(&material(Some((1_200.0, 10.0, 2.0))));
        let fit = fit_corrective(&speech, None, SR, &EqFitOptions::default());

        assert!(!fit.bands.is_empty(), "nothing was fitted");
        let band = fit
            .bands
            .iter()
            .min_by(|a, b| {
                (a.freq - 1_200.0)
                    .abs()
                    .total_cmp(&(b.freq - 1_200.0).abs())
            })
            .unwrap();
        assert!(
            (band.freq / 1_200.0).log2().abs() < 0.35,
            "band at {} Hz",
            band.freq
        );
        assert!(band.gain_db < -2.0, "gain {}", band.gain_db);
    }

    #[test]
    fn fitting_reduces_the_deviation_it_measured() {
        let speech = ltas_of(&material(Some((1_200.0, 10.0, 2.0))));
        let fit = fit_corrective(&speech, None, SR, &EqFitOptions::default());
        assert!(
            fit.deviation_after_db < fit.deviation_before_db,
            "{} then {}",
            fit.deviation_before_db,
            fit.deviation_after_db
        );
    }

    #[test]
    fn an_even_spectrum_gets_no_bands_at_all() {
        // The decline case, on white noise — the only genuinely even spectrum.
        // A fitter that always places something is not corrective.
        let flat = leveller_corpus::noise(400_000, 11);
        let fit = fit_corrective(&ltas_of(&flat), None, SR, &EqFitOptions::default());
        assert!(fit.bands.is_empty(), "{:?}", fit.bands);
        assert!(fit.deviation_before_db < 2.0, "{}", fit.deviation_before_db);
    }

    #[test]
    fn a_voice_gets_a_band_or_two_and_not_a_forest() {
        // Real material is never flat, and the fitter is not meant to make it
        // so: a couple of gentle corrections is the whole intended output.
        let fit = fit_corrective(
            &ltas_of(&material(None)),
            None,
            SR,
            &EqFitOptions::default(),
        );
        assert!(fit.bands.len() <= 3, "{:?}", fit.bands);
        for band in &fit.bands {
            assert!(
                band.gain_db.abs() < 4.0,
                "a heavy hand on clean material: {band:?}"
            );
        }
    }

    #[test]
    fn the_band_budget_is_respected() {
        // Several resonances, more than the budget allows.
        let mut samples = material(None);
        for (freq, q) in [
            (200.0, 3.0),
            (700.0, 3.0),
            (1_500.0, 3.0),
            (3_000.0, 3.0),
            (6_000.0, 3.0),
            (9_000.0, 3.0),
        ] {
            Biquad::peaking(f64::from(SR), freq, 10.0, q).apply_in_place(&mut samples);
        }
        let speech = ltas_of(&samples);
        for max_bands in [1usize, 3, 5] {
            let fit = fit_corrective(
                &speech,
                None,
                SR,
                &EqFitOptions {
                    max_bands,
                    ..EqFitOptions::default()
                },
            );
            assert!(fit.bands.len() <= max_bands, "{} bands", fit.bands.len());
        }
    }

    #[test]
    fn gains_stay_inside_the_cut_and_boost_limits() {
        // Cuts get the full range and boosts half of it, so a fit against a
        // very peaky spectrum must still respect the asymmetry.
        let mut samples = material(None);
        Biquad::peaking(f64::from(SR), 1_000.0, 20.0, 4.0).apply_in_place(&mut samples);
        Biquad::peaking(f64::from(SR), 3_000.0, -20.0, 4.0).apply_in_place(&mut samples);
        let speech = ltas_of(&samples);

        let options = EqFitOptions::default();
        // Give the noise floor a clean bill of health, so boosts are allowed
        // and the limit rather than the gate is what is being tested.
        let quiet = ltas::smooth_to_log_grid(
            speech.psd.iter().map(|p| p * 1e-6).collect(),
            SR,
            &LtasOptions::default(),
            speech.frames,
        );
        let fit = fit_corrective(&speech, Some(&quiet), SR, &options);

        for band in &fit.bands {
            assert!(
                band.gain_db >= -options.max_cut_db - 1e-9
                    && band.gain_db <= options.max_boost_db + 1e-9,
                "{band:?}"
            );
        }
        assert!(fit.bands.iter().any(|b| b.gain_db < 0.0), "no cut placed");
    }

    #[test]
    fn a_boost_is_refused_where_the_noise_floor_is_close() {
        // The same notch, gated two ways. With no noise LTAS, boosting is
        // forbidden outright; with a quiet one, it is allowed.
        let mut samples = material(None);
        Biquad::peaking(f64::from(SR), 3_000.0, -14.0, 3.0).apply_in_place(&mut samples);
        let speech = ltas_of(&samples);

        let forbidden = fit_corrective(&speech, None, SR, &EqFitOptions::default());
        assert!(
            forbidden.bands.iter().all(|b| b.gain_db <= 0.0),
            "{:?}",
            forbidden.bands
        );

        let quiet = ltas::smooth_to_log_grid(
            speech.psd.iter().map(|p| p * 1e-6).collect(),
            SR,
            &LtasOptions::default(),
            speech.frames,
        );
        let allowed = fit_corrective(&speech, Some(&quiet), SR, &EqFitOptions::default());
        assert!(
            allowed.bands.iter().any(|b| b.gain_db > 0.0),
            "a clean floor should permit a boost: {:?}",
            allowed.bands
        );
    }

    #[test]
    fn a_declined_notch_does_not_consume_the_band_budget() {
        // Notches the fitter may not fill, then a resonance it should cut. If a
        // declined notch spent a band, the resonance would go uncorrected.
        let mut samples = material(None);
        for freq in [250.0, 400.0, 600.0, 900.0] {
            Biquad::peaking(f64::from(SR), freq, -14.0, 4.0).apply_in_place(&mut samples);
        }
        Biquad::peaking(f64::from(SR), 5_000.0, 12.0, 3.0).apply_in_place(&mut samples);
        let speech = ltas_of(&samples);

        let fit = fit_corrective(
            &speech,
            None,
            SR,
            &EqFitOptions {
                max_bands: 2,
                ..EqFitOptions::default()
            },
        );
        assert!(
            fit.bands
                .iter()
                .any(|b| (b.freq / 5_000.0).log2().abs() < 0.4),
            "the resonance was never reached: {:?}",
            fit.bands
        );
    }

    #[test]
    fn nothing_is_fitted_outside_the_trusted_range() {
        let mut samples = material(None);
        // A resonance at 40 Hz, below the 80 Hz floor of the fit range.
        Biquad::peaking(f64::from(SR), 40.0, 16.0, 2.0).apply_in_place(&mut samples);
        let speech = ltas_of(&samples);
        let fit = fit_corrective(&speech, None, SR, &EqFitOptions::default());
        for band in &fit.bands {
            assert!(band.freq >= 80.0 && band.freq <= 12_000.0, "{band:?}");
        }
    }

    #[test]
    fn a_bump_wider_than_the_smoothing_is_read_as_tilt_and_left_alone() {
        // The broad target is the same spectrum smoothed over 1.5 octaves, so
        // anything broader than that is not a deviation from it — it is the
        // voice's character, and correcting it would be imposing a timbre.
        let fit = fit_corrective(
            &ltas_of(&material(Some((1_000.0, 10.0, 0.4)))),
            None,
            SR,
            &EqFitOptions::default(),
        );
        let near_the_bump = fit
            .bands
            .iter()
            .filter(|b| (b.freq / 1_000.0).log2().abs() < 0.5)
            .count();
        assert_eq!(near_the_bump, 0, "{:?}", fit.bands);
    }

    #[test]
    fn a_wider_deviation_gets_a_lower_q() {
        // Tested directly on the estimator, against deviation curves of known
        // width. Going through a real fit does not work: after the broad target
        // is subtracted, what is left of a stamped resonance is narrow at the
        // twelve-points-per-octave grid whatever its original Q, and every
        // estimate saturates at the clamp.
        let options = EqFitOptions {
            min_q: 0.1,
            max_q: 100.0,
            ..EqFitOptions::default()
        };
        // A log-spaced grid, and a Gaussian bump on it of `octaves` width.
        let freqs: Vec<f64> = (0..97)
            .map(|i| 100.0 * 2f64.powf(i as f64 / 12.0))
            .collect();
        let peak = 48; // 1600 Hz, in the middle
        let bump = |octaves: f64| -> Vec<f64> {
            freqs
                .iter()
                .map(|f| {
                    let d = (f / freqs[peak]).log2() / octaves;
                    6.0 * (-d * d).exp()
                })
                .collect()
        };

        let wide = estimate_q(&bump(1.0), &freqs, peak, &options);
        let narrow = estimate_q(&bump(0.2), &freqs, peak, &options);
        assert!(wide < narrow, "wide Q {wide}, narrow Q {narrow}");
    }

    #[test]
    fn every_q_stays_inside_its_clamp() {
        let options = EqFitOptions::default();
        for q in [0.3, 1.0, 12.0] {
            let fit = fit_corrective(
                &ltas_of(&material(Some((1_000.0, 12.0, q)))),
                None,
                SR,
                &options,
            );
            for band in &fit.bands {
                assert!(
                    band.q >= options.min_q && band.q <= options.max_q,
                    "{band:?}"
                );
            }
        }
    }

    #[test]
    fn a_rumble_filter_is_placed_only_when_there_is_rumble() {
        let clean = ltas_of(&material(None));
        assert_eq!(
            decide_rumble(&clean).freq,
            None,
            "{:?}",
            decide_rumble(&clean)
        );

        // Add a 35 Hz tone at the level a lorry outside would be.
        let mut rumbly = material(None);
        for (i, sample) in rumbly.iter_mut().enumerate() {
            *sample += (0.2 * (TAU * 35.0 * i as f64 / f64::from(SR)).sin()) as f32;
        }
        let decision = decide_rumble(&ltas_of(&rumbly));
        assert_eq!(decision.freq, Some(80.0), "{decision:?}");
        assert!(decision.excess_db > -12.0);
    }

    #[test]
    fn the_cascade_carries_the_rumble_filter_and_every_band() {
        let speech = ltas_of(&material(Some((1_200.0, 10.0, 2.0))));
        let fit = fit_corrective(&speech, None, SR, &EqFitOptions::default());

        let without = build_cascade(
            &fit,
            &RumbleDecision {
                freq: None,
                excess_db: -30.0,
            },
            SR,
        );
        assert_eq!(without.len(), fit.bands.len());

        let with = build_cascade(
            &fit,
            &RumbleDecision {
                freq: Some(80.0),
                excess_db: -5.0,
            },
            SR,
        );
        assert_eq!(with.len(), fit.bands.len() + 1);
        // The high-pass is first, so the bands see a signal with the rumble
        // already gone rather than fitting around it.
        assert!(with[0].magnitude_db(20.0, f64::from(SR)) < -20.0);
    }

    #[test]
    fn applying_the_cascade_flattens_the_resonance_it_was_fitted_to() {
        // End to end: fit, build, apply, measure again.
        let samples = material(Some((1_200.0, 10.0, 2.0)));
        let speech = ltas_of(&samples);
        let fit = fit_corrective(&speech, None, SR, &EqFitOptions::default());
        let cascade = build_cascade(
            &fit,
            &RumbleDecision {
                freq: None,
                excess_db: -30.0,
            },
            SR,
        );

        let corrected = ltas_of(&crate::apply_cascade(&samples, &cascade));

        // Measured the way the fitter measures: the largest departure from the
        // spectrum's own broad shape, inside the range it is allowed to work.
        let unevenness = |ltas: &Ltas| -> f64 {
            let target = ltas.broad_target(1.5);
            ltas.freqs
                .iter()
                .enumerate()
                .filter(|(_, f)| **f >= 80.0 && **f <= 12_000.0)
                .map(|(i, _)| (ltas.db[i] - target[i]).abs())
                .fold(0.0, f64::max)
        };
        let before = unevenness(&speech);
        let after = unevenness(&corrected);
        assert!(after < before - 1.0, "{before} dB then {after} dB");
    }
}
