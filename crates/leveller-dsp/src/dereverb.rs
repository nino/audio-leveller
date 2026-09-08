//! Dereverberation by weighted prediction error.
//!
//! Reverberation is the room convolving the voice with its own impulse
//! response. The *late* part of that response — everything past the first few
//! tens of milliseconds — is what makes a recording sound distant, and it has a
//! property worth exploiting: it is a linear function of the signal's own past.
//! So for each frequency bin, predict the current frame from frames a short
//! delay back and subtract what the prediction accounts for. What is left is
//! the direct sound plus the early reflections.
//!
//! The weighting is the idea in the name. Ordinary least squares would be
//! dominated by the loudest frames, which are exactly the ones where the direct
//! sound is strongest and reverberation matters least. Weighting each frame by
//! the inverse of the *desired* signal's power there concentrates the fit on
//! the frames that are mostly tail. That power is not known in advance, so it
//! is estimated from the current output and the whole thing iterated.
//!
//! Why this rather than a trained enhancer: it cannot invent speech. It only
//! ever subtracts a linear prediction, so its failure mode is leaving
//! reverberation behind, not fabricating detail that was never spoken. For a
//! podcast where the speaker's actual voice is the product, that is the right
//! trade — and it needs no weights, so it runs everywhere.
//!
//! The prediction delay is what protects the direct sound: without it the
//! filter would happily predict, and therefore cancel, the speech itself.

use crate::stft::Stft;
use rustfft::num_complex::Complex64;

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct DereverbOptions {
    /// Prediction filter length, in frames. Longer reaches further into the
    /// tail.
    pub taps: usize,
    /// Frames to skip before the prediction starts.
    ///
    /// This is what keeps the direct sound and the early reflections intact.
    /// They carry the intelligibility, and a filter allowed to predict them
    /// would cancel the voice along with the room.
    pub delay: usize,
    /// How many times to re-estimate the desired-signal power and refit.
    pub iterations: usize,
    pub frame_size: usize,
    pub hop_size: usize,
    /// Diagonal loading, relative to the mean power, for a stable solve.
    pub regularisation: f64,
}

impl Default for DereverbOptions {
    /// Chosen by sweeping frame size, delay and tap count against both a
    /// reverberant recording and a dry one — because the failure that matters
    /// is damage to material that never needed treating.
    ///
    /// The frame size is four times the pipeline's usual 1024, and that is the
    /// whole story. The method assumes the *desired* signal is uncorrelated
    /// across the prediction delay, and speech violates that badly at short
    /// delays: at 1024/256 a delay of two frames reaches back 10 ms, about one
    /// pitch period for a male voice, so the filter predicts the voice's own
    /// harmonic structure and subtracts it. Measured: dry speech came back at
    /// 1 dB SI-SDR — destroyed. At 4096/1024 with a delay of three frames the
    /// filter reaches back 64 ms, past the pitch period and past the vocal
    /// tract's ringing, and only the room's tail is left to predict. The same
    /// dry recording comes back at 20 dB.
    fn default() -> Self {
        Self {
            taps: 20,
            delay: 3,
            iterations: 3,
            frame_size: 4096,
            hop_size: 1024,
            regularisation: 1e-4,
        }
    }
}

/// Solve a small Hermitian system `A g = b` by Gaussian elimination with
/// partial pivoting. `a` is row-major.
fn solve_complex(a: &mut [Complex64], b: &mut [Complex64], n: usize) -> Option<Vec<Complex64>> {
    for col in 0..n {
        let mut pivot_row = col;
        let mut best = 0.0;
        for row in col..n {
            let magnitude = a[row * n + col].norm_sqr();
            if magnitude > best {
                best = magnitude;
                pivot_row = row;
            }
        }
        if best < 1e-30 {
            return None;
        }

        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
            }
            b.swap(col, pivot_row);
        }

        let pivot = a[col * n + col];
        for row in col + 1..n {
            let factor = a[row * n + col] / pivot;
            if factor == Complex64::default() {
                continue;
            }
            for k in col..n {
                let above = a[col * n + k];
                a[row * n + k] -= factor * above;
            }
            let above = b[col];
            b[row] -= factor * above;
        }
    }

    let mut x = b.to_vec();
    for row in (0..n).rev() {
        let mut sum = x[row];
        for k in row + 1..n {
            sum -= a[row * n + k] * x[k];
        }
        let diagonal = a[row * n + row];
        if diagonal.norm_sqr() < 1e-30 {
            return None;
        }
        x[row] = sum / diagonal;
    }
    Some(x)
}

/// Run the method over one bin's trajectory, in place.
fn dereverb_bin(trajectory: &mut [Complex64], options: &DereverbOptions) {
    let frames = trajectory.len();
    let (taps, delay) = (options.taps, options.delay);
    if frames <= taps + delay + 2 {
        return;
    }

    // Desired-signal power per frame; starts as the observation's own.
    let mut power: Vec<f64> = trajectory.iter().map(Complex64::norm_sqr).collect();
    let mean_power = power.iter().sum::<f64>() / frames as f64;
    if mean_power <= 0.0 {
        return;
    }

    // Floor the weighting power well above zero. The 1/power weighting is the
    // whole idea, but near-silent frames would otherwise carry effectively
    // infinite weight and drag the fit to a filter that subtracts far more than
    // the room put in — and because each iteration re-estimates power from its
    // own output, that runs away rather than settling.
    let floor = mean_power * 1e-3;

    let mut matrix = vec![Complex64::default(); taps * taps];
    let mut vector = vec![Complex64::default(); taps];
    let mut history = vec![Complex64::default(); taps];
    let mut out = trajectory.to_vec();

    for _ in 0..options.iterations {
        matrix.fill(Complex64::default());
        vector.fill(Complex64::default());

        // Weighted correlation of the delayed history with itself, and with the
        // current frame.
        for t in delay + taps..frames {
            let weight = 1.0 / power[t].max(floor);

            for (i, h) in history.iter_mut().enumerate() {
                *h = trajectory[t - delay - i];
            }
            let y = trajectory[t];

            for i in 0..taps {
                let xi = history[i];

                // R is Hermitian — R[j][i] is the conjugate of R[i][j] — so
                // only the upper triangle is computed and the rest mirrored.
                // That is exact rather than approximate: multiplication
                // commutes and negation is lossless, so the mirrored entry is
                // the same bits the full loop would have accumulated.
                for j in i..taps {
                    let value = xi * history[j].conj() * weight;
                    matrix[i * taps + j] += value;
                    if j != i {
                        matrix[j * taps + i] += value.conj();
                    }
                }

                vector[i] += xi * y.conj() * weight;
            }
        }

        // Diagonal loading: the system is near-singular wherever the signal is
        // quiet, and an unregularised solve there produces enormous filters
        // that subtract far more than the room put in.
        let load = mean_power * options.regularisation * frames as f64;
        for i in 0..taps {
            matrix[i * taps + i] += load;
        }

        let mut rhs = vector.clone();
        let Some(filter) = solve_complex(&mut matrix.clone(), &mut rhs, taps) else {
            return;
        };

        // Subtract the predicted late reverberation.
        for t in 0..frames {
            let prediction = if t >= delay + taps {
                (0..taps)
                    .map(|i| filter[i].conj() * trajectory[t - delay - i])
                    .sum::<Complex64>()
            } else {
                Complex64::default()
            };
            out[t] = trajectory[t] - prediction;
        }

        // Re-estimate the desired-signal power for the next pass.
        for (p, o) in power.iter_mut().zip(&out) {
            *p = o.norm_sqr();
        }
    }

    trajectory.copy_from_slice(&out);
}

#[derive(Clone, Debug)]
pub struct DereverbResult {
    pub channels: Vec<Vec<f32>>,
    /// Bins actually processed. A file too short to hold the filter leaves
    /// some — or all — untouched.
    pub bins_processed: usize,
}

/// Dereverberate every channel independently.
pub fn dereverb(channels: &[Vec<f32>], options: &DereverbOptions) -> DereverbResult {
    let stft = Stft::new(options.frame_size, options.hop_size);
    let bins = stft.bins();
    let mut bins_processed = 0usize;

    let out = channels
        .iter()
        .map(|samples| {
            let mut analysis = stft.analyze(samples);
            let frames = analysis.frames.len();

            // One bin's trajectory at a time. The transform hands back frames,
            // and the method works down the other axis, so the transpose is
            // unavoidable — but doing it a bin at a time keeps the extra memory
            // to one trajectory rather than a second whole spectrogram.
            let mut trajectory = vec![Complex64::default(); frames];
            for bin in 0..bins {
                for (t, frame) in analysis.frames.iter().enumerate() {
                    trajectory[t] = frame[bin];
                }
                let before = trajectory.clone();
                dereverb_bin(&mut trajectory, options);
                if trajectory != before {
                    bins_processed += 1;
                }
                for (t, frame) in analysis.frames.iter_mut().enumerate() {
                    frame[bin] = trajectory[t];
                }
            }

            analysis.synthesize()
        })
        .collect();

    DereverbResult {
        channels: out,
        bins_processed: if channels.is_empty() {
            0
        } else {
            bins_processed / channels.len()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Signal;
    use crate::reverbtime::reverb_decay;
    use leveller_corpus::{RirOptions, SpeechOptions, Spurt, add_reverb, synthetic_speech};
    use std::sync::OnceLock;

    const SR: u32 = 48_000;

    fn speech() -> &'static leveller_corpus::Speech {
        static SPEECH: OnceLock<leveller_corpus::Speech> = OnceLock::new();
        SPEECH.get_or_init(|| {
            synthetic_speech(&SpeechOptions {
                sample_rate: SR,
                spurts: vec![Spurt::new(3.0, -23.0), Spurt::new(3.0, -23.0)],
                pause_sec: 0.6,
                floor_dbfs: -80.0,
                seed: 4242,
                channels: 1,
            })
        })
    }

    fn dry() -> &'static Vec<f32> {
        static DRY: OnceLock<Vec<f32>> = OnceLock::new();
        DRY.get_or_init(|| speech().signal.channel(0).to_vec())
    }

    fn wet() -> &'static Vec<f32> {
        static WET: OnceLock<Vec<f32>> = OnceLock::new();
        WET.get_or_init(|| {
            add_reverb(&speech().signal, &RirOptions::default())
                .channel(0)
                .to_vec()
        })
    }

    /// Scale-invariant signal-to-distortion ratio against a reference, in dB.
    ///
    /// The standard measure for this kind of work: it projects the estimate
    /// onto the reference first, so a result that is merely quieter than the
    /// reference is not punished for it — only the part that is not a scaled
    /// copy counts as distortion.
    fn si_sdr(estimate: &[f32], reference: &[f32]) -> f64 {
        let n = estimate.len().min(reference.len());
        let dot: f64 = (0..n)
            .map(|i| f64::from(estimate[i]) * f64::from(reference[i]))
            .sum();
        let energy: f64 = (0..n).map(|i| f64::from(reference[i]).powi(2)).sum();
        if energy <= 0.0 {
            return f64::NEG_INFINITY;
        }
        let scale = dot / energy;
        let (mut signal, mut noise) = (0.0f64, 0.0f64);
        for i in 0..n {
            let target = scale * f64::from(reference[i]);
            signal += target * target;
            noise += (f64::from(estimate[i]) - target).powi(2);
        }
        if noise <= 0.0 {
            return f64::INFINITY;
        }
        10.0 * (signal / noise).log10()
    }

    #[test]
    fn a_reverberant_recording_comes_back_drier() {
        let result = dereverb(std::slice::from_ref(wet()), &DereverbOptions::default());

        let before = reverb_decay(&Signal::mono(SR, wet().clone())).expect("wet decay");
        let after =
            reverb_decay(&Signal::mono(SR, result.channels[0].clone())).expect("treated decay");
        assert!(after < before, "{before} ms then {after} ms");
    }

    #[test]
    fn it_moves_the_recording_toward_the_dry_reference() {
        let result = dereverb(std::slice::from_ref(wet()), &DereverbOptions::default());
        let before = si_sdr(wet(), dry());
        let after = si_sdr(&result.channels[0], dry());
        assert!(after > before, "{before} dB then {after} dB");
    }

    #[test]
    fn dry_material_survives_it() {
        // The failure that matters. At the wrong frame size this is where the
        // method destroys a recording it should have left alone: it predicts
        // the voice's own harmonic structure and subtracts it.
        let result = dereverb(std::slice::from_ref(dry()), &DereverbOptions::default());
        let sdr = si_sdr(&result.channels[0], dry());
        assert!(sdr > 15.0, "dry speech came back at {sdr} dB");
    }

    #[test]
    fn a_short_frame_is_what_destroys_dry_material() {
        // The measurement behind the default frame size, kept as a test so the
        // default cannot be "tidied" back to the pipeline's usual 1024 without
        // someone noticing.
        let short = DereverbOptions {
            frame_size: 1024,
            hop_size: 256,
            delay: 2,
            ..DereverbOptions::default()
        };
        let result = dereverb(std::slice::from_ref(dry()), &short);
        let sdr = si_sdr(&result.channels[0], dry());
        assert!(sdr < 12.0, "a 1024 frame left dry speech at {sdr} dB");
    }

    #[test]
    fn silence_stays_silent() {
        let result = dereverb(&[vec![0.0f32; 96_000]], &DereverbOptions::default());
        assert!(result.channels[0].iter().all(|s| s.abs() < 1e-9));
        assert_eq!(result.bins_processed, 0, "there is nothing to predict");
    }

    #[test]
    fn a_file_too_short_for_the_filter_is_left_alone() {
        // Fewer frames than taps + delay: nothing can be fitted, and passing
        // the audio through is the only honest answer.
        let short: Vec<f32> = dry()[..8_000].to_vec();
        let result = dereverb(std::slice::from_ref(&short), &DereverbOptions::default());
        assert_eq!(result.bins_processed, 0);
        let sdr = si_sdr(&result.channels[0], &short);
        assert!(sdr > 40.0, "{sdr} dB");
    }

    #[test]
    fn every_channel_is_treated() {
        let result = dereverb(&[wet().clone(), wet().clone()], &DereverbOptions::default());
        assert_eq!(result.channels.len(), 2);
        assert_eq!(result.channels[0], result.channels[1]);
        assert!(result.bins_processed > 0);
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let result = dereverb(&[], &DereverbOptions::default());
        assert!(result.channels.is_empty());
        assert_eq!(result.bins_processed, 0);
    }

    #[test]
    fn more_iterations_do_not_run_away() {
        // Each pass re-estimates the weighting power from its own output, which
        // is exactly the shape of a feedback loop. The power floor is what
        // stops it, and this is the test that would catch its removal.
        let level_of = |iterations: usize| -> f64 {
            let result = dereverb(
                std::slice::from_ref(wet()),
                &DereverbOptions {
                    iterations,
                    ..DereverbOptions::default()
                },
            );
            let energy: f64 = result.channels[0]
                .iter()
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum();
            10.0 * (energy / result.channels[0].len() as f64 + 1e-30).log10()
        };
        let one = level_of(1);
        let many = level_of(6);
        assert!(
            (one - many).abs() < 6.0,
            "level ran from {one} dB to {many} dB across iterations"
        );
    }

    #[test]
    fn the_prediction_delay_is_what_protects_the_voice() {
        // With no delay the filter is allowed to predict the direct sound, and
        // cancels the speech along with the room.
        let no_delay = DereverbOptions {
            delay: 0,
            ..DereverbOptions::default()
        };
        let reckless = dereverb(std::slice::from_ref(dry()), &no_delay);
        let careful = dereverb(std::slice::from_ref(dry()), &DereverbOptions::default());
        assert!(
            si_sdr(&reckless.channels[0], dry()) < si_sdr(&careful.channels[0], dry()),
            "{} vs {}",
            si_sdr(&reckless.channels[0], dry()),
            si_sdr(&careful.channels[0], dry())
        );
    }
}
