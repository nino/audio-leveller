//! Where the speech stops.
//!
//! A short window slides across the file, K-weighted loudness is measured in
//! each, and a threshold is chosen below which a window counts as silence. Runs
//! of silent windows longer than [`SilenceOptions::min_silence_sec`] become
//! silence regions; everything else is speech.
//!
//! This is what the leveller segments on, and what the room-tone harvester
//! mines, so the regions it returns matter rather more than the threshold it
//! picked to find them.

use crate::loudness::Weighted;

/// How the loudness threshold is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum ThresholdMethod {
    /// A fixed fraction of the way from the noise floor up to the programme.
    Fraction,
    /// The valley between the two clusters in the loudness histogram.
    Otsu,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct SilenceOptions {
    /// Detection window length, in seconds.
    pub window_sec: f64,
    /// Hop between windows, in seconds — the detection resolution.
    pub hop_sec: f64,
    /// Shortest gap that counts as silence.
    pub min_silence_sec: f64,
    pub method: ThresholdMethod,
    /// For [`ThresholdMethod::Fraction`]: how far from the estimated noise
    /// floor up to the integrated loudness the threshold sits.
    pub fraction: f64,
}

impl Default for SilenceOptions {
    fn default() -> Self {
        Self {
            window_sec: 0.1,
            hop_sec: 0.025,
            min_silence_sec: 1.0,
            method: ThresholdMethod::Fraction,
            fraction: 0.25,
        }
    }
}

/// A stretch of the recording with no speech in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SilenceRegion {
    /// First sample of the silence.
    pub start: usize,
    /// One past its last sample.
    pub end: usize,
    /// Midpoint — where a segment boundary goes.
    pub mid: usize,
}

impl SilenceRegion {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug)]
pub struct SilenceAnalysis {
    pub regions: Vec<SilenceRegion>,
    pub threshold_lufs: f64,
    pub floor_lufs: f64,
    pub integrated_lufs: f64,
    /// Per-window loudness, for plotting and for the stages that want to know
    /// how confident the split was.
    pub window_loudness: Vec<f64>,
    pub hop_samples: usize,
    pub window_samples: usize,
}

impl SilenceAnalysis {
    /// The speech between the silences: the complement of `regions`, split at
    /// each region's midpoint rather than at its edges.
    ///
    /// Cutting at the midpoint is what makes a gain ramp across a pause
    /// inaudible — the change happens where there is nothing to hear it in.
    pub fn speech_segments(&self, length: usize) -> Vec<(usize, usize)> {
        let mut segments = Vec::new();
        let mut start = 0usize;
        for region in &self.regions {
            if region.mid > start {
                segments.push((start, region.mid));
            }
            start = region.mid;
        }
        if length > start {
            segments.push((start, length));
        }
        segments
    }
}

/// Index-based percentile of an already-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NEG_INFINITY;
    }
    let index = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Otsu's method: the value that best splits a histogram into two clusters, by
/// maximising the variance between them.
///
/// The bimodal-valley heuristic — it finds the gap between "room" and "voice"
/// without being told where to look, which is the point. Returns a threshold in
/// the same units as the values it was given.
pub fn otsu_threshold(values: &[f64], bins: usize) -> f64 {
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NEG_INFINITY;
    }
    let min = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if min == max {
        return min;
    }

    let width = (max - min) / bins as f64;
    let mut histogram = vec![0usize; bins];
    for v in &finite {
        let bin = (((v - min) / width) as usize).min(bins - 1);
        histogram[bin] += 1;
    }

    let total = finite.len() as f64;
    let sum_all: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, count)| i as f64 * *count as f64)
        .sum();

    let mut weight_below = 0.0;
    let mut sum_below = 0.0;
    let mut best_variance = -1.0;
    let mut best_bin = 0usize;
    for (i, count) in histogram.iter().enumerate() {
        weight_below += *count as f64;
        if weight_below == 0.0 {
            continue;
        }
        let weight_above = total - weight_below;
        if weight_above == 0.0 {
            break;
        }
        sum_below += i as f64 * *count as f64;
        let mean_below = sum_below / weight_below;
        let mean_above = (sum_all - sum_below) / weight_above;
        let between =
            weight_below * weight_above * (mean_below - mean_above) * (mean_below - mean_above);
        if between > best_variance {
            best_variance = between;
            best_bin = i;
        }
    }

    // The upper edge of the winning bin, so the bin itself falls below.
    min + (best_bin + 1) as f64 * width
}

/// The threshold and the noise-floor estimate it was derived from.
pub fn compute_threshold(
    window_loudness: &[f64],
    integrated_lufs: f64,
    options: &SilenceOptions,
) -> (f64, f64) {
    let mut finite: Vec<f64> = window_loudness
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    finite.sort_by(f64::total_cmp);
    let floor = percentile(&finite, 10.0);

    let threshold = match options.method {
        ThresholdMethod::Otsu => otsu_threshold(window_loudness, 128),
        ThresholdMethod::Fraction => floor + options.fraction * (integrated_lufs - floor),
    };
    (threshold, floor)
}

/// Find the silences in a K-weighted signal.
pub fn analyze(weighted: &Weighted, options: &SilenceOptions) -> SilenceAnalysis {
    let length = weighted.len();
    let sample_rate = f64::from(weighted.sample_rate());

    let hop_samples = ((options.hop_sec * sample_rate).round() as usize).max(1);
    let window_samples = ((options.window_sec * sample_rate).round() as usize).max(1);
    let half_window = window_samples / 2;

    let frames = length / hop_samples;
    let window_loudness: Vec<f64> = (0..frames)
        .map(|f| {
            let centre = f * hop_samples + hop_samples / 2;
            let start = centre.saturating_sub(half_window);
            let end = (centre + half_window).min(length);
            weighted.loudness_of_range(start, end)
        })
        .collect();

    let integrated_lufs = weighted.integrated();
    let (threshold, floor) = compute_threshold(&window_loudness, integrated_lufs, options);

    let min_silence_frames = ((options.min_silence_sec / options.hop_sec).ceil() as usize).max(1);
    let mut regions = Vec::new();
    let mut run_start: Option<usize> = None;
    // One past the end, so a run that reaches the end of the file is closed.
    for (f, silent) in window_loudness
        .iter()
        .map(|v| *v < threshold)
        .chain(std::iter::once(false))
        .enumerate()
    {
        match (silent, run_start) {
            (true, None) => run_start = Some(f),
            (false, Some(start)) => {
                if f - start >= min_silence_frames {
                    let start_sample = start * hop_samples;
                    let end_sample = (f * hop_samples).min(length);
                    regions.push(SilenceRegion {
                        start: start_sample,
                        end: end_sample,
                        mid: (start_sample + end_sample) / 2,
                    });
                }
                run_start = None;
            }
            _ => {}
        }
    }

    SilenceAnalysis {
        regions,
        threshold_lufs: threshold,
        floor_lufs: floor,
        integrated_lufs,
        window_loudness,
        hop_samples,
        window_samples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    /// Speech-ish: a tone at `amplitude`, over a constant noise floor.
    fn speech(secs: f64, amplitude: f64) -> Vec<f32> {
        let n = (secs * f64::from(SR)) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64;
                (amplitude * (TAU * 220.0 * t / f64::from(SR)).sin()
                    + 0.0005 * (TAU * 3_000.0 * t / f64::from(SR)).sin()) as f32
            })
            .collect()
    }

    fn room(secs: f64) -> Vec<f32> {
        let n = (secs * f64::from(SR)) as usize;
        (0..n)
            .map(|i| (0.0005 * (TAU * 3_000.0 * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    fn analyse(channel: Vec<f32>, options: &SilenceOptions) -> SilenceAnalysis {
        analyze(&Weighted::new(&[channel], SR), options)
    }

    #[test]
    fn a_pause_between_two_phrases_is_found() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(2.0));
        signal.extend(speech(3.0, 0.3));

        let analysis = analyse(signal, &SilenceOptions::default());
        assert_eq!(analysis.regions.len(), 1, "{:?}", analysis.regions);

        let region = analysis.regions[0];
        let start_sec = region.start as f64 / f64::from(SR);
        let end_sec = region.end as f64 / f64::from(SR);
        assert!((start_sec - 3.0).abs() < 0.15, "starts at {start_sec}");
        assert!((end_sec - 5.0).abs() < 0.15, "ends at {end_sec}");
        assert_eq!(region.mid, (region.start + region.end) / 2);
    }

    #[test]
    fn a_gap_shorter_than_the_minimum_is_not_a_silence() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(0.4)); // well under the 1 s minimum
        signal.extend(speech(3.0, 0.3));

        let analysis = analyse(signal, &SilenceOptions::default());
        assert!(analysis.regions.is_empty(), "{:?}", analysis.regions);
    }

    #[test]
    fn continuous_speech_has_no_silences() {
        let analysis = analyse(speech(6.0, 0.3), &SilenceOptions::default());
        assert!(analysis.regions.is_empty());
    }

    #[test]
    fn a_silence_running_to_the_end_of_the_file_is_still_closed() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(3.0));

        let analysis = analyse(signal, &SilenceOptions::default());
        assert_eq!(analysis.regions.len(), 1);
        let end = analysis.regions[0].end as f64 / f64::from(SR);
        assert!(end > 5.5, "the run should reach the end, got {end}");
    }

    #[test]
    fn segments_are_cut_at_the_middle_of_each_pause() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(2.0));
        signal.extend(speech(3.0, 0.3));
        let length = signal.len();

        let analysis = analyse(signal, &SilenceOptions::default());
        let segments = analysis.speech_segments(length);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].0, 0);
        assert_eq!(segments[0].1, analysis.regions[0].mid);
        assert_eq!(segments[1].0, analysis.regions[0].mid);
        assert_eq!(segments[1].1, length);
    }

    #[test]
    fn a_file_with_no_pauses_is_one_segment() {
        let analysis = analyse(speech(4.0, 0.3), &SilenceOptions::default());
        let length = 4 * SR as usize;
        assert_eq!(analysis.speech_segments(length), vec![(0, length)]);
    }

    #[test]
    fn the_threshold_sits_between_the_floor_and_the_programme() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(2.0));
        signal.extend(speech(3.0, 0.3));

        let analysis = analyse(signal, &SilenceOptions::default());
        assert!(analysis.floor_lufs < analysis.threshold_lufs);
        assert!(analysis.threshold_lufs < analysis.integrated_lufs);
        // A quarter of the way up, by construction.
        let expected =
            analysis.floor_lufs + 0.25 * (analysis.integrated_lufs - analysis.floor_lufs);
        assert!((analysis.threshold_lufs - expected).abs() < 1e-9);
    }

    #[test]
    fn otsu_finds_the_valley_of_a_two_cluster_histogram() {
        let mut values: Vec<f64> = (0..100).map(|i| -60.0 + f64::from(i) * 0.02).collect();
        values.extend((0..100).map(|i| -20.0 + f64::from(i) * 0.02));
        let threshold = otsu_threshold(&values, 128);
        assert!(
            threshold > -59.0 && threshold < -21.0,
            "threshold {threshold} is not in the gap"
        );
    }

    #[test]
    fn otsu_survives_degenerate_input() {
        assert_eq!(otsu_threshold(&[], 128), f64::NEG_INFINITY);
        assert_eq!(
            otsu_threshold(&[f64::NEG_INFINITY; 4], 128),
            f64::NEG_INFINITY
        );
        assert_eq!(otsu_threshold(&[-30.0; 10], 128), -30.0);
    }

    #[test]
    fn the_otsu_method_finds_the_same_pause_the_fraction_method_does() {
        let mut signal = speech(3.0, 0.3);
        signal.extend(room(2.0));
        signal.extend(speech(3.0, 0.3));

        let options = SilenceOptions {
            method: ThresholdMethod::Otsu,
            ..Default::default()
        };
        let analysis = analyse(signal, &options);
        assert_eq!(analysis.regions.len(), 1, "{:?}", analysis.regions);
        let start = analysis.regions[0].start as f64 / f64::from(SR);
        assert!((start - 3.0).abs() < 0.2, "starts at {start}");
    }

    #[test]
    fn an_empty_signal_analyses_to_nothing() {
        let analysis = analyse(Vec::new(), &SilenceOptions::default());
        assert!(analysis.regions.is_empty());
        assert!(analysis.window_loudness.is_empty());
        assert!(analysis.speech_segments(0).is_empty());
    }
}
