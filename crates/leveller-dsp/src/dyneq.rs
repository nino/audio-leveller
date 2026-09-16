//! Dynamic EQ: suppress resonances only while they are resonating.
//!
//! The static EQ in [`crate::eq`] corrects colouration that is there
//! throughout — a room mode, a microphone's character. What it cannot fix is
//! the kind that comes and goes: a vowel that rings on one note, a sibilant
//! that spikes at 7 kHz, a plosive that booms. Those need a filter that reacts.
//!
//! The rule is the one behind the spectral-smoothing plugins: for each frame,
//! compare the spectrum against a *smoothed version of itself* and pull down
//! whatever protrudes. Anything narrow and loud relative to its own
//! neighbourhood is a resonance almost by definition, while the broad shape —
//! which is the voice — passes through untouched. It needs no threshold in dB
//! against an absolute level, which is what makes it work across voices and
//! levels without tuning.
//!
//! Three things keep it from sounding processed:
//!
//! - **Attack and release across frames.** Gain that changes freely frame to
//!   frame modulates the signal at the frame rate, which is audible as a
//!   flutter. Reacting fast to a rising resonance and slowly to a falling one
//!   is both more natural and less audible.
//! - **A limit on total attenuation**, so a genuinely loud note is tamed rather
//!   than erased.
//! - **Frequency-dependent sensitivity.** Sibilance sits in a known band and is
//!   the most common complaint, so the stage leans harder there. That is the
//!   de-esser: not a separate device, just this one weighted by frequency.

use crate::stft::{Stft, apply_gains, magnitudes};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct DynEqOptions {
    pub frame_size: usize,
    pub hop_size: usize,
    /// Smoothing half-width, in octaves, for the per-frame reference envelope.
    pub smoothing_octaves: f64,
    /// Floor on the smoothing window width, in Hz.
    ///
    /// Without it the reference is too narrow at the bottom to be a reference
    /// at all. Half an octave at 400 Hz spans 336–476 Hz, which for a 110 Hz
    /// voice holds barely one harmonic — so every harmonic reads as a peak
    /// above its own neighbourhood, and the stage attenuates the voice's
    /// harmonic structure rather than any resonance. Measured before this
    /// existed: 92% of all time-frequency cells were being attenuated on
    /// perfectly clean speech.
    pub min_smoothing_hz: f64,
    /// Protrusion above the envelope, in dB, before any attenuation starts.
    ///
    /// Higher than it looks like it should be, because the reference it is
    /// compared against is biased low. The envelope is the dB-mean of a
    /// neighbourhood of magnitudes, and the dB-mean of a fluctuating spectrum
    /// sits well below its typical value — so with a "reasonable" 6 dB
    /// threshold, 92% of all cells in clean speech read as protruding. Swept
    /// against both a clean recording and one with a sustained resonance: at
    /// 12 dB the stage touches 5% of cells and still delivers the same +4.9 dB
    /// SI-SDR on the resonance that 6 dB did. All the extra activity was cost
    /// with no benefit.
    pub threshold_db: f64,
    /// Fraction of the excess above the threshold that is removed.
    pub ratio: f64,
    /// Most attenuation any bin may receive, in dB.
    pub max_reduction_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Sibilance band, given extra sensitivity.
    pub sibilance_low_hz: f64,
    pub sibilance_high_hz: f64,
    /// How much lower the threshold is inside the sibilance band, in dB.
    pub sibilance_extra_db: f64,
    /// Ignore everything below this. The fundamental region is the voice, not a
    /// resonance to be shaved.
    pub min_freq_hz: f64,
}

impl Default for DynEqOptions {
    fn default() -> Self {
        Self {
            frame_size: 1024,
            hop_size: 256,
            smoothing_octaves: 0.5,
            min_smoothing_hz: 400.0,
            threshold_db: 12.0,
            ratio: 0.5,
            max_reduction_db: 8.0,
            attack_ms: 2.0,
            release_ms: 60.0,
            sibilance_low_hz: 5_000.0,
            sibilance_high_hz: 9_000.0,
            sibilance_extra_db: 3.0,
            min_freq_hz: 300.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DynEqResult {
    pub channels: Vec<Vec<f32>>,
    /// Worst attenuation applied to any bin, in dB — a positive number.
    pub max_reduction_db: f64,
    /// Mean attenuation over the bins that were reduced at all, in dB.
    pub mean_reduction_db: f64,
    /// Fraction of time-frequency cells touched at all.
    pub active_fraction: f64,
}

/// For each bin, the range of bins its smoothing window covers.
///
/// Constant across frames, so it is worth computing once.
fn smoothing_windows(
    bins: usize,
    sample_rate: u32,
    frame_size: usize,
    octaves: f64,
    min_width_hz: f64,
) -> Vec<(usize, usize)> {
    let bin_hz = f64::from(sample_rate) / frame_size as f64;
    let half = 2f64.powf(octaves / 2.0);

    (0..bins)
        .map(|k| {
            let freq = k as f64 * bin_hz;
            let (mut lo_hz, mut hi_hz) = if freq > 0.0 {
                (freq / half, freq * half)
            } else {
                (0.0, bin_hz * 2.0)
            };
            // Widen to the Hz floor wherever the octave window is too narrow to
            // hold several harmonics.
            if hi_hz - lo_hz < min_width_hz {
                lo_hz = freq - min_width_hz / 2.0;
                hi_hz = freq + min_width_hz / 2.0;
            }
            let lo = ((lo_hz / bin_hz).floor().max(1.0) as usize).min(bins - 1);
            let hi = ((hi_hz / bin_hz).ceil() as usize).clamp(lo, bins - 1);
            (lo, hi)
        })
        .collect()
}

/// Per-bin threshold, lower inside the sibilance band and infinite below the
/// point where the voice stops being a resonance.
fn threshold_curve(
    bins: usize,
    sample_rate: u32,
    frame_size: usize,
    options: &DynEqOptions,
) -> Vec<f64> {
    let bin_hz = f64::from(sample_rate) / frame_size as f64;
    (0..bins)
        .map(|k| {
            let freq = k as f64 * bin_hz;
            if freq < options.min_freq_hz {
                // Effectively disabled: the fundamental and the first formant
                // are the voice, not something to shave.
                f64::INFINITY
            } else if freq >= options.sibilance_low_hz && freq <= options.sibilance_high_hz {
                options.threshold_db - options.sibilance_extra_db
            } else {
                options.threshold_db
            }
        })
        .collect()
}

/// Suppress dynamic resonances across every channel.
///
/// Channels are processed independently, which is right for damage, and does
/// mean a resonance present in both is attenuated in both — the desired
/// outcome, since it is the same resonance.
pub fn dynamic_eq(channels: &[Vec<f32>], sample_rate: u32, options: &DynEqOptions) -> DynEqResult {
    let stft = Stft::new(options.frame_size, options.hop_size);
    let bins = stft.bins();
    let windows = smoothing_windows(
        bins,
        sample_rate,
        options.frame_size,
        options.smoothing_octaves,
        options.min_smoothing_hz,
    );
    let thresholds = threshold_curve(bins, sample_rate, options.frame_size, options);

    // One hop is the time step for the envelope followers.
    let hop_sec = options.hop_size as f64 / f64::from(sample_rate);
    let attack = (-hop_sec / (options.attack_ms / 1000.0).max(1e-6)).exp();
    let release = (-hop_sec / (options.release_ms / 1000.0).max(1e-6)).exp();

    let mut worst = 0.0f64;
    let mut reduction_sum = 0.0f64;
    let mut reduced_cells = 0usize;
    let mut total_cells = 0usize;

    let out = channels
        .iter()
        .map(|samples| {
            let mut mags = vec![0.0f64; bins];
            let mut level_db = vec![0.0f64; bins];
            let mut prefix = vec![0.0f64; bins + 1];
            let mut target_db = vec![0.0f64; bins];
            // Smoothed gain state per bin, in dB. Negative is attenuating.
            let mut state = vec![0.0f64; bins];
            let mut gains = vec![1.0f64; bins];

            stft.process(samples, |frame, _| {
                magnitudes(frame, &mut mags);

                // Each bin's level in dB, computed once. The envelope below
                // reads every bin roughly 84 times — once per neighbouring bin
                // whose smoothing window covers it — and taking the logarithm
                // inside that loop was 93% of this stage's entire runtime.
                // Hoisting it changes nothing about the result: the same
                // expression, evaluated once instead of 84 times.
                for (k, m) in mags.iter().enumerate() {
                    level_db[k] = 10.0 * (m * m + 1e-30).log10();
                }

                // A running total, so each window is two lookups and a
                // subtraction rather than a loop over its 84-bin width.
                prefix[0] = 0.0;
                for k in 0..bins {
                    prefix[k + 1] = prefix[k] + level_db[k];
                }

                for (k, (lo, hi)) in windows.iter().enumerate() {
                    // The reference: this bin's own neighbourhood, averaged in
                    // dB. A dB average is deliberate — averaging in power would
                    // let a peak lift its own reference and hide from the
                    // comparison.
                    let envelope_db = (prefix[hi + 1] - prefix[*lo]) / (hi - lo + 1) as f64;
                    let excess = level_db[k] - envelope_db - thresholds[k];
                    target_db[k] = if excess > 0.0 {
                        -(excess * options.ratio).min(options.max_reduction_db)
                    } else {
                        0.0
                    };
                }

                for k in 0..bins {
                    // Attack when the required attenuation deepens, release
                    // when it eases.
                    let coefficient = if target_db[k] < state[k] {
                        attack
                    } else {
                        release
                    };
                    state[k] = target_db[k] + (state[k] - target_db[k]) * coefficient;
                    // Most bins are untouched and hold exactly zero, where the
                    // gain is exactly one — worth a branch to skip a few
                    // million exponentiations.
                    gains[k] = if state[k] == 0.0 {
                        1.0
                    } else {
                        10f64.powf(state[k] / 20.0)
                    };

                    total_cells += 1;
                    if state[k] < -0.01 {
                        reduced_cells += 1;
                        reduction_sum += -state[k];
                        worst = worst.max(-state[k]);
                    }
                }

                apply_gains(frame, &gains);
            })
        })
        .collect();

    DynEqResult {
        channels: out,
        max_reduction_db: worst,
        mean_reduction_db: if reduced_cells > 0 {
            reduction_sum / reduced_cells as f64
        } else {
            0.0
        },
        active_fraction: if total_cells > 0 {
            reduced_cells as f64 / total_cells as f64
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biquad::Biquad;
    use crate::ltas::{self, Ltas, LtasOptions};
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};
    use std::f64::consts::TAU;
    use std::sync::OnceLock;

    const SR: u32 = 48_000;

    fn voice() -> &'static [f32] {
        static VOICE: OnceLock<Vec<f32>> = OnceLock::new();
        VOICE.get_or_init(|| {
            let speech = synthetic_speech(&SpeechOptions {
                sample_rate: SR,
                spurts: vec![Spurt::new(6.0, -23.0)],
                pause_sec: 0.0,
                floor_dbfs: -80.0,
                seed: 4242,
                channels: 1,
            });
            speech.signal.channel(0).to_vec()
        })
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

    fn level_at(ltas: &Ltas, freq: f64) -> f64 {
        let i = ltas
            .freqs
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - freq).abs().total_cmp(&(b.1 - freq).abs()))
            .map(|(i, _)| i)
            .unwrap();
        ltas.db[i]
    }

    /// A steady tone riding on the voice, which is what a ringing room mode or
    /// a resonant note looks like.
    fn with_resonance(freq: f64, amplitude: f64) -> Vec<f32> {
        voice()
            .iter()
            .enumerate()
            .map(|(i, s)| s + (amplitude * (TAU * freq * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    #[test]
    fn a_resonance_is_pulled_down() {
        let resonance = 2_000.0;
        let input = with_resonance(resonance, 0.05);
        let result = dynamic_eq(std::slice::from_ref(&input), SR, &DynEqOptions::default());

        let before = ltas_of(&input);
        let after = ltas_of(&result.channels[0]);
        let reduction = level_at(&before, resonance) - level_at(&after, resonance);
        assert!(reduction > 2.0, "only {reduction} dB came off");
        assert!(result.max_reduction_db > 2.0, "{}", result.max_reduction_db);
    }

    #[test]
    fn the_voice_around_it_is_left_where_it_was() {
        // The point of comparing against a smoothed version of the frame's own
        // spectrum: the broad shape is the voice, and it must survive.
        let input = with_resonance(2_000.0, 0.05);
        let result = dynamic_eq(std::slice::from_ref(&input), SR, &DynEqOptions::default());

        let before = ltas_of(&input);
        let after = ltas_of(&result.channels[0]);
        for freq in [400.0, 700.0, 1_100.0, 3_500.0] {
            let change = level_at(&before, freq) - level_at(&after, freq);
            assert!(change.abs() < 1.5, "{change} dB moved at {freq} Hz");
        }
    }

    #[test]
    fn clean_speech_is_barely_touched() {
        // The measurement that set the threshold. Before the Hz floor on the
        // smoothing window and the 12 dB threshold, this was 92%.
        let result = dynamic_eq(&[voice().to_vec()], SR, &DynEqOptions::default());
        assert!(
            result.active_fraction < 0.10,
            "{}% of cells touched",
            result.active_fraction * 100.0
        );
    }

    #[test]
    fn nothing_below_the_minimum_frequency_is_touched() {
        // A big resonance right on the fundamental: the stage must leave it,
        // because down there the peak is the voice.
        let input = with_resonance(150.0, 0.15);
        let result = dynamic_eq(std::slice::from_ref(&input), SR, &DynEqOptions::default());

        let before = ltas_of(&input);
        let after = ltas_of(&result.channels[0]);
        let change = level_at(&before, 150.0) - level_at(&after, 150.0);
        assert!(change.abs() < 1.0, "{change} dB moved at 150 Hz");
    }

    #[test]
    fn sibilance_is_treated_more_readily_than_the_same_peak_elsewhere() {
        // The de-esser, which is this stage weighted by frequency rather than a
        // device of its own.
        let options = DynEqOptions::default();
        let reduction_at = |freq: f64| -> f64 {
            let input = with_resonance(freq, 0.02);
            let result = dynamic_eq(std::slice::from_ref(&input), SR, &options);
            level_at(&ltas_of(&input), freq) - level_at(&ltas_of(&result.channels[0]), freq)
        };
        // 7 kHz is inside the sibilance band, 3 kHz outside it, and the peak is
        // the same size in both.
        assert!(
            reduction_at(7_000.0) > reduction_at(3_000.0),
            "{} vs {}",
            reduction_at(7_000.0),
            reduction_at(3_000.0)
        );
    }

    #[test]
    fn the_reduction_limit_is_respected() {
        // A resonance far louder than anything the limit allows.
        let input = with_resonance(2_000.0, 0.5);
        for max_reduction_db in [2.0, 8.0] {
            let result = dynamic_eq(
                std::slice::from_ref(&input),
                SR,
                &DynEqOptions {
                    max_reduction_db,
                    ..DynEqOptions::default()
                },
            );
            assert!(
                result.max_reduction_db <= max_reduction_db + 1e-9,
                "{} past a limit of {max_reduction_db}",
                result.max_reduction_db
            );
        }
    }

    #[test]
    fn a_higher_threshold_touches_fewer_cells() {
        let input = with_resonance(2_000.0, 0.05);
        let active = |threshold_db: f64| -> f64 {
            dynamic_eq(
                std::slice::from_ref(&input),
                SR,
                &DynEqOptions {
                    threshold_db,
                    ..DynEqOptions::default()
                },
            )
            .active_fraction
        };
        assert!(
            active(20.0) < active(6.0),
            "{} vs {}",
            active(20.0),
            active(6.0)
        );
    }

    #[test]
    fn the_gain_moves_smoothly_across_frames() {
        // Free frame-to-frame gain modulates the signal at the frame rate,
        // which is audible as flutter. A resonance that stops abruptly is the
        // case that would expose it.
        let mut input = with_resonance(2_000.0, 0.1);
        for sample in &mut input[144_000..] {
            *sample = voice()[144_000];
        }
        let result = dynamic_eq(std::slice::from_ref(&input), SR, &DynEqOptions::default());

        // The output should not contain a step: measure the biggest jump in
        // 256-sample RMS across the release.
        let rms: Vec<f64> = result.channels[0][140_000..160_000]
            .as_chunks::<256>()
            .0
            .iter()
            .map(|c| (c.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / 256.0).sqrt())
            .collect();
        let peak = rms.iter().copied().fold(0.0f64, f64::max);
        let worst_jump = rms
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f64, f64::max);
        assert!(worst_jump < peak, "a step of {worst_jump} against {peak}");
    }

    #[test]
    fn a_frame_with_nothing_protruding_passes_through_unchanged() {
        // White noise has no *sustained* narrow peaks against its own
        // neighbourhood, so almost nothing is attenuated and reconstruction is
        // near-perfect. Not nothing: a noisy spectrum fluctuates, and a few
        // percent of cells cross the threshold on any given frame — which is
        // the same few percent the threshold was chosen to allow.
        let noise = leveller_corpus::noise(100_000, 21);
        let result = dynamic_eq(std::slice::from_ref(&noise), SR, &DynEqOptions::default());
        let worst = result.channels[0]
            .iter()
            .zip(&noise)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(result.active_fraction < 0.08, "{}", result.active_fraction);
        assert!(worst < 0.05, "worst change {worst}");
    }

    #[test]
    fn every_channel_is_processed() {
        let a = with_resonance(2_000.0, 0.05);
        let b: Vec<f32> = a.iter().map(|s| s * 0.5).collect();
        let result = dynamic_eq(&[a, b], SR, &DynEqOptions::default());
        assert_eq!(result.channels.len(), 2);
        assert_eq!(result.channels[0].len(), result.channels[1].len());
    }

    #[test]
    fn silence_stays_silent_and_reports_nothing() {
        let result = dynamic_eq(&[vec![0.0f32; 48_000]], SR, &DynEqOptions::default());
        assert_eq!(result.active_fraction, 0.0);
        assert_eq!(result.max_reduction_db, 0.0);
        assert_eq!(result.mean_reduction_db, 0.0);
        assert!(result.channels[0].iter().all(|s| s.abs() < 1e-9));
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let result = dynamic_eq(&[], SR, &DynEqOptions::default());
        assert!(result.channels.is_empty());
        assert_eq!(result.active_fraction, 0.0);
    }

    #[test]
    fn a_static_resonance_is_left_to_the_static_eq() {
        // Not a decline exactly — it will shave a little — but a filter-shaped
        // colouration present throughout is what eq.rs corrects, and this stage
        // should not be doing most of that work.
        let mut coloured = voice().to_vec();
        Biquad::peaking(f64::from(SR), 2_000.0, 6.0, 1.0).apply_in_place(&mut coloured);
        let result = dynamic_eq(
            std::slice::from_ref(&coloured),
            SR,
            &DynEqOptions::default(),
        );

        let reduction = level_at(&ltas_of(&coloured), 2_000.0)
            - level_at(&ltas_of(&result.channels[0]), 2_000.0);
        assert!(
            reduction < 3.0,
            "took {reduction} dB off a broad static bump"
        );
    }
}
