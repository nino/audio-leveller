//! Level-domain dynamics: one gain, computed from the signal's own envelope
//! and smoothed over time.
//!
//! A compressor and a downward expander are the same machine pointed in
//! opposite directions — measure the level, look up a gain on a static curve,
//! smooth that gain, apply it — so both live here and differ only in the curve
//! and the two time constants. Sharing the machine means the envelope detector,
//! the smoother and the multi-channel behaviour are written and tested once.
//!
//! Three decisions carry the quality:
//!
//! - **The gain is smoothed, not the envelope.** Smoothing the detector first
//!   and then looking up a gain makes the effective time constant depend on how
//!   far up the curve you are, which is why some compressors breathe. Measured
//!   against the reference this project was fitted to, smoothing the gain is
//!   also what that processor does: its gain trajectory fits a first-order
//!   smoother on the *gain* to within a few hundredths.
//!
//! - **One gain for every channel.** Detecting per channel and applying per
//!   channel would let a loud left channel duck while the right stayed put,
//!   which moves the stereo image in time with the speech. The detector sums
//!   channel power, so the image holds still.
//!
//! - **No make-up gain.** A compressor normally needs one, because pulling the
//!   peaks down makes everything quieter. Here the level stage runs afterwards
//!   and normalises to an exact loudness target, so make-up gain would be a
//!   number the leveller immediately takes back out. Leaving it off keeps the
//!   compressor's report honest: the gain it shows is the gain it applied.

/// A static gain curve: given a detected level in dBFS, the gain in dB.
pub trait Curve {
    fn gain_db(&self, level_db: f64) -> f64;
}

impl<F: Fn(f64) -> f64> Curve for F {
    fn gain_db(&self, level_db: f64) -> f64 {
        self(level_db)
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct Timing {
    /// Time constant while the gain is *falling*, in ms.
    pub down_ms: f64,
    /// Time constant while the gain is *rising*, in ms.
    pub up_ms: f64,
    /// Envelope detector window, in ms.
    ///
    /// Short enough to follow syllables, long enough not to track individual
    /// pitch periods — at 100 Hz a window under 10 ms would ripple at the
    /// fundamental and modulate the gain with it.
    pub detector_ms: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            down_ms: 33.0,
            up_ms: 168.0,
            detector_ms: 15.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DynamicsResult {
    pub channels: Vec<Vec<f32>>,
    /// Gain actually applied, per sample, in dB.
    pub gain_db: Vec<f32>,
    /// Largest attenuation applied, in dB — a positive number.
    pub max_reduction_db: f64,
    /// Mean gain over the samples where the detector saw signal, in dB.
    pub mean_gain_db: f64,
    /// Fraction of those samples where the gain differed from unity by more
    /// than 0.1 dB.
    pub active_fraction: f64,
}

/// Level in dBFS of a sliding-RMS detector, one value per sample.
///
/// A boxcar rather than a one-pole, so the detector has a definite memory: a
/// syllable leaves the window entirely rather than trailing an exponential tail
/// into the next one.
fn detector_levels(channels: &[Vec<f32>], sample_rate: u32, detector_ms: f64) -> Vec<f64> {
    let length = channels.first().map_or(0, Vec::len);
    let window = ((detector_ms / 1000.0 * f64::from(sample_rate)).round() as usize).max(1);
    let channel_count = channels.len().max(1) as f64;

    let mut levels = Vec::with_capacity(length);
    let mut history = vec![0.0f64; window];
    let mut sum = 0.0;
    let mut at = 0usize;

    for i in 0..length {
        let power = channels
            .iter()
            .map(|c| f64::from(c[i]) * f64::from(c[i]))
            .sum::<f64>()
            / channel_count;

        sum += power - history[at];
        history[at] = power;
        at = (at + 1) % window;

        let filled = (i + 1).min(window);
        let rms = (sum.max(0.0) / filled as f64).sqrt();
        // A floor rather than −∞: the smoother would take a very long time to
        // climb back from negative infinity, and every curve here is flat down
        // there anyway.
        levels.push(if rms > 1e-12 {
            20.0 * rms.log10()
        } else {
            -240.0
        });
    }
    levels
}

/// Apply a static gain curve with attack and release smoothing.
///
/// The detector is delay-free with respect to the output, so a transient
/// arrives before the gain has finished moving — that is what an attack time
/// means, and pre-delaying the signal to hide it would turn the compressor into
/// a limiter. The true-peak limiter in the level stage catches what gets
/// through, and runs last for exactly this reason.
pub fn apply(
    channels: &[Vec<f32>],
    sample_rate: u32,
    curve: &impl Curve,
    timing: &Timing,
) -> DynamicsResult {
    let length = channels.first().map_or(0, Vec::len);
    let levels = detector_levels(channels, sample_rate, timing.detector_ms);

    let coefficient = |ms: f64| -> f64 {
        if ms <= 0.0 {
            0.0
        } else {
            (-1.0 / (ms / 1000.0 * f64::from(sample_rate))).exp()
        }
    };
    let down = coefficient(timing.down_ms);
    let up = coefficient(timing.up_ms);

    let mut gain_db = Vec::with_capacity(length);
    let mut gain = 0.0f64;
    let mut max_reduction_db = 0.0f64;
    let mut sum_gain = 0.0;
    let mut counted = 0usize;
    let mut active = 0usize;

    for level in &levels {
        let target = curve.gain_db(*level);
        // Falling gain uses the attack constant, rising gain the release one.
        let a = if target < gain { down } else { up };
        gain = a * gain + (1.0 - a) * target;
        gain_db.push(gain as f32);

        if *level > -120.0 {
            sum_gain += gain;
            counted += 1;
            if gain.abs() > 0.1 {
                active += 1;
            }
        }
        if -gain > max_reduction_db {
            max_reduction_db = -gain;
        }
    }

    let out = channels
        .iter()
        .map(|channel| {
            channel
                .iter()
                .zip(&gain_db)
                .map(|(s, g)| (f64::from(*s) * 10f64.powf(f64::from(*g) / 20.0)) as f32)
                .collect()
        })
        .collect();

    DynamicsResult {
        channels: out,
        gain_db,
        max_reduction_db,
        mean_gain_db: if counted > 0 {
            sum_gain / counted as f64
        } else {
            0.0
        },
        active_fraction: if counted > 0 {
            active as f64 / counted as f64
        } else {
            0.0
        },
    }
}

/// Downward compression above a threshold, with a quadratic soft knee.
#[derive(Clone, Copy, Debug)]
pub struct Compressor {
    /// Level above which gain reduction begins, in dBFS.
    pub threshold_db: f64,
    /// Compression ratio. 1 is no compression.
    pub ratio: f64,
    /// Width of the soft knee, in dB, centred on the threshold.
    ///
    /// A hard knee makes the gain a kinked function of level, and speech spends
    /// most of its time near the threshold — so the kink is audible as the gain
    /// flicking on and off across it. The reference processor's own curve bends
    /// over roughly 10 dB rather than turning a corner.
    pub knee_db: f64,
    /// Never attenuate by more than this, in dB.
    pub max_reduction_db: f64,
}

impl Curve for Compressor {
    fn gain_db(&self, level_db: f64) -> f64 {
        let slope = 1.0 - 1.0 / self.ratio.max(1e-6);
        let reduction = knee(level_db - self.threshold_db, slope, self.knee_db);
        -reduction.min(self.max_reduction_db)
    }
}

/// Downward expansion below a threshold — the quiet gets quieter.
///
/// [`range_db`](Self::range_db) is what keeps this from being a gate. An
/// expander with unlimited range drives the gaps between words to digital
/// silence, and a recording scrubbed to silence between words sounds broken in
/// a way the noise never did: the room disappears and reappears with every
/// syllable. Bounding the attenuation keeps the room present, just further down.
#[derive(Clone, Copy, Debug)]
pub struct Expander {
    /// Level below which gain reduction begins, in dBFS.
    pub threshold_db: f64,
    /// Expansion ratio. 1 is no expansion.
    pub ratio: f64,
    /// Width of the soft knee, in dB, centred on the threshold.
    pub knee_db: f64,
    /// Never attenuate by more than this, in dB.
    pub range_db: f64,
}

impl Curve for Expander {
    fn gain_db(&self, level_db: f64) -> f64 {
        let slope = (self.ratio - 1.0).max(0.0);
        let reduction = knee(self.threshold_db - level_db, slope, self.knee_db);
        -reduction.min(self.range_db)
    }
}

/// Reduction for being `over` dB past the threshold, bent across a soft knee.
///
/// Quadratic interpolation across the knee: continuous in both value and slope
/// at each end, which is what stops the gain flickering at the threshold.
fn knee(over: f64, slope: f64, knee_db: f64) -> f64 {
    let half = knee_db / 2.0;
    if knee_db > 0.0 && over > -half && over < half {
        let x = over + half;
        slope * x * x / (2.0 * knee_db)
    } else if over <= 0.0 {
        0.0
    } else {
        slope * over
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    fn tone(secs: f64, amplitude: f64) -> Vec<f32> {
        let n = (secs * f64::from(SR)) as usize;
        (0..n)
            .map(|i| (amplitude * (TAU * 300.0 * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    fn compressor() -> Compressor {
        Compressor {
            threshold_db: -24.0,
            ratio: 3.0,
            knee_db: 10.0,
            max_reduction_db: 12.0,
        }
    }

    fn expander() -> Expander {
        Expander {
            threshold_db: -50.0,
            ratio: 2.0,
            knee_db: 8.0,
            range_db: 12.0,
        }
    }

    #[test]
    fn a_compressor_leaves_quiet_material_alone_and_pulls_loud_material_down() {
        let c = compressor();
        assert_eq!(c.gain_db(-60.0), 0.0);
        assert_eq!(c.gain_db(-40.0), 0.0, "well below the knee");
        assert!(c.gain_db(-6.0) < -5.0, "well above it: {}", c.gain_db(-6.0));
        // Gain falls monotonically as level rises.
        let mut previous = 1.0;
        for level in -60..0 {
            let gain = c.gain_db(f64::from(level));
            assert!(gain <= previous + 1e-12, "not monotonic at {level} dB");
            previous = gain;
        }
    }

    #[test]
    fn a_compressor_honours_its_ratio_above_the_knee() {
        let c = compressor();
        // 3:1 means 12 dB more input past the threshold gives 4 dB more output,
        // so the gain moves by −8 dB over that span. Both points are clear of
        // the knee below and of the 12 dB reduction limit above.
        let a = c.gain_db(-19.0);
        let b = c.gain_db(-7.0);
        assert!((b - a - -8.0).abs() < 0.01, "{a} then {b}");
    }

    #[test]
    fn a_compressor_never_exceeds_its_reduction_limit() {
        let c = compressor();
        assert!((c.gain_db(60.0) - -12.0).abs() < 1e-12);
    }

    #[test]
    fn the_knee_is_smooth_where_a_hard_corner_would_kink() {
        let c = compressor();
        // Sample the gain finely across the knee; the second difference should
        // stay small, which a corner's would not.
        let step = 0.05;
        let at = |x: f64| c.gain_db(-24.0 + x);
        for i in -80..80 {
            let x = f64::from(i) * step;
            let curvature = (at(x - step) - 2.0 * at(x) + at(x + step)).abs();
            assert!(curvature < 0.01, "kink at {x} dB past the threshold");
        }
    }

    #[test]
    fn an_expander_leaves_loud_material_alone_and_pushes_quiet_material_down() {
        let e = expander();
        assert_eq!(e.gain_db(-20.0), 0.0);
        assert_eq!(e.gain_db(-46.0), 0.0, "just above the knee");
        assert!(e.gain_db(-70.0) < -10.0, "{}", e.gain_db(-70.0));
    }

    #[test]
    fn an_expander_is_bounded_so_it_is_not_a_gate() {
        let e = expander();
        // Digital silence still gets the room left in it, 12 dB down.
        assert!((e.gain_db(-240.0) - -12.0).abs() < 1e-12);
    }

    #[test]
    fn compressing_a_loud_tone_attenuates_it() {
        let input = vec![tone(2.0, 0.5)];
        let result = apply(&input, SR, &compressor(), &Timing::default());

        assert_eq!(result.channels.len(), 1);
        assert_eq!(result.channels[0].len(), input[0].len());
        assert!(result.max_reduction_db > 3.0, "{}", result.max_reduction_db);
        assert!(result.mean_gain_db < -3.0, "{}", result.mean_gain_db);
        assert!(result.active_fraction > 0.9, "{}", result.active_fraction);

        let peak_in = input[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let peak_out = result.channels[0]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak_out < peak_in, "{peak_out} vs {peak_in}");
    }

    #[test]
    fn a_quiet_tone_passes_a_compressor_untouched() {
        let input = vec![tone(1.0, 0.001)]; // −60 dBFS
        let result = apply(&input, SR, &compressor(), &Timing::default());
        assert_eq!(result.max_reduction_db, 0.0);
        assert!(result.active_fraction < 0.01);
        for (a, b) in result.channels[0].iter().zip(&input[0]) {
            assert!((a - b).abs() < 1e-7);
        }
    }

    #[test]
    fn both_channels_get_the_same_gain() {
        // The stereo-image argument. A loud left and a silent right must not
        // pull the two apart: the gain is one number, detected on the sum.
        let loud = tone(1.0, 0.5);
        let quiet = tone(1.0, 0.01);
        let result = apply(
            &[loud.clone(), quiet.clone()],
            SR,
            &compressor(),
            &Timing::default(),
        );

        for i in (0..loud.len()).step_by(997) {
            let applied = 10f64.powf(f64::from(result.gain_db[i]) / 20.0);
            assert!((f64::from(result.channels[0][i]) - f64::from(loud[i]) * applied).abs() < 1e-6);
            assert!(
                (f64::from(result.channels[1][i]) - f64::from(quiet[i]) * applied).abs() < 1e-6
            );
        }
    }

    #[test]
    fn the_gain_takes_the_attack_time_to_arrive() {
        // The detector is delay-free, so a step into a loud passage is not
        // caught instantly — that is what an attack time is.
        let mut samples = tone(0.5, 0.005);
        samples.extend(tone(0.5, 0.5));
        let step = (0.5 * f64::from(SR)) as usize;

        let result = apply(&[samples], SR, &compressor(), &Timing::default());
        assert!(
            result.gain_db[step + 10].abs() < 1.0,
            "the gain should still be near unity 10 samples in"
        );
        // 33 ms down constant, so most of the way there after ~100 ms.
        let settled = result.gain_db[step + (0.15 * f64::from(SR)) as usize];
        assert!(settled < -3.0, "settled at {settled} dB");
    }

    #[test]
    fn the_gain_moves_smoothly_rather_than_in_steps() {
        let mut samples = tone(0.3, 0.005);
        samples.extend(tone(0.3, 0.5));
        let result = apply(&[samples], SR, &compressor(), &Timing::default());
        let worst = result
            .gain_db
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // A hundredth of a dB between neighbouring samples is inaudible; a
        // smoother that was not working would show whole dB here.
        assert!(worst < 0.01, "biggest single-sample jump {worst} dB");
    }

    #[test]
    fn a_ratio_of_one_does_nothing() {
        let flat = Compressor {
            ratio: 1.0,
            ..compressor()
        };
        for level in -80..0 {
            assert_eq!(flat.gain_db(f64::from(level)), 0.0);
        }
    }

    #[test]
    fn an_empty_signal_is_handled_without_dividing_by_zero() {
        let result = apply(&[Vec::new()], SR, &compressor(), &Timing::default());
        assert!(result.gain_db.is_empty());
        assert_eq!(result.mean_gain_db, 0.0);
        assert_eq!(result.active_fraction, 0.0);
        assert_eq!(result.max_reduction_db, 0.0);
    }

    #[test]
    fn digital_silence_is_left_at_digital_silence() {
        let result = apply(&[vec![0.0f32; 4_800]], SR, &expander(), &Timing::default());
        assert!(result.channels[0].iter().all(|s| *s == 0.0));
    }
}
