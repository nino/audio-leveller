//! Spectral noise reduction.
//!
//! Estimate what the noise looks like, then attenuate each time-frequency cell
//! in proportion to how much of it is noise. The whole difficulty is in not
//! making it worse: naive spectral subtraction leaves *musical noise*, a
//! shimmer of isolated surviving bins that warble in and out and sounds far
//! worse than the steady hiss it replaced. Three things here exist to prevent
//! that, and all three cost some raw reduction to buy it:
//!
//! - **Decision-directed a-priori SNR** (Ephraim and Malah). Estimating a
//!   bin's speech-to-noise ratio from the current frame alone is far too noisy
//!   a statistic; blending it with what the *previous* frame's output actually
//!   contained smooths the gain trajectory enormously. This is the single
//!   biggest difference between "denoised" and "underwater".
//!
//! - **A gain floor.** Never attenuating a bin by more than a set amount
//!   leaves the residual noise as a quiet, natural version of the original
//!   rather than a field of holes. The floor *is* the reduction target: asking
//!   for 12 dB of reduction sets the floor at −12 dB, which is why there is no
//!   separate wet/dry control. A global blend would dilute the speech along
//!   with the noise; the floor only ever binds where the bin is
//!   noise-dominated.
//!
//! - **Smoothing the gain across neighbouring bins**, so a surviving bin drags
//!   its neighbours up with it instead of standing alone as a tone.
//!
//! Reaching for a bigger number than the source can support is how denoising
//! got its reputation. The default 12 dB is deliberately modest.

use crate::stft::{Stft, apply_gains, magnitudes};

/// A half-open sample range, shared with the LTAS code — both mean the same
/// thing, so there is no reason for two of them.
pub type SampleRange = std::ops::Range<usize>;

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct DenoiseOptions {
    /// How far the noise floor may be pushed down, in dB. Also the per-bin gain
    /// floor — see the module note on why those are the same number.
    pub reduction_db: f64,
    pub frame_size: usize,
    pub hop_size: usize,
    /// Decision-directed blend. Higher is smoother; 0.98 is the usual choice.
    pub smoothing: f64,
    /// Multiply the noise estimate by this before subtracting.
    ///
    /// Slightly over-estimating noise trades a little speech dulling for
    /// markedly less musical noise — and a profile measured from pauses is a
    /// slight under-estimate of what sits under speech anyway.
    pub over_estimate: f64,
    /// Half-width, in bins, of the gain smoothing across frequency.
    pub bin_smoothing: usize,
}

impl Default for DenoiseOptions {
    fn default() -> Self {
        let stft = Stft::default();
        Self {
            reduction_db: 12.0,
            frame_size: stft.frame_size,
            hop_size: stft.hop_size,
            smoothing: 0.98,
            over_estimate: 1.5,
            bin_smoothing: 2,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NoiseProfile {
    /// Mean noise power per bin.
    pub power: Vec<f64>,
    /// How many frames it was measured over.
    pub frames: usize,
    /// True when it came from detected pauses rather than minimum statistics.
    pub from_pauses: bool,
}

/// Frames the minimum-statistics fallback will look at.
///
/// It wants the quietest fifth of frames per bin, which means holding a value
/// per bin per frame — 513 × 241 219 × 8 bytes, very nearly a gigabyte, on a
/// twenty-minute recording. A noise floor is a stationary property, so it is
/// estimated just as well from a few thousand frames spread across the file as
/// from every one of them, and the count that was used is reported either way.
/// Below the cap nothing changes, which covers everything the corpus and the
/// unit suite contain.
const MIN_STATISTICS_FRAME_CAP: usize = 20_000;

/// Does frame `f` fall wholly inside one of the ranges?
fn frame_in_ranges(f: usize, stft: &Stft, ranges: &[SampleRange]) -> bool {
    // The transform pads by one frame, so frame f covers
    // [f·hop − frame_size, …).
    let Some(start) = (f * stft.hop_size).checked_sub(stft.frame_size) else {
        return false;
    };
    let end = start + stft.frame_size;
    ranges.iter().any(|r| start >= r.start && end <= r.end)
}

/// Estimate the noise spectrum.
///
/// From detected pauses when there are any — that is a direct measurement of
/// the thing to be removed. Otherwise fall back to minimum statistics: per bin,
/// the mean of the quietest fifth of frames. That is more fragile, since in
/// continuous speech the quietest frames still contain speech, which is why it
/// is reported: the caller can then be more conservative.
pub fn estimate_noise_profile(
    samples: &[f32],
    pauses: &[SampleRange],
    stft: &Stft,
) -> NoiseProfile {
    let bins = stft.bins();
    let total = stft.frame_count(samples.len());
    let mut power = vec![0.0f64; bins];

    let usable = (0..total)
        .filter(|f| frame_in_ranges(*f, stft, pauses))
        .count();

    if usable >= 4 {
        let mut scratch = vec![0.0f64; bins];
        stft.scan(samples, |spectrum, f| {
            if !frame_in_ranges(f, stft, pauses) {
                return;
            }
            magnitudes(spectrum, &mut scratch);
            for (p, m) in power.iter_mut().zip(&scratch) {
                *p += m * m;
            }
        });
        for p in &mut power {
            *p /= usable as f64;
        }
        return NoiseProfile {
            power,
            frames: usable,
            from_pauses: true,
        };
    }

    if total == 0 {
        return NoiseProfile {
            power,
            frames: 0,
            from_pauses: false,
        };
    }

    // Minimum statistics: per bin, the mean of the quietest fifth of frames.
    let stride = if total <= MIN_STATISTICS_FRAME_CAP {
        1
    } else {
        total.div_ceil(MIN_STATISTICS_FRAME_CAP)
    };
    let kept = total.div_ceil(stride);

    let mut per_bin = vec![Vec::with_capacity(kept); bins];
    let mut scratch = vec![0.0f64; bins];
    stft.scan(samples, |spectrum, f| {
        if !f.is_multiple_of(stride) || per_bin[0].len() >= kept {
            return;
        }
        magnitudes(spectrum, &mut scratch);
        for (values, m) in per_bin.iter_mut().zip(&scratch) {
            values.push(m * m);
        }
    });

    let counted = per_bin[0].len();
    let take = ((counted as f64 * 0.2) as usize).max(1);
    for (p, values) in power.iter_mut().zip(&mut per_bin) {
        values.sort_by(f64::total_cmp);
        *p = values[..take.min(values.len())].iter().sum::<f64>() / take as f64;
    }

    NoiseProfile {
        power,
        frames: take,
        from_pauses: false,
    }
}

#[derive(Clone, Debug)]
pub struct DenoiseResult {
    pub channels: Vec<Vec<f32>>,
    pub profile: NoiseProfile,
    /// Mean gain applied to noise-dominated bins, in dB — negative.
    pub mean_noise_gain_db: f64,
    /// Mean gain applied to speech-dominated bins, in dB. Should be near zero.
    pub mean_speech_gain_db: f64,
}

#[derive(Default)]
struct Stats {
    noise_gain: f64,
    noise_count: usize,
    speech_gain: f64,
    speech_count: usize,
}

/// Attenuate steady noise across every channel.
///
/// `pauses` are sample ranges believed to hold no speech; the silence analysis
/// already knows them. Channels are processed independently, so a profile
/// measured on one side is never imposed on the other.
pub fn denoise(
    channels: &[Vec<f32>],
    pauses: &[SampleRange],
    options: &DenoiseOptions,
) -> DenoiseResult {
    let stft = Stft::new(options.frame_size, options.hop_size);
    let bins = stft.bins();
    let floor = 10f64.powf(-options.reduction_db.abs() / 20.0);
    let mut stats = Stats::default();
    let mut profile = NoiseProfile::default();

    let out = channels
        .iter()
        .map(|samples| {
            profile = estimate_noise_profile(samples, pauses, &stft);

            let mut mags = vec![0.0f64; bins];
            let mut gains = vec![0.0f64; bins];
            let mut smoothed = vec![0.0f64; bins];
            // The previous frame's clean power estimate, for the
            // decision-directed rule.
            let mut previous_clean = vec![0.0f64; bins];

            stft.process(samples, |frame, _| {
                magnitudes(frame, &mut mags);

                for k in 0..bins {
                    let noise = profile.power[k] * options.over_estimate + 1e-20;
                    let observed = mags[k] * mags[k];

                    // Posterior SNR: how much louder this bin is than noise
                    // alone.
                    let posterior = observed / noise;
                    // A-priori SNR, blending the previous frame's clean
                    // estimate with this frame's instantaneous reading. The
                    // blend is what stops the gain flickering bin to bin and
                    // turning the residual noise musical.
                    let instantaneous = (posterior - 1.0).max(0.0);
                    let prior = options.smoothing * (previous_clean[k] / noise)
                        + (1.0 - options.smoothing) * instantaneous;

                    // Wiener gain, floored so noise is attenuated but never
                    // erased.
                    gains[k] = (prior / (1.0 + prior)).max(floor);
                }

                // Smooth across frequency: an isolated surviving bin is exactly
                // what musical noise sounds like, so let its neighbours pull it
                // back down.
                let half = options.bin_smoothing;
                for (k, s) in smoothed.iter_mut().enumerate() {
                    let from = k.saturating_sub(half);
                    let to = (k + half).min(bins - 1);
                    *s = gains[from..=to].iter().sum::<f64>() / (to - from + 1) as f64;
                }

                for k in 0..bins {
                    let observed = mags[k] * mags[k];
                    let noise = profile.power[k] * options.over_estimate + 1e-20;
                    previous_clean[k] = smoothed[k] * smoothed[k] * observed;

                    // Book-keeping: how hard is noise being pushed, against
                    // speech?
                    let gain_db = 20.0 * smoothed[k].max(1e-12).log10();
                    if observed < 4.0 * noise {
                        stats.noise_gain += gain_db;
                        stats.noise_count += 1;
                    } else if observed > 100.0 * noise {
                        stats.speech_gain += gain_db;
                        stats.speech_count += 1;
                    }
                }

                apply_gains(frame, &smoothed);
            })
        })
        .collect();

    DenoiseResult {
        channels: out,
        profile,
        mean_noise_gain_db: if stats.noise_count > 0 {
            stats.noise_gain / stats.noise_count as f64
        } else {
            0.0
        },
        mean_speech_gain_db: if stats.speech_count > 0 {
            stats.speech_gain / stats.speech_count as f64
        } else {
            0.0
        },
    }
}

#[cfg(test)]
// `&[a..b]` here is a one-element slice of ranges, which is what the profile
// estimator takes; clippy reads it as a mistyped `vec![a; b]`.
#[expect(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use leveller_corpus::{SpeechOptions, Spurt, add_noise, synthetic_speech};
    use std::sync::OnceLock;

    const SR: u32 = 48_000;

    fn speech() -> &'static leveller_corpus::Speech {
        static SPEECH: OnceLock<leveller_corpus::Speech> = OnceLock::new();
        SPEECH.get_or_init(|| {
            synthetic_speech(&SpeechOptions {
                sample_rate: SR,
                spurts: vec![Spurt::new(3.0, -23.0), Spurt::new(3.0, -23.0)],
                pause_sec: 1.5,
                floor_dbfs: -90.0,
                seed: 4242,
                channels: 1,
            })
        })
    }

    fn clean() -> Vec<f32> {
        speech().signal.channel(0).to_vec()
    }

    /// The pauses, as the silence analysis would report them: before the first
    /// spurt, between the two, and after the last.
    fn pauses() -> Vec<SampleRange> {
        let segments = &speech().segments;
        vec![
            0..segments[0].start,
            segments[0].end..segments[1].start,
            segments[1].end..speech().signal.len(),
        ]
    }

    fn noisy(snr_db: f64) -> Vec<f32> {
        add_noise(&speech().signal, snr_db, 5).channel(0).to_vec()
    }

    fn rms_db(samples: &[f32], range: SampleRange) -> f64 {
        let slice = &samples[range];
        let energy: f64 = slice.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
        10.0 * (energy / slice.len() as f64 + 1e-30).log10()
    }

    #[test]
    fn the_noise_floor_comes_down() {
        let input = noisy(15.0);
        let result = denoise(
            std::slice::from_ref(&input),
            &pauses(),
            &DenoiseOptions::default(),
        );

        let pause = pauses()[1].clone();
        let before = rms_db(&input, pause.clone());
        let after = rms_db(&result.channels[0], pause);
        assert!(before - after > 6.0, "only {} dB came off", before - after);
    }

    #[test]
    fn the_speech_is_left_where_it_was() {
        let input = noisy(15.0);
        let result = denoise(
            std::slice::from_ref(&input),
            &pauses(),
            &DenoiseOptions::default(),
        );

        let segment = speech().segments[0];
        let before = rms_db(&input, segment.start..segment.end);
        let after = rms_db(&result.channels[0], segment.start..segment.end);
        assert!(
            (before - after).abs() < 2.0,
            "speech moved by {} dB",
            before - after
        );
        // And the book-keeping should agree: near-unity gain on speech bins.
        assert!(
            result.mean_speech_gain_db > -1.5,
            "{}",
            result.mean_speech_gain_db
        );
        assert!(
            result.mean_noise_gain_db < -3.0,
            "{}",
            result.mean_noise_gain_db
        );
    }

    #[test]
    fn the_reduction_setting_is_also_the_floor() {
        // The two are the same number by design, so asking for less reduction
        // must leave more noise.
        let input = noisy(15.0);
        let pause = pauses()[1].clone();
        let residual = |reduction_db: f64| -> f64 {
            let result = denoise(
                std::slice::from_ref(&input),
                &pauses(),
                &DenoiseOptions {
                    reduction_db,
                    ..DenoiseOptions::default()
                },
            );
            rms_db(&result.channels[0], pause.clone())
        };
        assert!(
            residual(6.0) > residual(18.0) + 3.0,
            "{} vs {}",
            residual(6.0),
            residual(18.0)
        );
    }

    #[test]
    fn a_profile_from_pauses_says_so() {
        let input = noisy(15.0);
        let stft = Stft::default();
        let profile = estimate_noise_profile(&input, &pauses(), &stft);
        assert!(profile.from_pauses);
        assert!(profile.frames > 4, "{}", profile.frames);
        assert_eq!(profile.power.len(), stft.bins());
        assert!(profile.power.iter().all(|p| *p > 0.0));
    }

    #[test]
    fn with_no_pauses_it_falls_back_to_minimum_statistics_and_says_so() {
        let input = noisy(15.0);
        let profile = estimate_noise_profile(&input, &[], &Stft::default());
        assert!(!profile.from_pauses);
        assert!(profile.frames > 0);
        // Still a usable estimate: the quietest fifth of a recording with
        // pauses in it is mostly pause.
        let from_pauses = estimate_noise_profile(&input, &pauses(), &Stft::default());
        let total = |p: &NoiseProfile| p.power.iter().sum::<f64>();
        let ratio = total(&profile) / total(&from_pauses);
        assert!(ratio > 0.1 && ratio < 10.0, "off by {ratio}x");
    }

    #[test]
    fn a_pause_list_too_short_to_measure_falls_back() {
        // At 1024/256 a range of 1500 samples holds exactly two frames whose
        // whole window fits inside it — fewer than the four the average needs.
        let input = noisy(15.0);
        let profile = estimate_noise_profile(&input, &[0..1_500], &Stft::default());
        assert!(!profile.from_pauses);
    }

    #[test]
    fn denoising_clean_material_barely_touches_it() {
        // The transparency case. There is nothing to remove, so the gain should
        // sit near unity almost everywhere.
        let input = clean();
        let result = denoise(
            std::slice::from_ref(&input),
            &pauses(),
            &DenoiseOptions::default(),
        );
        let segment = speech().segments[0];
        let before = rms_db(&input, segment.start..segment.end);
        let after = rms_db(&result.channels[0], segment.start..segment.end);
        assert!(
            (before - after).abs() < 1.0,
            "moved by {} dB",
            before - after
        );
    }

    #[test]
    fn a_worse_recording_gets_more_reduction() {
        let pause = pauses()[1].clone();
        let removed = |snr_db: f64| -> f64 {
            let input = noisy(snr_db);
            let result = denoise(
                std::slice::from_ref(&input),
                &pauses(),
                &DenoiseOptions::default(),
            );
            rms_db(&input, pause.clone()) - rms_db(&result.channels[0], pause.clone())
        };
        assert!(
            removed(6.0) > removed(30.0),
            "{} vs {}",
            removed(6.0),
            removed(30.0)
        );
    }

    #[test]
    fn every_channel_is_treated() {
        let input = noisy(15.0);
        let result = denoise(
            &[input.clone(), input],
            &pauses(),
            &DenoiseOptions::default(),
        );
        assert_eq!(result.channels.len(), 2);
        assert_eq!(result.channels[0], result.channels[1]);
    }

    #[test]
    fn silence_stays_silent() {
        let result = denoise(
            &[vec![0.0f32; 48_000]],
            &[0..48_000],
            &DenoiseOptions::default(),
        );
        assert!(result.channels[0].iter().all(|s| s.abs() < 1e-9));
    }

    #[test]
    fn nothing_at_all_is_handled() {
        let result = denoise(&[], &[], &DenoiseOptions::default());
        assert!(result.channels.is_empty());
        assert_eq!(result.mean_noise_gain_db, 0.0);
        assert_eq!(result.mean_speech_gain_db, 0.0);
    }

    #[test]
    fn the_gain_floor_is_a_floor_and_not_a_target() {
        // Noise is attenuated to the floor and no further, which is what keeps
        // the residual sounding like quiet room rather than like holes.
        let input = noisy(10.0);
        let result = denoise(
            std::slice::from_ref(&input),
            &pauses(),
            &DenoiseOptions {
                reduction_db: 12.0,
                ..DenoiseOptions::default()
            },
        );
        assert!(
            result.mean_noise_gain_db >= -12.5,
            "went past the floor: {}",
            result.mean_noise_gain_db
        );
    }
}
