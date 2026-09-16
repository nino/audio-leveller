//! De-clicker: find impulsive damage and rebuild it from its surroundings.
//!
//! Detection runs on the *linear-prediction residual*. A short window of speech
//! is well described by an AR model (see [`crate::lpc`]), so a click — which
//! owes nothing to the samples around it — leaves a spike in the residual far
//! larger than anything speech produces.
//!
//! The awkward part of that idea, and the reason naive AR de-clickers smear
//! damage across a whole model order, is that one corrupt sample at m pollutes
//! the forward residual for the next p samples: the predictor keeps feeding on
//! the bad value. Thresholding the forward residual alone therefore flags
//! `[m, m+p]` and "repairs" p samples of perfectly good audio.
//!
//! So this detector requires the *backward* residual to agree. Running the same
//! model in reverse, a corrupt sample at m pollutes `[m−p, m]`. Where both are
//! large is exactly where the damage is:
//!
//! ```text
//! forward large:   [m1,     m2 + p]
//! backward large:  [m1 - p, m2    ]
//! both:            [m1,     m2    ]   <- the actual burst
//! ```
//!
//! The threshold comes from the median absolute deviation of the residual
//! rather than its standard deviation, because the clicks themselves would
//! inflate a standard deviation and hide behind it.
//!
//! Repair is least-squares AR interpolation, which reconstructs the resonance
//! that was there rather than bridging or fading across the hole.

use crate::lpc::{ArModel, interpolate_gap};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct DeclickOptions {
    /// AR model order. Around 32 captures speech formants at 44.1–48 kHz.
    pub order: usize,
    /// Analysis block length. The model is refitted and the threshold
    /// re-estimated per block.
    ///
    /// Short matters more than it looks. Voiced speech is driven by a glottal
    /// pulse every pitch period — impulsive excitation, five to fifteen
    /// milliseconds apart, which is exactly what an impulsive-outlier detector
    /// fires on. Over a 50 ms block those pulses are a minority of the samples
    /// and sit above the median, so the threshold lets them through as "clicks"
    /// and the repair replaces real glottal pulses with interpolated mush. Over
    /// a block comparable to one pitch period they dominate their own
    /// statistics and lift the threshold above themselves. Measured on the
    /// corpus: 50 ms blocks give ~2 false positives per second of clean speech,
    /// 10 ms blocks give none, and both still catch every injected click.
    pub block_sec: f64,
    /// Detection threshold, in robust standard deviations of the residual.
    pub threshold_sigma: f64,
    /// Longest burst to treat as a click. Longer events are more likely to be
    /// real transients, and are left alone.
    pub max_burst_sec: f64,
    /// Bursts closer together than this are merged into one repair.
    pub merge_gap_samples: usize,
    /// Widen each burst by this much before repairing.
    ///
    /// Real clicks decay: the first sample or two tower over the threshold and
    /// the tail ducks under it, so repairing only the detected run leaves the
    /// tail behind. Two samples covers the tail of a typical short click; the
    /// cost is four extra interpolated samples per burst, in material the model
    /// fits well.
    pub dilate_samples: usize,
    /// Discard a whole block's detections when they cover more than this
    /// fraction of it.
    ///
    /// Clicks are rare by definition — a block where the residual is over
    /// threshold this often is a transient the model does not fit, a door slam
    /// or dense crackle, and interpolating chunks of it would rewrite real
    /// audio.
    pub max_block_density: f64,
    /// Abort the whole stage if detection covers more than this fraction of the
    /// file. That means the threshold is wrong for this material, and doing
    /// nothing is much safer than rewriting it.
    pub max_repair_fraction: f64,
    /// Pulse-train veto: how large a neighbouring residual peak has to be,
    /// against a candidate's own, for the candidate to be a glottal pulse.
    ///
    /// Voiced speech is excited by a glottal pulse every pitch period, and in
    /// the residual those pulses *are* impulsive outliers — a robust per-block
    /// threshold sits between them and flags them. On real speech that comes to
    /// roughly twenty "clicks" a second, every one a repair replacing a real
    /// pulse with interpolated mush. Short blocks reduce this and do not cure
    /// it.
    ///
    /// What separates a click from a pulse is not its size against the residual
    /// floor but its size against the *neighbouring pulses*: a pulse has
    /// comparable neighbours one pitch period away, a click towers over them.
    /// So a candidate is vetoed when the residual between
    /// [`pulse_lag_min_sec`](Self::pulse_lag_min_sec) and
    /// [`pulse_lag_max_sec`](Self::pulse_lag_max_sec) on either side — one to
    /// two pitch periods, 50–400 Hz — reaches this fraction of its own peak. At
    /// 0.3 a click has to be about 10 dB more impulsive than the voice's own
    /// excitation around it, and one that is not is masked by that excitation
    /// anyway.
    ///
    /// Measured on a real 44.1 kHz podcast recording, clean and with no clicks:
    /// the detector alone repaired ~780 "clicks" per minute of speech, all of
    /// them glottal pulses; with the veto, ~20. Injected clicks at 0.3× the
    /// waveform peak were still caught 28 times out of 30, at 0.5× 30 out of
    /// 30. Judge this on real recordings — the synthetic corpus voice is more
    /// impulsive than a real one even after its glottal return phase was added,
    /// so the margin there is thinner than it looks.
    pub pulse_veto_ratio: f64,
    pub pulse_lag_min_sec: f64,
    pub pulse_lag_max_sec: f64,
}

impl Default for DeclickOptions {
    fn default() -> Self {
        Self {
            order: 32,
            block_sec: 0.01,
            threshold_sigma: 6.0,
            max_burst_sec: 0.002,
            merge_gap_samples: 4,
            dilate_samples: 2,
            max_block_density: 0.05,
            max_repair_fraction: 0.02,
            pulse_veto_ratio: 0.3,
            pulse_lag_min_sec: 0.0025,
            pulse_lag_max_sec: 0.02,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ClickBurst {
    pub channel: usize,
    pub start: usize,
    pub end: usize,
    /// How far above the threshold the worst sample sat, in robust sigmas.
    pub peak_sigma: f64,
    pub repaired: bool,
}

#[derive(Clone, Debug)]
pub struct DeclickResult {
    pub channels: Vec<Vec<f32>>,
    pub bursts: Vec<ClickBurst>,
    /// Bursts that were detected and successfully rebuilt.
    pub repaired: usize,
    pub samples_repaired: usize,
    pub repaired_fraction: f64,
    /// True when detection covered more than
    /// [`DeclickOptions::max_repair_fraction`] and the stage declined to touch
    /// the audio at all.
    pub aborted: bool,
    /// Candidates dropped by the pulse-train veto.
    pub vetoed: usize,
}

/// Residual magnitude below which a block has no usable scale.
const MIN_SIGMA: f64 = 1e-7;

#[derive(Clone, Copy, Debug)]
struct Detection {
    start: usize,
    end: usize,
    peak_sigma: f64,
    /// Absolute peak of the forward residual over the burst.
    peak_abs: f64,
    /// Which analysis block this burst sits in, so repair can reuse the model
    /// that did the detecting.
    block: usize,
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    }
}

struct Detected {
    detections: Vec<Detection>,
    models: Vec<ArModel>,
    vetoed: usize,
}

/// Find click bursts in one channel.
fn detect(samples: &[f32], sample_rate: u32, options: &DeclickOptions) -> Detected {
    let order = options.order;
    let block_samples = (options.block_sec * f64::from(sample_rate)).round() as usize;
    let block_samples = block_samples.max(order * 4).max(1);
    let max_burst = ((options.max_burst_sec * f64::from(sample_rate)).round() as usize).max(1);
    let length = samples.len();

    let mut models: Vec<ArModel> = Vec::new();
    let mut detections: Vec<Detection> = Vec::new();

    // Coarse envelope of the forward residual — the maximum per ~1 ms bin —
    // plus which block each bin came from, both for the pulse-train veto.
    let bin_samples = ((0.001 * f64::from(sample_rate)).round() as usize).max(1);
    let mut envelope = vec![0.0f64; length.div_ceil(bin_samples)];
    let mut envelope_block = vec![usize::MAX; envelope.len()];

    for (block, block_start) in (0..length).step_by(block_samples).enumerate() {
        let block_end = (block_start + block_samples).min(length);

        // Fit on the block plus a margin, so the model is not starved at the
        // edges.
        models.push(ArModel::fit(
            samples,
            block_start.saturating_sub(order)..(block_end + order).min(length),
            order,
        ));
        let model = models.last().expect("just pushed");

        // The residuals need `order` samples of context on each side, so the
        // first and last `order` samples cannot be examined. Clicks landing
        // there are rare, and repairing them without context is guesswork.
        let from = block_start.max(order);
        let to = block_end.min(length.saturating_sub(order));
        if to <= from {
            continue;
        }

        let a = model.coefficients();
        let mut forward = Vec::with_capacity(to - from);
        let mut backward = Vec::with_capacity(to - from);
        for n in from..to {
            let mut f = 0.0;
            let mut b = 0.0;
            for (k, coefficient) in a.iter().enumerate() {
                f += coefficient * f64::from(samples[n - k]);
                b += coefficient * f64::from(samples[n + k]);
            }
            forward.push(f);
            backward.push(b);

            let bin = n / bin_samples;
            if f.abs() > envelope[bin] {
                envelope[bin] = f.abs();
                envelope_block[bin] = block;
            }
        }

        // Robust scale from the forward residual: median absolute deviation,
        // not standard deviation, which the clicks would inflate and hide in.
        let mut magnitudes: Vec<f64> = forward.iter().map(|f| f.abs()).collect();
        let sigma = 1.4826 * median(&mut magnitudes);
        if sigma < MIN_SIGMA {
            continue;
        }
        let threshold = options.threshold_sigma * sigma;

        // Where forward and backward residuals *both* exceed the threshold.
        let mut block_detections: Vec<Detection> = Vec::new();
        let mut run: Option<(usize, f64)> = None;
        for i in 0..=forward.len() {
            let hit =
                i < forward.len() && forward[i].abs() > threshold && backward[i].abs() > threshold;

            match (hit, run) {
                (true, None) => run = Some((i, forward[i].abs())),
                (true, Some((start, peak))) => run = Some((start, peak.max(forward[i].abs()))),
                (false, Some((run_start, peak))) => {
                    let (start, end) = (from + run_start, from + i);
                    // Long events are more likely to be real transients than
                    // damage.
                    if end - start <= max_burst {
                        block_detections.push(Detection {
                            start,
                            end,
                            peak_sigma: peak / sigma,
                            peak_abs: peak,
                            block,
                        });
                    }
                    run = None;
                }
                (false, None) => {}
            }
        }

        // Density guard: clicks are rare. A block peppered with over-threshold
        // runs is a transient the model does not fit, not a cluster of clicks —
        // drop all of it rather than interpolating chunks of real audio.
        let flagged: usize = block_detections.iter().map(|d| d.end - d.start).sum();
        if flagged as f64 <= options.max_block_density * (to - from) as f64 {
            detections.extend(block_detections);
        }
    }

    let (detections, vetoed) = apply_pulse_veto(
        detections,
        &envelope,
        &envelope_block,
        bin_samples,
        sample_rate,
        options,
    );

    Detected {
        detections,
        models,
        vetoed,
    }
}

/// Drop candidates that have a comparable residual peak one pitch period away —
/// those are glottal pulses, not clicks.
///
/// Two sources of "neighbour", because each alone has a blind spot. Other
/// candidates, which glottal pulses are, but only the cycles that crossed the
/// threshold. And the raw residual envelope, which sees every cycle — but a
/// click corrupts the AR model of its own block and inflates that block's
/// residual everywhere, so bins from the candidate's own block are ignored,
/// otherwise the click hides behind its own pollution.
fn apply_pulse_veto(
    detections: Vec<Detection>,
    envelope: &[f64],
    envelope_block: &[usize],
    bin_samples: usize,
    sample_rate: u32,
    options: &DeclickOptions,
) -> (Vec<Detection>, usize) {
    let lag_min = (options.pulse_lag_min_sec * f64::from(sample_rate)).round() as usize;
    let lag_max = (options.pulse_lag_max_sec * f64::from(sample_rate)).round() as usize;

    let envelope_max = |from: usize, to: usize, own_block: usize| -> f64 {
        let b0 = from.div_ceil(bin_samples);
        let b1 = (to / bin_samples).min(envelope.len());
        (b0..b1)
            .filter(|b| envelope_block[*b] != own_block)
            .map(|b| envelope[b])
            .fold(0.0, f64::max)
    };

    let mut kept = Vec::with_capacity(detections.len());
    let mut vetoed = 0usize;
    let mut lo = 0usize;

    for i in 0..detections.len() {
        let d = detections[i];
        let bar = options.pulse_veto_ratio * d.peak_abs;

        let mut comparable = envelope_max(
            d.start.saturating_sub(lag_max),
            d.start.saturating_sub(lag_min),
            d.block,
        ) >= bar
            || envelope_max(d.end + lag_min, d.end + lag_max, d.block) >= bar;

        while lo < i && detections[lo].end + lag_max < d.start {
            lo += 1;
        }
        for (j, o) in detections.iter().enumerate().skip(lo) {
            if comparable {
                break;
            }
            if j == i {
                continue;
            }
            if o.start > d.end + lag_max {
                break;
            }
            let gap = if o.start >= d.end {
                o.start - d.end
            } else {
                d.start.saturating_sub(o.end)
            };
            if gap >= lag_min && gap <= lag_max && o.peak_abs >= bar {
                comparable = true;
            }
        }

        if comparable {
            vetoed += 1;
        } else {
            kept.push(d);
        }
    }

    (kept, vetoed)
}

/// Merge detections close enough to be one event, then drop merged events too
/// long to be clicks.
///
/// The order matters. Merging *with* a length cap would carve a long dense
/// event — a door slam, a burst of crackle — into many maximum-length "clicks"
/// and repair them all, rewriting something that was never a click. Merging
/// first lets the event show its real extent, and the length filter then
/// discards it whole.
fn merge(detections: &[Detection], gap: usize, max_burst: usize) -> Vec<Detection> {
    let mut merged: Vec<Detection> = Vec::new();
    for current in detections {
        match merged.last_mut() {
            Some(last) if current.start.saturating_sub(last.end) <= gap => {
                last.end = current.end;
                last.peak_sigma = last.peak_sigma.max(current.peak_sigma);
            }
            _ => merged.push(*current),
        }
    }
    merged.retain(|d| d.end - d.start <= max_burst);
    merged
}

/// Detect and repair clicks across every channel.
///
/// Channels are handled independently: damage often hits one side only, and
/// repairing the intact channel to match would be inventing signal.
pub fn declick(channels: &[Vec<f32>], sample_rate: u32, options: &DeclickOptions) -> DeclickResult {
    let max_burst = ((options.max_burst_sec * f64::from(sample_rate)).round() as usize).max(1);
    let length = channels.first().map_or(0, Vec::len);

    let found: Vec<(Vec<Detection>, Vec<ArModel>, usize)> = channels
        .iter()
        .map(|samples| {
            let detected = detect(samples, sample_rate, options);
            let merged = merge(&detected.detections, options.merge_gap_samples, max_burst);
            (merged, detected.models, detected.vetoed)
        })
        .collect();

    let vetoed: usize = found.iter().map(|(_, _, v)| v).sum();
    let detected_samples: usize = found
        .iter()
        .flat_map(|(d, _, _)| d.iter())
        .map(|d| d.end - d.start)
        .sum();

    let total_samples = length * channels.len().max(1);
    let detected_fraction = if total_samples > 0 {
        detected_samples as f64 / total_samples as f64
    } else {
        0.0
    };

    // Too much of the file flagged means the threshold is wrong for this
    // material. Rewriting 2% of every channel on a bad hypothesis is far worse
    // than leaving the clicks in, so decline, and say so.
    if detected_fraction > options.max_repair_fraction {
        return DeclickResult {
            channels: channels.to_vec(),
            bursts: Vec::new(),
            repaired: 0,
            samples_repaired: 0,
            repaired_fraction: 0.0,
            aborted: true,
            vetoed,
        };
    }

    let mut out = channels.to_vec();
    let mut bursts = Vec::new();
    let mut repaired = 0usize;
    let mut samples_repaired = 0usize;
    let fit_half = ((options.block_sec * f64::from(sample_rate) / 2.0).round() as usize)
        .max(options.order * 4);

    for (channel, (detections, models, _)) in found.iter().enumerate() {
        let samples = &mut out[channel];
        for detection in detections {
            let start = detection.start.saturating_sub(options.dilate_samples);
            let end = (detection.end + options.dilate_samples).min(length);
            let Some(model) = models.get(detection.block).or_else(|| models.last()) else {
                continue;
            };

            let mut ok = interpolate_gap(samples, start..end, model);

            // Refinement pass. The first repair used the model that did the
            // detecting, fitted on a block *containing the click* — and in
            // quiet audio the click dominates that block's autocorrelation
            // utterly, since three samples at click amplitude carry orders of
            // magnitude more energy than 50 ms of noise floor. So the model
            // describes the click rather than the audio, and the repair
            // inherits the error. With the click now gone, refit on the
            // repaired neighbourhood and interpolate again against a model of
            // the actual signal.
            if ok {
                let refit = ArModel::fit(
                    samples,
                    start.saturating_sub(fit_half)..(end + fit_half).min(length),
                    options.order,
                );
                ok = interpolate_gap(samples, start..end, &refit) || ok;
                repaired += 1;
                samples_repaired += end - start;
            }

            bursts.push(ClickBurst {
                channel,
                start,
                end,
                peak_sigma: detection.peak_sigma,
                repaired: ok,
            });
        }
    }

    DeclickResult {
        channels: out,
        bursts,
        repaired,
        samples_repaired,
        repaired_fraction: if total_samples > 0 {
            samples_repaired as f64 / total_samples as f64
        } else {
            0.0
        },
        aborted: false,
        vetoed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{ClickOptions, Rng, SpeechOptions, Spurt, add_clicks, synthetic_speech};

    const SR: u32 = 48_000;

    fn speech() -> leveller_corpus::Speech {
        synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(3.0, -23.0).at_pitch(110.0)],
            ..SpeechOptions::default()
        })
    }

    /// Setting the veto's bar out of reach turns it off: nothing can be
    /// "comparable" to a candidate, so nothing is vetoed. (Setting it to zero
    /// does the opposite — everything is comparable and everything is vetoed.)
    const VETO_OFF: f64 = 1e9;

    fn covered(bursts: &[ClickBurst], positions: &[usize]) -> usize {
        positions
            .iter()
            .filter(|p| bursts.iter().any(|b| **p + 4 >= b.start && **p < b.end + 4))
            .count()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    }

    fn worst_change(a: &[f32], b: &[f32]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| f64::from((x - y).abs()))
            .fold(0.0, f64::max)
    }

    /// A bare pulse train through a resonator: the most impulsive "voice"
    /// possible, every pulse a textbook outlier in the residual. Without the
    /// veto the detector repairs all of them.
    fn pulse_train(seconds: f64, f0: f64) -> Vec<f32> {
        let n = (seconds * f64::from(SR)) as usize;
        let period = f64::from(SR) / f0;
        let mut rng = Rng::new(7);
        let mut out = vec![0.0f32; n];

        let mut next = 100.0f64;
        for (i, sample) in out.iter_mut().enumerate() {
            // A little aspiration, so the residual has a scale to threshold
            // against — an exactly-zero floor reads as "nothing to measure".
            *sample = ((rng.next_f64() - 0.5) * 0.03) as f32;
            if i as f64 >= next {
                *sample += 1.0;
                next += period * (1.0 + 0.02 * (i as f64 / 700.0).sin());
            }
        }

        // Two-pole resonator, roughly a formant at 700 Hz.
        let r = 0.985;
        let w = std::f64::consts::TAU * 700.0 / f64::from(SR);
        let (mut y1, mut y2) = (0.0f64, 0.0f64);
        for sample in &mut out {
            let y = f64::from(*sample) + 2.0 * r * w.cos() * y1 - r * r * y2;
            *sample = (y * 0.05) as f32;
            y2 = y1;
            y1 = y;
        }
        out
    }

    #[test]
    fn essentially_every_injected_click_is_found() {
        // Twice the waveform peak and 50 ms apart, as in the corpus. The
        // synthetic voice's excitation is far more impulsive than a real one,
        // so only clicks this size stand out from it the way real clicks stand
        // out from a real voice; the veto deliberately leaves anything within
        // ~10 dB of the surrounding excitation alone.
        let clicked = add_clicks(&speech().signal, &ClickOptions::default());
        let result = declick(clicked.signal.channels(), SR, &DeclickOptions::default());

        assert!(!result.aborted);
        // A couple may land in the unexaminable head or tail context region.
        assert!(
            covered(&result.bursts, &clicked.positions) >= clicked.positions.len() - 2,
            "found {} of {}",
            covered(&result.bursts, &clicked.positions),
            clicked.positions.len()
        );
    }

    #[test]
    fn a_burst_is_localised_rather_than_smeared_over_the_model_order() {
        // The whole point of requiring the forward and backward residuals to
        // agree: a forward-only detector would flag ~32 samples per click.
        let clicked = add_clicks(
            &speech().signal,
            &ClickOptions {
                count: 20,
                relative_amplitude: 0.6,
                min_gap_sec: 0.0,
                seed: 5,
                ..ClickOptions::default()
            },
        );
        let result = declick(clicked.signal.channels(), SR, &DeclickOptions::default());

        for burst in &result.bursts {
            // 3-sample clicks dilated by 2 each side, so past ~12 samples means
            // the detector is smearing.
            assert!(burst.end - burst.start < 12, "{burst:?}");
        }
        assert!(result.samples_repaired < DeclickOptions::default().order * 20);
    }

    #[test]
    fn clean_speech_is_left_essentially_untouched() {
        // The transparency test. A de-clicker that mangles clean audio is worse
        // than no de-clicker, because most material has no clicks at all.
        let speech = speech();
        let result = declick(speech.signal.channels(), SR, &DeclickOptions::default());

        let changed = worst_change(speech.signal.channel(0), &result.channels[0]);
        let level = rms(speech.signal.channel(0));
        let relative = 20.0 * (changed.max(1e-12) / level).log10();
        assert!(
            relative < -30.0,
            "worst change {relative} dB below programme"
        );
        assert!(
            result.repaired_fraction < 0.001,
            "{}",
            result.repaired_fraction
        );
    }

    #[test]
    fn a_pure_tone_has_nothing_impulsive_in_it() {
        let tone: Vec<f32> = (0..SR as usize * 2)
            .map(|i| {
                (0.5 * (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(SR)).sin()) as f32
            })
            .collect();
        let result = declick(&[tone], SR, &DeclickOptions::default());
        assert_eq!(result.samples_repaired, 0);
    }

    #[test]
    fn white_noise_has_no_outliers_even_though_it_is_all_residual() {
        // The adversarial case: every sample is unpredictable, so the residual
        // is large everywhere. A robust threshold must still see no *outliers*,
        // because nothing stands out from anything else.
        let mut rng = Rng::new(3);
        let noise: Vec<f32> = (0..SR as usize * 2)
            .map(|_| (rng.next_bipolar() * 0.2) as f32)
            .collect();
        let result = declick(&[noise], SR, &DeclickOptions::default());
        assert!(
            result.repaired_fraction < 0.005,
            "{}",
            result.repaired_fraction
        );
    }

    #[test]
    fn an_event_too_long_to_be_a_click_is_not_rewritten() {
        let speech = speech();
        let mut damaged = speech.signal.channel(0).to_vec();
        // A 20 ms burst: a door slam, not a click.
        let start = SR as usize;
        let length = (0.02 * f64::from(SR)) as usize;
        let mut rng = Rng::new(17);
        for sample in &mut damaged[start..start + length] {
            *sample += (rng.next_bipolar() * 0.5) as f32;
        }

        let result = declick(&[damaged], SR, &DeclickOptions::default());
        let inside: usize = result
            .bursts
            .iter()
            .filter(|b| b.start >= start && b.end <= start + length)
            .map(|b| b.end - b.start)
            .sum();
        // It may nibble at the sharp edges; it must not rewrite the event.
        assert!(inside < length / 2, "rewrote {inside} of {length} samples");
    }

    #[test]
    fn a_periodic_pulse_train_is_left_alone_where_the_bare_detector_would_not() {
        let train = pulse_train(2.0, 120.0);
        let bare = declick(
            std::slice::from_ref(&train),
            SR,
            &DeclickOptions {
                pulse_veto_ratio: VETO_OFF,
                ..DeclickOptions::default()
            },
        );
        let vetoed = declick(std::slice::from_ref(&train), SR, &DeclickOptions::default());

        assert!(
            bare.bursts.len() > 50,
            "bare detector found {}",
            bare.bursts.len()
        );
        assert_eq!(
            vetoed.bursts.len(),
            0,
            "{:?}",
            &vetoed.bursts[..3.min(vetoed.bursts.len())]
        );
        // `vetoed` counts raw candidates and `bursts` are post-merge, so this
        // is a lower bound rather than an equality.
        assert!(vetoed.vetoed >= bare.bursts.len());
    }

    #[test]
    fn a_click_that_towers_over_the_pulses_is_still_caught() {
        let train = pulse_train(2.0, 120.0);
        let peak = train.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let at = (1.0137 * f64::from(SR)).round() as usize;

        let mut clicked = train;
        clicked[at] += peak * 1.5;
        clicked[at + 1] -= peak * 1.2;

        let result = declick(&[clicked], SR, &DeclickOptions::default());
        assert_eq!(result.bursts.len(), 1, "{:?}", result.bursts);
        assert!(result.bursts[0].start <= at);
        assert!(result.bursts[0].end > at + 1);
    }

    #[test]
    fn the_detected_click_energy_is_removed() {
        let speech = speech();
        let clicked = add_clicks(
            &speech.signal,
            &ClickOptions {
                count: 25,
                ..ClickOptions::default()
            },
        );
        let result = declick(clicked.signal.channels(), SR, &DeclickOptions::default());

        // At each click site the repair should be far closer to the original
        // than the damage was.
        let scale = f64::from(speech.signal.peak());
        for at in &clicked.positions {
            let original = f64::from(speech.signal.channel(0)[*at]);
            let repaired = f64::from(result.channels[0][*at]);
            assert!(
                (repaired - original).abs() < 0.4 * scale,
                "at {at}: {repaired} against {original}, peak {scale}"
            );
        }
    }

    #[test]
    fn too_much_damage_makes_the_stage_decline_rather_than_rewrite_the_file() {
        // Driven by lowering the limit rather than by raising the damage. With
        // the defaults this guard is nearly unreachable: the per-block density
        // guard and the pulse veto both fire on damage dense enough to cover 2%
        // of a file, and drop it before the whole-file fraction is counted. It
        // is the last line rather than the first, and this is what it does when
        // it is reached.
        let speech = speech();
        let clicked = add_clicks(&speech.signal, &ClickOptions::default());
        let damaged = clicked.signal.channel(0).to_vec();

        let strict = DeclickOptions {
            max_repair_fraction: 1e-9,
            ..DeclickOptions::default()
        };
        let result = declick(std::slice::from_ref(&damaged), SR, &strict);

        assert!(result.aborted);
        assert_eq!(result.repaired, 0);
        assert_eq!(result.repaired_fraction, 0.0);
        assert!(result.bursts.is_empty());
        assert_eq!(result.channels[0], damaged, "an abort must change nothing");
    }

    #[test]
    fn channels_are_repaired_independently() {
        // Damage often hits one side only, and repairing the intact channel to
        // match would be inventing signal.
        let speech = speech();
        let clean = speech.signal.channel(0).to_vec();
        let peak = speech.signal.peak();
        let mut damaged = clean.clone();
        damaged[72_000] += peak * 3.0;
        damaged[72_001] -= peak * 1.8;

        let result = declick(&[damaged, clean.clone()], SR, &DeclickOptions::default());
        let on_the_clean_side = result.bursts.iter().filter(|b| b.channel == 1).count();
        assert_eq!(on_the_clean_side, 0, "{:?}", result.bursts);
        assert_eq!(result.channels[1], clean);
    }

    #[test]
    fn silence_has_no_clicks_in_it() {
        let result = declick(&[vec![0.0f32; 48_000]], SR, &DeclickOptions::default());
        assert!(result.bursts.is_empty());
        assert!(!result.aborted);
        assert!(result.channels[0].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_signal_too_short_to_analyse_is_returned_untouched() {
        let short = vec![0.1f32; 64];
        let result = declick(std::slice::from_ref(&short), SR, &DeclickOptions::default());
        assert_eq!(result.channels[0], short);
        assert!(!result.aborted);
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let result = declick(&[], SR, &DeclickOptions::default());
        assert!(result.channels.is_empty());
        assert_eq!(result.repaired_fraction, 0.0);
    }

    #[test]
    fn a_repaired_burst_is_reported_with_where_and_how_bad() {
        let speech = speech();
        let peak = speech.signal.peak();
        let mut damaged = speech.signal.channel(0).to_vec();
        damaged[72_000] += peak * 3.0;
        damaged[72_001] -= peak * 1.8;

        let result = declick(&[damaged], SR, &DeclickOptions::default());
        let burst = result
            .bursts
            .iter()
            .find(|b| b.start <= 72_000 && b.end > 72_000)
            .expect("the click should be reported");
        assert!(burst.repaired);
        assert!(burst.peak_sigma > 6.0, "peak sigma {}", burst.peak_sigma);
        assert!(result.repaired_fraction > 0.0);
    }
}
