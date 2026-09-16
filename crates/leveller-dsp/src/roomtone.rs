//! Room-tone bed generator.
//!
//! Mirrors what an editor does by hand: look through the silences, pick the
//! cleanest slice of each — no clicks, no breaths — then concatenate them with
//! small equal-power crossfades into a seamless bed. One gain, the
//! length-weighted mean of the gains the speech segments got, puts the bed at
//! the same level as the processed voice.

use crate::silence::SilenceRegion;

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct RoomToneOptions {
    /// Trim this much off each end of a silence — speech tails, breath onsets.
    pub edge_trim_sec: f64,
    /// Ignore a silence whose usable core is shorter than this.
    pub min_clip_sec: f64,
    /// Cap what a single silence contributes, so one gap cannot dominate.
    pub max_clip_sec: f64,
    /// Crossfade between concatenated clips.
    pub crossfade_sec: f64,
    /// Fade in and out at the very start and end of the bed.
    pub edge_fade_sec: f64,
    /// Step used when scanning a silence for its cleanest window.
    pub scan_hop_sec: f64,
    /// Drop clips more than this many dB dirtier than the best one.
    pub keep_margin_db: f64,
    /// Loop the bed, with crossfades, until it reaches at least this long.
    pub min_duration_sec: f64,
}

impl Default for RoomToneOptions {
    fn default() -> Self {
        Self {
            edge_trim_sec: 0.15,
            min_clip_sec: 0.3,
            max_clip_sec: 1.0,
            crossfade_sec: 0.05,
            edge_fade_sec: 0.02,
            scan_hop_sec: 0.02,
            keep_margin_db: 6.0,
            min_duration_sec: 10.0,
        }
    }
}

/// A slice of the original file that made it into the bed.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RoomToneClip {
    /// Source sample range in the original file.
    pub start: usize,
    pub end: usize,
    /// Cleanliness score. Lower is cleaner.
    pub score: f64,
}

#[derive(Clone, Debug)]
pub struct RoomTone {
    pub channels: Vec<Vec<f32>>,
    pub length: usize,
    /// The distinct source clips used, in file order.
    pub clips: Vec<RoomToneClip>,
    /// How many clip instances were laid down — at least `clips.len()`, more
    /// once the bed loops.
    pub instances: usize,
    /// Gain applied to the whole bed, in dB.
    pub gain_db: f64,
}

const EPS: f64 = 1e-9;

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Dirtiness of a mono-summed window. Lower is cleaner.
///
/// Room tone is the quietest *steady* part of a silence, so loudness is the
/// primary term — a breath is simply louder than the noise floor. On top of
/// that:
///
/// - **clicks**: a localised transient shows up as one ~10 ms block whose peak
///   towers over the *median* block peak. The median ignores the outlier, which
///   is what separates a real click from noise's own natural peakiness.
/// - **breaths and swells**: a high coefficient of variation of short-window
///   RMS.
///
/// Near-digital silence is treated as maximally clean.
///
/// Public because the labelled-fixture evaluation scores against it directly.
pub fn score_range(channels: &[Vec<f32>], sample_rate: u32, range: std::ops::Range<usize>) -> f64 {
    let n = range.end.saturating_sub(range.start);
    if n == 0 || channels.is_empty() {
        return f64::INFINITY;
    }
    let channel_count = channels.len() as f64;

    // ~10 ms blocks: each block's RMS, for loudness and steadiness, and its
    // peak, for click detection.
    let block_len = ((0.01 * f64::from(sample_rate)).round() as usize).max(1);
    let mut block_rms = Vec::new();
    let mut block_peak = Vec::new();
    let mut sum_squares = 0.0f64;
    let mut peak_all = 0.0f64;

    let mut start = range.start;
    while start < range.end {
        let end = (start + block_len).min(range.end);
        let mut block_squares = 0.0f64;
        let mut peak = 0.0f64;
        for i in start..end {
            let mono = channels.iter().map(|c| f64::from(c[i])).sum::<f64>() / channel_count;
            block_squares += mono * mono;
            peak = peak.max(mono.abs());
        }
        sum_squares += block_squares;
        peak_all = peak_all.max(peak);
        block_rms.push((block_squares / (end - start) as f64).sqrt());
        block_peak.push(peak);
        start = end;
    }

    // Effectively silent: the cleanest possible bed material.
    if peak_all < 1e-4 {
        return -140.0;
    }

    let rms_db = 20.0 * ((sum_squares / n as f64).sqrt() + EPS).log10();

    // Click term: the loudest block peak against the median block peak, in dB.
    let loudest = block_peak.iter().copied().fold(0.0f64, f64::max);
    let click_db = 20.0 * (loudest / (median(&block_peak) + EPS)).log10();

    // Swell term: coefficient of variation of the block RMS.
    let mean = block_rms.iter().sum::<f64>() / block_rms.len() as f64;
    let variance = block_rms
        .iter()
        .map(|r| (r - mean) * (r - mean))
        .sum::<f64>()
        / block_rms.len() as f64;
    let cov = variance.sqrt() / (mean + EPS);

    rms_db + 1.5 * click_db + 25.0 * cov
}

/// The cleanest window of `window_len` samples within `core`.
fn cleanest_window(
    channels: &[Vec<f32>],
    sample_rate: u32,
    core: std::ops::Range<usize>,
    window_len: usize,
    hop: usize,
) -> RoomToneClip {
    if core.end - core.start <= window_len {
        return RoomToneClip {
            start: core.start,
            end: core.end,
            score: score_range(channels, sample_rate, core.clone()),
        };
    }

    let mut best = RoomToneClip {
        start: core.start,
        end: core.start + window_len,
        score: f64::INFINITY,
    };
    let mut start = core.start;
    while start + window_len <= core.end {
        let score = score_range(channels, sample_rate, start..start + window_len);
        if score < best.score {
            best = RoomToneClip {
                start,
                end: start + window_len,
                score,
            };
        }
        start += hop;
    }
    best
}

/// A segment as far as the bed's level is concerned.
pub trait GainedSegment {
    fn range(&self) -> std::ops::Range<usize>;
    fn gain_db(&self) -> f64;
    fn is_speech(&self) -> bool;
}

/// Length-weighted mean of the speech segments' gains, in dB.
///
/// Length-weighted because a two-second aside and a ten-minute passage should
/// not have equal say in where the bed sits.
pub fn weighted_mean_gain_db(segments: &[impl GainedSegment]) -> f64 {
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for segment in segments {
        if !segment.is_speech() {
            continue;
        }
        let length = (segment.range().end - segment.range().start) as f64;
        numerator += segment.gain_db() * length;
        denominator += length;
    }
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

/// Build the bed from the silences of an untouched recording.
///
/// Untouched matters: harvesting has to happen before a denoiser deletes the
/// room tone there is to harvest.
pub fn build(
    channels: &[Vec<f32>],
    sample_rate: u32,
    silences: &[SilenceRegion],
    gain_db: f64,
    options: &RoomToneOptions,
) -> RoomTone {
    let rate = f64::from(sample_rate);
    let trim = (options.edge_trim_sec * rate).round() as usize;
    let min_clip = (options.min_clip_sec * rate).round() as usize;
    let max_clip = (options.max_clip_sec * rate).round() as usize;
    let hop = ((options.scan_hop_sec * rate).round() as usize).max(1);

    // 1. The cleanest window of each silence.
    let candidates: Vec<RoomToneClip> = silences
        .iter()
        .filter_map(|s| {
            let core_start = s.start + trim;
            let core_end = s.end.checked_sub(trim)?;
            if core_end.checked_sub(core_start)? < min_clip {
                return None;
            }
            let window_len = (core_end - core_start).min(max_clip);
            Some(cleanest_window(
                channels,
                sample_rate,
                core_start..core_end,
                window_len,
                hop,
            ))
        })
        .collect();

    let Some(best_score) = candidates.iter().map(|c| c.score).reduce(f64::min) else {
        // No silence held a usable core: there is no bed to build, and saying
        // so is better than emitting one second of whatever was nearest.
        return RoomTone {
            channels: vec![Vec::new(); channels.len()],
            length: 0,
            clips: Vec::new(),
            instances: 0,
            gain_db,
        };
    };

    // 2. Drop clips more than keep_margin_db dirtier than the cleanest — a
    //    louder patch of room, a breath, a click — but never drop them all.
    let cutoff = best_score + options.keep_margin_db;
    let mut clips: Vec<RoomToneClip> = candidates
        .iter()
        .copied()
        .filter(|c| c.score <= cutoff)
        .collect();
    if clips.is_empty() {
        clips = vec![
            *candidates
                .iter()
                .min_by(|a, b| a.score.total_cmp(&b.score))
                .expect("candidates is not empty"),
        ];
    }

    // Extract each distinct clip's audio once.
    let clip_audio: Vec<Vec<Vec<f32>>> = clips
        .iter()
        .map(|c| {
            channels
                .iter()
                .map(|ch| ch[c.start..c.end].to_vec())
                .collect()
        })
        .collect();

    // 3. The play sequence: loop through the clips until the bed is long
    //    enough. Every other instance is time-reversed, so repeats of the same
    //    clip stay decorrelated and identical material never crossfades into
    //    itself and builds up coherently.
    let target = (options.min_duration_sec * rate).round() as usize;
    let crossfade = (options.crossfade_sec * rate).round() as usize;

    let mut sequence: Vec<Vec<Vec<f32>>> = Vec::new();
    let mut projected = 0usize;
    for p in 0..1_000 {
        let base = &clip_audio[p % clips.len()];
        let audio: Vec<Vec<f32>> = if p % 2 == 1 {
            base.iter()
                .map(|c| c.iter().rev().copied().collect())
                .collect()
        } else {
            base.clone()
        };
        let len = audio[0].len();
        let overlap = match sequence.last() {
            None => 0,
            Some(previous) => crossfade.min(previous[0].len() / 2).min(len / 2),
        };
        sequence.push(audio);
        projected += len - overlap;
        if projected >= target {
            break;
        }
    }

    // 4. Concatenate with equal-power crossfades and edge fades.
    let mut bed = concat_crossfade(
        &sequence,
        crossfade,
        (options.edge_fade_sec * rate).round() as usize,
        channels.len(),
    );
    let gain = 10f64.powf(gain_db / 20.0) as f32;
    for channel in &mut bed {
        for sample in channel {
            *sample *= gain;
        }
    }

    RoomTone {
        length: bed.first().map_or(0, Vec::len),
        channels: bed,
        clips,
        instances: sequence.len(),
        gain_db,
    }
}

/// Overlap-add a sequence of clips with equal-power crossfades between them.
fn concat_crossfade(
    sequence: &[Vec<Vec<f32>>],
    crossfade: usize,
    edge_fade: usize,
    channel_count: usize,
) -> Vec<Vec<f32>> {
    if sequence.is_empty() {
        return vec![Vec::new(); channel_count];
    }

    let lengths: Vec<usize> = sequence.iter().map(|clip| clip[0].len()).collect();
    let overlaps: Vec<usize> = (0..sequence.len())
        .map(|i| {
            if i == 0 {
                0
            } else {
                crossfade.min(lengths[i - 1] / 2).min(lengths[i] / 2)
            }
        })
        .collect();

    let mut offsets = vec![0usize; sequence.len()];
    for i in 1..sequence.len() {
        offsets[i] = offsets[i - 1] + lengths[i - 1] - overlaps[i];
    }
    let total = offsets[sequence.len() - 1] + lengths[sequence.len() - 1];

    let mut out = vec![vec![0.0f32; total]; channel_count];
    let last = sequence.len() - 1;

    #[expect(
        clippy::needless_range_loop,
        reason = "k indexes every channel of the clip and the output alike"
    )]
    for (i, clip) in sequence.iter().enumerate() {
        let len = lengths[i];
        let fade_in_overlap = overlaps[i];
        let fade_out_overlap = if i < last { overlaps[i + 1] } else { 0 };
        let fade_in = if i == 0 { edge_fade.min(len) } else { 0 };
        let fade_out = if i == last { edge_fade.min(len) } else { 0 };

        for k in 0..len {
            let mut w = 1.0f64;
            if fade_in_overlap > 0 && k < fade_in_overlap {
                w *=
                    (std::f64::consts::FRAC_PI_2 * (k as f64 + 0.5) / fade_in_overlap as f64).sin();
            }
            if fade_out_overlap > 0 && k >= len - fade_out_overlap {
                let into = (k - (len - fade_out_overlap)) as f64 + 0.5;
                w *= (std::f64::consts::FRAC_PI_2 * into / fade_out_overlap as f64).cos();
            }
            if fade_in > 0 && k < fade_in {
                w *= (k as f64 + 0.5) / fade_in as f64;
            }
            if fade_out > 0 && k >= len - fade_out {
                w *= (len - k) as f64 / fade_out as f64;
            }

            let at = offsets[i] + k;
            for (c, channel) in out.iter_mut().enumerate() {
                channel[at] += (f64::from(clip[c][k]) * w) as f32;
            }
        }
    }

    out
}
