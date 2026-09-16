//! True-peak measurement, per ITU-R BS.1770 Annex 2.
//!
//! A sample peak is not the peak. Between two samples the waveform an actual
//! converter reconstructs can overshoot both of them, so a signal measuring
//! −1 dBFS sample-peak can reach +0.5 dBTP on the way out — or clip inside a
//! lossy encoder, which reconstructs the same inter-sample content. The
//! standard's answer is to oversample by at least 4× through a band-limited
//! interpolator and take the peak of that.
//!
//! The filter here is the standard's 4×, 48-tap design, as four 12-tap phases.

/// BS.1770-4 Annex 2 Table 3: 4× oversampling, 12 taps per phase.
const PHASES: [[f64; TAPS]; 4] = [
    [
        0.001708984375,
        0.010986328125,
        -0.0196533203125,
        0.033203125,
        -0.0594482421875,
        0.1373291015625,
        0.97216796875,
        -0.102294921875,
        0.047607421875,
        -0.026611328125,
        0.014892578125,
        -0.00830078125,
    ],
    [
        -0.0291748046875,
        0.029296875,
        -0.0517578125,
        0.089111328125,
        -0.16650390625,
        0.465087890625,
        0.77978515625,
        -0.2003173828125,
        0.1015625,
        -0.0582275390625,
        0.0330810546875,
        -0.0189208984375,
    ],
    [
        -0.0189208984375,
        0.0330810546875,
        -0.0582275390625,
        0.1015625,
        -0.2003173828125,
        0.77978515625,
        0.465087890625,
        -0.16650390625,
        0.089111328125,
        -0.0517578125,
        0.029296875,
        -0.0291748046875,
    ],
    [
        -0.00830078125,
        0.014892578125,
        -0.026611328125,
        0.047607421875,
        -0.102294921875,
        0.97216796875,
        0.1373291015625,
        -0.0594482421875,
        0.033203125,
        -0.0196533203125,
        0.010986328125,
        0.001708984375,
    ],
];

const TAPS: usize = 12;

/// Offset from the current sample to the first tap, so the 12-tap window is
/// centred on it.
const LEAD: usize = TAPS / 2 - 1;

/// Largest interpolated magnitude in the neighbourhood of sample `i`.
///
/// Samples near either end of the buffer see the window run off the edge; those
/// taps are treated as zero, which is what padding with silence would give.
#[inline]
fn local_peak(channel: &[f32], i: usize) -> f32 {
    let n = channel.len();
    let mut peak = 0.0f64;
    for phase in &PHASES {
        let mut acc = 0.0f64;
        for (t, coefficient) in phase.iter().enumerate() {
            // i + t - LEAD, without going negative on the way.
            let Some(index) = (i + t).checked_sub(LEAD) else {
                continue;
            };
            if index >= n {
                break;
            }
            acc += coefficient * f64::from(channel[index]);
        }
        peak = peak.max(acc.abs());
    }
    peak as f32
}

/// Peak of the 4×-oversampled signal, as a linear amplitude.
///
/// The standard also specifies a 12.5 kHz low-pass and a 48 kHz working rate.
/// For deciding how hard to limit, the oversampled peak alone is the number
/// that matters, and leaving the low-pass out makes the measurement slightly
/// conservative rather than slightly optimistic.
pub fn true_peak_linear(channels: &[Vec<f32>]) -> f32 {
    channels
        .iter()
        .flat_map(|channel| (0..channel.len()).map(move |i| local_peak(channel, i)))
        .fold(0.0f32, f32::max)
}

/// True peak in dBFS (dBTP). −∞ for digital silence.
pub fn true_peak_dbfs(channels: &[Vec<f32>]) -> f64 {
    let peak = f64::from(true_peak_linear(channels));
    if peak > 0.0 {
        20.0 * peak.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Per-sample envelope of the 4×-oversampled peak: for each input sample, the
/// largest interpolated magnitude near it, across all channels.
///
/// This is what a true-peak limiter needs. The gain curve it applies lives in
/// the sample domain, but it has to respond to overshoots that live *between*
/// the samples it can touch — so each sample's peak is spread to its immediate
/// neighbours, which are the samples whose gain can still pull it down.
pub fn true_peak_envelope(channels: &[Vec<f32>]) -> Vec<f32> {
    let n = channels.first().map_or(0, Vec::len);
    let mut out = vec![0.0f32; n];

    for channel in channels {
        for i in 0..n {
            let local = local_peak(channel, i);
            if local > out[i] {
                out[i] = local;
            }
            if i > 0 && local > out[i - 1] {
                out[i - 1] = local;
            }
            if i + 1 < n && local > out[i + 1] {
                out[i + 1] = local;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    #[test]
    fn a_sine_on_the_grid_measures_its_own_amplitude() {
        // 1 kHz at 48 kHz: 48 samples per cycle, plenty for the interpolator to
        // find the crest without much help.
        let signal: Vec<f32> = (0..48_000)
            .map(|i| (0.5 * (TAU * 1000.0 * i as f64 / 48_000.0).sin()) as f32)
            .collect();
        let peak = true_peak_linear(&[signal]);
        assert!((peak - 0.5).abs() < 0.005, "{peak}");
    }

    #[test]
    fn an_inter_sample_overshoot_is_found_where_a_sample_peak_misses_it() {
        // The textbook case: a sine at exactly a quarter of the sample rate,
        // offset by an eighth of a cycle, so every sample lands at ±1/√2 and
        // every crest falls exactly halfway between two of them. Sample-peak
        // says −3 dBFS; the converter will produce full scale.
        let signal: Vec<f32> = (0..2_048)
            .map(|i| (TAU * i as f64 / 4.0 + TAU / 8.0).sin() as f32)
            .collect();
        let sample_peak = signal.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let true_peak = true_peak_linear(&[signal]);
        let half_power = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (sample_peak - half_power).abs() < 1e-3,
            "sample peak {sample_peak}"
        );
        assert!(true_peak > 0.98, "true peak {true_peak}");
    }

    #[test]
    fn silence_is_minus_infinity() {
        assert_eq!(true_peak_dbfs(&[vec![0.0; 1000]]), f64::NEG_INFINITY);
        assert_eq!(true_peak_linear(&[vec![0.0; 1000]]), 0.0);
    }

    #[test]
    fn an_empty_signal_measures_nothing_rather_than_panicking() {
        assert_eq!(true_peak_linear(&[]), 0.0);
        assert!(true_peak_envelope(&[]).is_empty());
        assert_eq!(true_peak_linear(&[Vec::new()]), 0.0);
    }

    #[test]
    fn the_peak_is_taken_across_channels() {
        let tone = |amplitude: f64| -> Vec<f32> {
            (0..4_800)
                .map(|i| (amplitude * (TAU * 1000.0 * i as f64 / 48_000.0).sin()) as f32)
                .collect()
        };
        let both = true_peak_linear(&[tone(0.1), tone(0.9)]);
        assert!((both - 0.9).abs() < 0.01, "{both}");
    }

    #[test]
    fn the_envelope_reaches_the_peak_and_spreads_to_the_neighbours() {
        let mut signal = vec![0.0f32; 64];
        signal[32] = 1.0;
        let envelope = true_peak_envelope(&[signal]);

        assert_eq!(envelope.len(), 64);
        let peak = envelope.iter().fold(0.0f32, |m, v| m.max(*v));
        assert!(peak > 0.9, "{peak}");
        // An isolated impulse rings through the whole 12-tap window, so the
        // envelope is non-zero on both sides of it.
        assert!(envelope[30] > 0.0 && envelope[34] > 0.0);
    }

    #[test]
    fn the_envelope_never_undersells_the_overall_peak() {
        let signal: Vec<f32> = (0..2_000)
            .map(|i| (0.8 * (TAU * 7_000.0 * i as f64 / 48_000.0).sin()) as f32)
            .collect();
        let envelope = true_peak_envelope(std::slice::from_ref(&signal));
        let overall = true_peak_linear(&[signal]);
        let from_envelope = envelope.iter().fold(0.0f32, |m, v| m.max(*v));
        assert!((from_envelope - overall).abs() < 1e-6);
    }
}
