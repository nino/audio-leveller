//! Objective metrics for the evaluation harness.
//!
//! The design rule: every metric must be able to say a stage made things
//! *worse*, not just better. A harness that only measures improvement will
//! happily report that a denoiser which eats consonants is working perfectly.
//!
//! The one to watch as the chain grows is `snrGainDb` — programme-to-floor
//! distance, in minus out. A pure gain change (what the leveller does) moves
//! the programme and the floor together and must leave it at 0. A denoiser has
//! to move it positive. Anything that moves it negative is amplifying noise.

use std::collections::BTreeMap;
use std::sync::Arc;

use leveller_corpus::SpeechSegment;
use leveller_dsp::{Signal, Weighted, ltas, reverbtime, truepeak};
use leveller_pipeline::Analyzer;

/// Metric keys are free-form so stages can add their own; values are always
/// dB-ish.
pub type Metrics = BTreeMap<String, f64>;

/// Channels laid end to end, for whole-signal correlations.
fn flatten(signal: &Signal) -> Vec<f32> {
    signal.channels().concat()
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

fn rms(a: &[f32]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    (dot(a, a) / a.len() as f64).sqrt()
}

/// Scale-invariant signal-to-distortion ratio, in dB.
///
/// Scale-invariant is the point: the leveller changes the overall gain by
/// design, and a metric that punished it for that would be measuring the wrong
/// thing. The reference is projected onto the estimate first, so only the
/// *shape* of the difference counts.
pub fn si_sdr_db(reference: &Signal, estimate: &Signal) -> f64 {
    let s = flatten(reference);
    let x = flatten(estimate);
    let energy = dot(&s, &s);
    if energy == 0.0 {
        return f64::NEG_INFINITY;
    }

    let alpha = dot(&x, &s) / energy;
    let mut target_energy = 0.0;
    let mut noise_energy = 0.0;
    for (s, x) in s.iter().zip(&x) {
        let target = alpha * f64::from(*s);
        target_energy += target * target;
        let error = f64::from(*x) - target;
        noise_energy += error * error;
    }

    if noise_energy == 0.0 {
        return f64::INFINITY;
    }
    if target_energy == 0.0 {
        return f64::NEG_INFINITY;
    }
    10.0 * (target_energy / noise_energy).log10()
}

/// The gain-aligned difference between two signals — what a stage actually
/// changed, with any overall level change divided out.
fn residual(reference: &Signal, estimate: &Signal) -> Vec<f32> {
    let s = flatten(reference);
    let x = flatten(estimate);
    let energy = dot(&s, &s);
    let alpha = if energy == 0.0 {
        1.0
    } else {
        dot(&x, &s) / energy
    };

    s.iter()
        .zip(&x)
        .map(|(s, x)| (f64::from(*x) - alpha * f64::from(*s)) as f32)
        .collect()
}

/// How audible the worst surviving click is, in dB. Lower is better; anything
/// below about −6 dB sits under the signal around it and cannot be heard as a
/// click.
///
/// The residual peak at each click site is compared against the *local peak* of
/// the reference (±10 ms), because that is how click audibility works: a spike
/// is a click when it pokes above what is already there. Two earlier versions
/// of this metric were wrong in instructive ways:
///
/// - Normalising by whole-file RMS punished good repairs in loud speech (global
///   RMS is dragged down by the pauses) and forgave bad ones in silence.
/// - Normalising by local RMS still had a false floor: AR interpolation
///   necessarily replaces the *unpredictable* part of the signal (fricative
///   noise, floor noise) with a different realisation, so in a pause the
///   residual is the floor noise itself — and a peak measured against an RMS
///   sits crest-factor above it even when the repair is perfect.
///
/// Peak against peak is like for like. In dead silence the mask is floored at a
/// fraction of the global RMS so the score cannot explode dividing by nothing.
pub fn impulsive_residual_db(
    reference: &Signal,
    estimate: &Signal,
    positions: &[usize],
    width_samples: usize,
) -> f64 {
    if positions.is_empty() {
        return f64::NEG_INFINITY;
    }

    let s = flatten(reference);
    let x = flatten(estimate);
    let energy = dot(&s, &s);
    let alpha = if energy == 0.0 {
        1.0
    } else {
        dot(&x, &s) / energy
    };
    // The residual lives at the estimate's scale and the mask at the
    // reference's; divide the gain back out so the score is scale-invariant.
    let alpha_scale = if alpha.abs() > 1e-12 { alpha.abs() } else { 1.0 };
    let diff = residual(reference, estimate);
    let global = rms(&s);
    if global == 0.0 {
        return f64::NEG_INFINITY;
    }

    let mask_half = (0.01 * f64::from(reference.sample_rate())).round() as usize;
    let guard = 8usize;
    let mut worst = f64::NEG_INFINITY;

    for channel in 0..reference.channel_count() {
        let offset = channel * reference.len();
        for position in positions {
            let at = offset + position;
            let from = at.saturating_sub(guard);
            let to = (at + width_samples + guard).min(diff.len());
            let peak = diff[from..to]
                .iter()
                .fold(0.0f64, |worst, v| worst.max(f64::from(v.abs())))
                / alpha_scale;
            if peak == 0.0 {
                continue;
            }

            // Local peak of the reference around the site, floored against
            // silence.
            let mask_from = at.saturating_sub(mask_half);
            let mask_to = (at + mask_half).min(s.len());
            let local = s[mask_from..mask_to]
                .iter()
                .fold(0.0f64, |worst, v| worst.max(f64::from(v.abs())));
            let mask = local.max(0.02 * global);

            worst = worst.max(20.0 * (peak / mask).log10());
        }
    }

    worst
}

/// Worst per-segment deviation from the target loudness, in LU.
///
/// This is the leveller's actual job, measured on the output over the segment
/// boundaries the generator knows about — not the ones the leveller guessed.
pub fn segment_lufs_error(signal: &Signal, segments: &[SpeechSegment], target_lufs: f64) -> f64 {
    if segments.is_empty() {
        return 0.0;
    }
    let weighted = Weighted::new(signal.channels(), signal.sample_rate());
    let rate = f64::from(signal.sample_rate());

    let mut worst = 0.0f64;
    for segment in segments {
        // Stay clear of the gain ramps that run across the pauses at each end.
        let pad = (0.3 * rate).round() as usize;
        let start = (segment.start + pad).min(segment.end);
        let end = segment.end.saturating_sub(pad).max(start);
        if ((end - start) as f64) < rate * 0.5 {
            continue;
        }

        // Gated integrated loudness, not an ungated mean over the range: that
        // is what a LUFS target means, what the leveller measures to pick its
        // gain, and what the generator normalises to. An ungated mean reads
        // ~2 LU low on syllable-modulated speech, which would show up here as
        // a phantom error.
        let measured = weighted.integrated_over(start, end);
        if !measured.is_finite() {
            continue;
        }
        worst = worst.max((measured - target_lufs).abs());
    }
    worst
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    }
}

/// Signal-to-noise measured *locally*: each speech segment against the noise in
/// the pause leading into it, median across segments.
///
/// This is the transparency metric, and it needs to be local. The whole-file
/// `snrGainDb` moves under the leveller for a legitimate reason — boosting a
/// quiet passage boosts its noise floor too, so the distance between programme
/// and floor genuinely shrinks when segments are pulled together. That is worth
/// reporting but it is not a defect.
///
/// Within one segment the leveller applies a single gain, and the gain ramp has
/// essentially reached that segment's value by the end of the pause before it.
/// So speech-minus-noise measured this way is invariant to levelling, and any
/// movement is a stage genuinely changing the noise relative to the speech —
/// which is exactly what a denoiser must do, and what nothing else may do.
pub fn segment_snr_db(signal: &Signal, segments: &[SpeechSegment]) -> f64 {
    if segments.is_empty() {
        return f64::NAN;
    }
    let rate = f64::from(signal.sample_rate());
    let weighted = Weighted::new(signal.channels(), signal.sample_rate());
    let pad = (0.3 * rate).round() as usize;
    let guard = (0.05 * rate).round() as usize; // stay off the speech onset

    let mut values = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let previous_end = if i == 0 { 0 } else { segments[i - 1].end };

        // A short window at the very end of the pause. It has to be short and
        // it has to be *there*: the leveller ramps gain across the whole
        // silence and only reaches this segment's gain at the end of it, so
        // sampling earlier measures noise at a gain the following speech never
        // gets, which reads as a phantom SNR change proportional to the gain
        // difference.
        let pause_end = segment.start.saturating_sub(guard).max(previous_end);
        let pause_length = pause_end - previous_end;
        if (pause_length as f64) < 0.15 * rate {
            continue;
        }
        let pause_start = pause_end - pause_length.min((0.15 * rate).round() as usize);

        // Ungated: a quiet noise floor fails the −70 LUFS absolute gate
        // outright, and here the quiet part *is* the thing being measured.
        let noise = weighted.loudness_of_range(pause_start, pause_end);

        let start = (segment.start + pad).min(segment.end);
        let end = segment.end.saturating_sub(pad).max(start);
        if ((end - start) as f64) < rate * 0.5 {
            continue;
        }
        let speech = weighted.integrated_over(start, end);

        if noise.is_finite() && speech.is_finite() {
            values.push(speech - noise);
        }
    }

    median(&mut values)
}

/// Worst deviation of the speech spectrum from its own broadly-smoothed shape,
/// in dB — how coloured the material is.
///
/// The corrective EQ exists to bring this down; nothing else in the chain
/// should move it much.
pub fn spectral_deviation_db(signal: &Signal, segments: &[SpeechSegment]) -> f64 {
    let ranges: Vec<std::ops::Range<usize>> =
        segments.iter().map(|s| s.start..s.end).collect();
    let Some(ltas) = ltas::compute(
        signal.channel(0),
        signal.sample_rate(),
        &ranges,
        &ltas::LtasOptions::default(),
    ) else {
        return f64::NAN;
    };

    let target = ltas.broad_target(1.5);
    let mut worst = 0.0f64;
    for (i, freq) in ltas.freqs.iter().enumerate() {
        if *freq < 80.0 || *freq > 12_000.0 {
            continue;
        }
        worst = worst.max((ltas.db[i] - target[i]).abs());
    }
    worst
}

/// How much a chain changed a signal at all: RMS difference, in dB relative to
/// the input.
pub fn change_db(before: &Signal, after: &Signal) -> f64 {
    let a = flatten(before);
    let b = flatten(after);
    let n = a.len().min(b.len());
    let diff: f64 = a[..n]
        .iter()
        .zip(&b[..n])
        .map(|(a, b)| {
            let d = f64::from(*b) - f64::from(*a);
            d * d
        })
        .sum();
    if diff == 0.0 {
        return f64::NEG_INFINITY; // bit-identical
    }
    let level = rms(&a);
    if level > 0.0 {
        10.0 * (diff / n as f64 / (level * level)).log10()
    } else {
        f64::INFINITY
    }
}

/// The clean signal a case was built from, before and after the chain.
pub struct References {
    /// The undegraded signal, scored against the raw input — the "before"
    /// number.
    pub for_input: Arc<Signal>,
    /// The undegraded signal *put through the same chain*, scored against the
    /// output.
    ///
    /// Using the processed reference is what separates "what the degradation
    /// did" from "what the chain was asked to do": the leveller changes segment
    /// levels on purpose, and comparing against a raw reference would score
    /// that intended change as damage.
    pub for_output: Arc<Signal>,
}

#[derive(Default)]
pub struct Inputs<'a> {
    pub input: Option<&'a Arc<Signal>>,
    pub output: Option<&'a Arc<Signal>>,
    /// Present only for cases built from a known-clean source.
    pub reference: Option<&'a References>,
    /// Known speech-segment boundaries from the generator.
    pub segments: &'a [SpeechSegment],
    /// Known click positions, when the case injected any.
    pub click_positions: &'a [usize],
    /// Loudness the chain was asked to hit.
    pub target_lufs: Option<f64>,
}

/// Everything measurable about one case.
///
/// Keys are stable: the runner compares them against a stored baseline, so
/// renaming one is a breaking change.
pub fn compute(inputs: &Inputs) -> Metrics {
    let (Some(input), Some(output)) = (inputs.input, inputs.output) else {
        return Metrics::new();
    };

    let in_analysis = Analyzer::new(input.clone());
    let out_analysis = Analyzer::new(output.clone());

    let in_lufs = in_analysis.integrated_lufs();
    let out_lufs = out_analysis.integrated_lufs();
    let in_floor = in_analysis.default_silence().floor_lufs;
    let out_floor = out_analysis.default_silence().floor_lufs;

    // Whole-file programme-to-floor distance. This is a *diagnostic*, not a
    // transparency check: pulling segments toward a common level legitimately
    // shrinks it, because boosting a quiet passage boosts its noise too. It is
    // the honest measure of what levelling costs in noise, and the reason a
    // denoiser belongs before the leveller in the chain.
    let in_snr = in_lufs - in_floor;
    let out_snr = out_lufs - out_floor;

    let mut metrics = Metrics::new();
    let mut put = |key: &str, value: f64| {
        metrics.insert(key.to_string(), value);
    };

    put("inputLufs", in_lufs);
    put("outputLufs", out_lufs);
    put("inputPeakDbfs", in_analysis.peak_dbfs());
    put("outputPeakDbfs", out_analysis.peak_dbfs());
    put("inputFloorLufs", in_floor);
    put("outputFloorLufs", out_floor);
    put("inputSnrDb", in_snr);
    put("outputSnrDb", out_snr);
    put("snrGainDb", out_snr - in_snr);
    put("changeDb", change_db(input, output));
    // What a converter or a lossy encoder will actually see, as opposed to the
    // highest sample value. The limiter's ceiling is meaningless without it.
    put("inputTruePeakDbfs", truepeak::true_peak_dbfs(input.channels()));
    put(
        "outputTruePeakDbfs",
        truepeak::true_peak_dbfs(output.channels()),
    );

    // Reverberation decay, so a dereverb stage can be judged on what it did.
    let before = reverbtime::reverb_decay(input);
    let after = reverbtime::reverb_decay(output);
    put("inputDecayMs", before.unwrap_or(f64::NAN));
    put("outputDecayMs", after.unwrap_or(f64::NAN));
    put(
        "decayShorteningMs",
        match (before, after) {
            (Some(before), Some(after)) => before - after,
            _ => 0.0,
        },
    );

    if !inputs.segments.is_empty() {
        let in_deviation = spectral_deviation_db(input, inputs.segments);
        let out_deviation = spectral_deviation_db(output, inputs.segments);
        put("inputSpectralDeviationDb", in_deviation);
        put("outputSpectralDeviationDb", out_deviation);
        // Positive means the chain flattened the colouration; negative means it
        // added some.
        put("spectralFlatteningDb", in_deviation - out_deviation);

        // The local version, which levelling leaves alone — see
        // `segment_snr_db`.
        let in_segment = segment_snr_db(input, inputs.segments);
        let out_segment = segment_snr_db(output, inputs.segments);
        put("inputSegmentSnrDb", in_segment);
        put("outputSegmentSnrDb", out_segment);
        put("segmentSnrGainDb", out_segment - in_segment);
    }

    if let Some(target) = inputs.target_lufs {
        put("lufsError", (out_lufs - target).abs());
        if !inputs.segments.is_empty() {
            put(
                "segmentLufsError",
                segment_lufs_error(output, inputs.segments, target),
            );
        }
    }

    if let Some(reference) = inputs.reference {
        let before = si_sdr_db(&reference.for_input, input);
        let after = si_sdr_db(&reference.for_output, output);
        put("inputSiSdrDb", before);
        put("outputSiSdrDb", after);
        put("siSdrGainDb", after - before);

        if !inputs.click_positions.is_empty() {
            put(
                "inputClickResidualDb",
                impulsive_residual_db(&reference.for_input, input, inputs.click_positions, 3),
            );
            put(
                "outputClickResidualDb",
                impulsive_residual_db(&reference.for_output, output, inputs.click_positions, 3),
            );
        }
    }

    metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};

    fn speech() -> leveller_corpus::Speech {
        synthetic_speech(&SpeechOptions {
            sample_rate: 48_000,
            spurts: vec![
                Spurt::new(2.0, -23.0),
                Spurt::new(2.0, -23.0),
                Spurt::new(2.0, -23.0),
            ],
            pause_sec: 1.0,
            floor_dbfs: -70.0,
            seed: 4242,
            channels: 1,
        })
    }

    fn scaled(signal: &Signal, gain: f32) -> Signal {
        let mut out = signal.clone();
        out.scale(gain);
        out
    }

    #[test]
    fn si_sdr_ignores_a_pure_gain_change() {
        // The whole reason it is scale-invariant: the leveller changes the
        // overall gain on purpose, and a metric that punished it would be
        // measuring the wrong thing.
        let signal = speech().signal;
        let louder = scaled(&signal, 3.0);
        let score = si_sdr_db(&signal, &louder);
        // Not infinite: the scaling is done in f32, so each sample is rounded
        // to about one part in 10^7 and that rounding is the only "distortion"
        // there is. 120 dB is far above anything a real stage produces and far
        // below what f32 rounding costs.
        assert!(score > 120.0, "a gain change is not distortion, got {score}");
    }

    #[test]
    fn si_sdr_falls_when_noise_is_added() {
        let signal = speech().signal;
        let noisy = leveller_corpus::add_noise(&signal, 20.0, 991);
        let score = si_sdr_db(&signal, &noisy);
        assert!(
            (10.0..30.0).contains(&score),
            "20 dB of noise should score near 20 dB, got {score}"
        );
    }

    #[test]
    fn change_db_is_negative_infinity_for_an_untouched_signal() {
        // The bypass test in the corpus leans on this: bit-identical has to be
        // distinguishable from very nearly identical.
        let signal = speech().signal;
        assert_eq!(change_db(&signal, &signal.clone()), f64::NEG_INFINITY);
    }

    #[test]
    fn segment_lufs_error_reads_zero_on_material_already_on_target() {
        let speech = speech();
        // The generator normalises each spurt to its stated level, so measuring
        // against that level is measuring the generator — which is the point:
        // a metric that could not read zero on perfect material could never
        // say the leveller had succeeded.
        let error = segment_lufs_error(&speech.signal, &speech.segments, -23.0);
        assert!(error < 1.0, "expected near zero, got {error}");
    }

    #[test]
    fn segment_lufs_error_reads_the_offset_when_the_target_moves() {
        let speech = speech();
        let error = segment_lufs_error(&speech.signal, &speech.segments, -18.0);
        assert!(
            (4.0..6.0).contains(&error),
            "5 LU off target should read about 5, got {error}"
        );
    }

    #[test]
    fn segment_snr_is_unchanged_by_a_gain_applied_to_everything() {
        // This is the property the metric exists for: levelling must not move
        // it, so anything that does move it is a stage changing the noise
        // relative to the speech.
        let speech = speech();
        let before = segment_snr_db(&speech.signal, &speech.segments);
        let after = segment_snr_db(&scaled(&speech.signal, 4.0), &speech.segments);
        assert!(
            (before - after).abs() < 0.01,
            "{before} became {after} under a pure gain"
        );
    }

    #[test]
    fn segment_snr_falls_when_the_floor_rises() {
        let speech = speech();
        let noisy = leveller_corpus::add_noise(&speech.signal, 20.0, 991);
        let before = segment_snr_db(&speech.signal, &speech.segments);
        let after = segment_snr_db(&noisy, &speech.segments);
        assert!(after < before - 10.0, "{before} → {after}");
    }

    #[test]
    fn a_surviving_click_reads_far_above_a_repaired_one() {
        let speech = speech();
        let clicked = leveller_corpus::add_clicks(
            &speech.signal,
            &leveller_corpus::ClickOptions {
                count: 8,
                relative_amplitude: 2.0,
                width_samples: 3,
                min_gap_sec: 0.2,
                seed: 5150,
            },
        );

        let untouched = impulsive_residual_db(
            &speech.signal,
            &clicked.signal,
            &clicked.positions,
            3,
        );
        let repaired = impulsive_residual_db(
            &speech.signal,
            &speech.signal.clone(),
            &clicked.positions,
            3,
        );
        assert!(
            untouched > 10.0,
            "a click nobody repaired should be loudly audible, got {untouched}"
        );
        assert_eq!(repaired, f64::NEG_INFINITY, "a perfect repair leaves nothing");
    }

    #[test]
    fn the_click_score_does_not_move_with_the_overall_gain() {
        // Scale-invariance again: the residual lives at the estimate's scale
        // and the mask at the reference's, so the gain has to be divided out or
        // levelling would look like a de-clicking failure.
        let speech = speech();
        let clicked = leveller_corpus::add_clicks(
            &speech.signal,
            &leveller_corpus::ClickOptions {
                count: 8,
                relative_amplitude: 2.0,
                width_samples: 3,
                min_gap_sec: 0.2,
                seed: 5150,
            },
        );

        let plain = impulsive_residual_db(&speech.signal, &clicked.signal, &clicked.positions, 3);
        let louder = impulsive_residual_db(
            &speech.signal,
            &scaled(&clicked.signal, 4.0),
            &clicked.positions,
            3,
        );
        assert!((plain - louder).abs() < 0.01, "{plain} became {louder}");
    }

    #[test]
    fn spectral_deviation_rises_when_a_resonance_is_added() {
        let speech = speech();
        let flat = spectral_deviation_db(&speech.signal, &speech.segments);

        let cascade = [leveller_dsp::Biquad::peaking(48_000.0, 3200.0, 12.0, 4.0)];
        let mut coloured = speech.signal.clone();
        for channel in coloured.channels_mut() {
            *channel = leveller_dsp::apply_cascade(channel, &cascade);
        }
        let bumpy = spectral_deviation_db(&coloured, &speech.segments);
        assert!(bumpy > flat + 3.0, "{flat} → {bumpy}");
    }
}
