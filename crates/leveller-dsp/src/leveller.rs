//! The leveller: normalise every speech segment to a target loudness, with the
//! gain ramped across each silence so nothing jumps.
//!
//! This is the stage the whole project is named for. Silence detection says
//! where the segments are, loudness measurement says how loud each one is, and
//! the gain between them is a straight line drawn through the pause — which is
//! the one place in a recording where a level change has nothing to be heard
//! against.

use crate::loudness::Weighted;
use crate::roomtone::{self, GainedSegment, RoomTone, RoomToneOptions};
use crate::signal::{Signal, from_db};
use crate::silence::{self, SilenceOptions, SilenceRegion};
use crate::truepeak::true_peak_envelope;

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase", default))]
pub struct LevellerOptions {
    /// Target loudness for every segment, in LUFS.
    ///
    /// −18, not the −23 of EBU R128. R128 is a broadcast *delivery* target, and
    /// it was the right default when this was a tool for levelling material on
    /// its way into someone else's chain. It is now the chain, delivering
    /// finished spoken word, where −18 is the ordinary target and the one the
    /// reference this project is measured against uses. Broadcast delivery is
    /// still one `--target -23` away.
    pub target_lufs: f64,
    /// Clamp per-segment gain to ±this many dB, which guards against boosting
    /// a quiet segment's noise floor into the programme.
    pub max_gain_db: f64,
    /// Ceiling for the output limiter, in dBTP.
    pub ceiling_db: f64,
    pub silence: SilenceOptions,
    pub roomtone: RoomToneOptions,
}

impl Default for LevellerOptions {
    fn default() -> Self {
        Self {
            target_lufs: -18.0,
            max_gain_db: 30.0,
            ceiling_db: -1.0,
            silence: SilenceOptions::default(),
            roomtone: RoomToneOptions::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct SegmentReport {
    pub start: usize,
    pub end: usize,
    pub loudness_lufs: f64,
    pub gain_db: f64,
    /// False for the near-silent head, tail and room-tone segments, which are
    /// not boosted toward the target.
    pub is_speech: bool,
}

impl GainedSegment for SegmentReport {
    fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }
    fn gain_db(&self) -> f64 {
        self.gain_db
    }
    fn is_speech(&self) -> bool {
        self.is_speech
    }
}

#[derive(Clone, Debug)]
pub struct RoomToneReport {
    pub clips: Vec<roomtone::RoomToneClip>,
    pub instances: usize,
    pub gain_db: f64,
    pub length: usize,
    pub duration_sec: f64,
}

#[derive(Clone, Debug)]
pub struct LevellerReport {
    pub sample_rate: u32,
    pub integrated_lufs: f64,
    pub threshold_lufs: f64,
    pub floor_lufs: f64,
    pub segments: Vec<SegmentReport>,
    pub silences: Vec<SilenceRegion>,
    pub limiter_gain_reduction_db: f64,
    pub room_tone: RoomToneReport,
}

#[derive(Clone, Debug)]
pub struct LevellerResult {
    pub signal: Signal,
    /// The room-tone bed. Empty when no usable silence was found.
    pub room_tone: Signal,
    pub report: LevellerReport,
}

/// A breakpoint in the piecewise-linear gain envelope.
#[derive(Clone, Copy, Debug)]
struct Breakpoint {
    at: usize,
    db: f64,
}

/// Give every non-speech segment the gain of its nearest speech neighbour, by
/// segment distance, so quiet head, tail and room-tone regions ride along
/// smoothly instead of being boosted toward the loudness target.
///
/// With no speech segment at all, everything stays at unity.
fn fill_non_speech_gains(speech_gain: &[f64], is_speech: &[bool]) -> Vec<f64> {
    (0..speech_gain.len())
        .map(|i| {
            if is_speech[i] {
                return speech_gain[i];
            }
            (0..speech_gain.len())
                .filter(|j| is_speech[*j])
                .min_by_key(|j| j.abs_diff(i))
                .map_or(0.0, |j| speech_gain[j])
        })
        .collect()
}

/// The gain envelope: constant across each speech body, ramping linearly across
/// each silence from the gain on its left to the gain on its right.
fn build_envelope(
    segment_gains: &[f64],
    silences: &[SilenceRegion],
    length: usize,
) -> Vec<Breakpoint> {
    let mut points = vec![Breakpoint {
        at: 0,
        db: segment_gains[0],
    }];
    for (i, silence) in silences.iter().enumerate() {
        // Hold the left segment's gain to the start of the silence...
        points.push(Breakpoint {
            at: silence.start,
            db: segment_gains[i],
        });
        // ...then ramp to the right segment's gain across it.
        points.push(Breakpoint {
            at: silence.end,
            db: segment_gains[i + 1],
        });
    }
    points.push(Breakpoint {
        at: length,
        db: *segment_gains.last().expect("at least one segment"),
    });
    points
}

/// Linear gain at a sample, interpolating between breakpoints.
fn gain_at(points: &[Breakpoint], sample: usize) -> f32 {
    // Points are sorted by position, and an envelope has a handful of them, so
    // a linear scan from the end is both simplest and fastest.
    for (i, point) in points.iter().enumerate().rev() {
        if sample >= point.at {
            let Some(next) = points.get(i + 1) else {
                return from_db(point.db) as f32;
            };
            let span = next.at - point.at;
            let t = if span > 0 {
                (sample - point.at) as f64 / span as f64
            } else {
                0.0
            };
            return from_db(point.db + t * (next.db - point.db)) as f32;
        }
    }
    from_db(points[0].db) as f32
}

/// Feed-forward limiter: one gain-reduction curve applied to every channel, a
/// ~5 ms lookahead attack ramp and a one-pole release. Returns the largest
/// reduction applied, in dB.
///
/// The detector runs on the **true peak**, not the sample peak. Between two
/// samples the waveform a converter reconstructs can overshoot both of them, so
/// a sample-peak limiter set to −1 dBFS routinely lets through −0.3 dBTP, and
/// lossy encoders then clip on the way out. Detecting on the 4× oversampled
/// envelope costs one pass and makes the ceiling mean what it says.
///
/// The attack is a backward pass rather than a delay line: with the whole file
/// in memory, easing the gain down *before* each peak is lookahead without the
/// latency bookkeeping. A gain curve that steps down in one sample puts a sharp
/// corner into the waveform — audibly much like clipping — whereas the ramp
/// keeps the curve itself free of high-frequency energy. The pass only ever
/// lowers gain, so the ceiling guarantee survives it.
fn limit(channels: &mut [Vec<f32>], ceiling_db: f64, sample_rate: u32) -> f64 {
    let ceiling = from_db(ceiling_db);
    let length = channels.first().map_or(0, Vec::len);
    let release = (-1.0 / (0.05 * f64::from(sample_rate))).exp(); // ~50 ms
    let attack = (-1.0 / (0.005 * f64::from(sample_rate))).exp(); // ~5 ms

    let peaks = true_peak_envelope(channels);
    let mut gain = vec![1.0f64; length];
    let mut envelope = 1.0f64;
    let mut max_reduction = 1.0f64;

    for (i, peak) in peaks.iter().enumerate() {
        let peak = f64::from(*peak);
        let required = if peak > ceiling { ceiling / peak } else { 1.0 };
        // Clamp to the required reduction at once; release recovers slowly.
        envelope = if required < envelope {
            required
        } else {
            envelope + (1.0 - envelope) * (1.0 - release)
        };
        max_reduction = max_reduction.min(envelope);
        gain[i] = envelope;
    }

    // Backward pass: ease into each reduction ahead of the peak, reaching the
    // required value exactly as the peak arrives.
    for i in (0..length.saturating_sub(1)).rev() {
        let ramped = 1.0 - (1.0 - gain[i + 1]) * attack;
        if ramped < gain[i] {
            gain[i] = ramped;
        }
    }

    for channel in channels.iter_mut() {
        for (sample, g) in channel.iter_mut().zip(&gain) {
            *sample = (f64::from(*sample) * g) as f32;
        }
    }

    if max_reduction < 1.0 {
        -20.0 * max_reduction.log10()
    } else {
        0.0
    }
}

/// Level a recording.
pub fn level(input: &Signal, options: &LevellerOptions) -> LevellerResult {
    let sample_rate = input.sample_rate();
    let length = input.len();

    // K-weight once, and reuse it for every measurement.
    let weighted = Weighted::new(input.channels(), sample_rate);
    let analysis = silence::analyze(&weighted, &options.silence);
    let silences = analysis.regions.clone();

    // Segment boundaries are the file edges plus every silence midpoint.
    let mut boundaries = vec![0usize];
    boundaries.extend(silences.iter().map(|s| s.mid));
    boundaries.push(length);

    // First pass: measure each segment, and decide whether it is real speech. A
    // segment quieter than the silence threshold is room tone, or a quiet head
    // or tail — boosting it to target would only amplify the noise floor.
    let loudness: Vec<f64> = boundaries
        .windows(2)
        .map(|b| weighted.integrated_over(b[0], b[1]))
        .collect();
    let mut is_speech: Vec<bool> = loudness
        .iter()
        .map(|l| l.is_finite() && *l > analysis.threshold_lufs)
        .collect();

    // Degenerate case — one uniform segment, no silence — where the threshold
    // can sit at the programme loudness and demote everything. Fall back to
    // treating every non-silent segment as speech, so the file is still
    // normalised.
    if !is_speech.iter().any(|s| *s) {
        is_speech = loudness.iter().map(|l| l.is_finite()).collect();
    }

    // Speech segments get the gain that brings them to target; non-speech
    // segments inherit their nearest speech neighbour's, so the envelope stays
    // smooth and no noise is boosted.
    let speech_gain: Vec<f64> = loudness
        .iter()
        .map(|l| (options.target_lufs - l).clamp(-options.max_gain_db, options.max_gain_db))
        .collect();
    let segment_gains = fill_non_speech_gains(&speech_gain, &is_speech);

    let segments: Vec<SegmentReport> = boundaries
        .windows(2)
        .enumerate()
        .map(|(i, b)| SegmentReport {
            start: b[0],
            end: b[1],
            loudness_lufs: loudness[i],
            gain_db: segment_gains[i],
            is_speech: is_speech[i],
        })
        .collect();

    // Apply the interpolated envelope.
    let points = build_envelope(&segment_gains, &silences, length);
    let mut channels: Vec<Vec<f32>> = input
        .channels()
        .iter()
        .map(|channel| {
            channel
                .iter()
                .enumerate()
                .map(|(i, s)| s * gain_at(&points, i))
                .collect()
        })
        .collect();

    // Catch any peaks the gain created.
    let limiter_gain_reduction_db = limit(&mut channels, options.ceiling_db, sample_rate);

    // Build the bed from the *original* silence audio, level-matched to the
    // length-weighted mean of the speech gains.
    let mean_gain_db = roomtone::weighted_mean_gain_db(&segments);
    let bed = roomtone::build(
        input.channels(),
        sample_rate,
        &silences,
        mean_gain_db,
        &options.roomtone,
    );

    LevellerResult {
        signal: Signal::new(sample_rate, channels),
        room_tone: Signal::new(sample_rate, bed.channels),
        report: LevellerReport {
            sample_rate,
            integrated_lufs: analysis.integrated_lufs,
            threshold_lufs: analysis.threshold_lufs,
            floor_lufs: analysis.floor_lufs,
            segments,
            silences,
            limiter_gain_reduction_db,
            room_tone: RoomToneReport {
                clips: bed.clips,
                instances: bed.instances,
                gain_db: bed.gain_db,
                length: bed.length,
                duration_sec: bed.length as f64 / f64::from(sample_rate),
            },
        },
    }
}

/// What [`RoomTone`] became once it reached the leveller's report — kept so the
/// type is nameable from outside.
pub type Bed = RoomTone;

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};

    const SR: u32 = 48_000;

    /// The corpus crate depends on this one, so the copy of `leveller-dsp` it
    /// links against is not the copy being tested and its `Signal` is a
    /// different type. Rebuilding from the bare channels crosses that line.
    fn signal_of(speech: &leveller_corpus::Speech) -> Signal {
        Signal::new(SR, speech.signal.channels().to_vec())
    }

    fn two_segments_at(first: f64, second: f64) -> leveller_corpus::Speech {
        synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, first), Spurt::new(4.0, second)],
            pause_sec: 2.0,
            floor_dbfs: -62.0,
            seed: 4242,
            channels: 1,
        })
    }

    fn loudness_of(signal: &Signal, range: std::ops::Range<usize>) -> f64 {
        Weighted::new(signal.channels(), signal.sample_rate())
            .integrated_over(range.start, range.end)
    }

    #[test]
    fn two_segments_recorded_at_different_levels_come_out_at_the_same_one() {
        // The whole point of the thing.
        let speech = two_segments_at(-30.0, -14.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());

        for segment in &speech.segments {
            let measured = loudness_of(&result.signal, segment.start..segment.end);
            assert!(
                (measured - -18.0).abs() < 1.0,
                "segment at {} came out at {measured} LUFS",
                segment.start
            );
        }
    }

    #[test]
    fn the_target_is_a_setting_and_not_a_constant() {
        let speech = two_segments_at(-30.0, -14.0);
        let result = level(
            &signal_of(&speech),
            &LevellerOptions {
                target_lufs: -23.0,
                ..LevellerOptions::default()
            },
        );
        let measured = loudness_of(
            &result.signal,
            speech.segments[0].start..speech.segments[0].end,
        );
        assert!((measured - -23.0).abs() < 1.0, "{measured}");
    }

    #[test]
    fn the_gain_ramps_across_the_pause_rather_than_stepping() {
        // A step in the middle of a pause is the artefact this design exists to
        // avoid: it is inaudible in the pause only because it is gradual.
        let speech = two_segments_at(-30.0, -14.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());

        // Ratio of output to input, sample by sample, is the gain envelope.
        let gain: Vec<f64> = (speech.segments[0].end..speech.segments[1].start)
            .map(|i| {
                f64::from(result.signal.channel(0)[i])
                    / f64::from(signal_of(&speech).channel(0).to_vec()[i])
            })
            .filter(|g| g.is_finite())
            .collect();

        let worst_step = gain
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f64, f64::max);
        let span = gain.iter().copied().fold(0.0f64, f64::max)
            - gain.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(span > 1.0, "the two segments should need different gains");
        assert!(
            worst_step < span / 1_000.0,
            "the envelope stepped by {worst_step} out of a {span} span"
        );
    }

    #[test]
    fn the_output_respects_its_true_peak_ceiling() {
        let speech = two_segments_at(-40.0, -14.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        let peak = crate::truepeak::true_peak_dbfs(result.signal.channels());
        assert!(peak <= -1.0 + 0.05, "{peak} dBTP");
    }

    #[test]
    fn a_lower_ceiling_is_honoured_and_reported() {
        let speech = two_segments_at(-40.0, -8.0);
        let result = level(
            &signal_of(&speech),
            &LevellerOptions {
                ceiling_db: -6.0,
                ..LevellerOptions::default()
            },
        );
        let peak = crate::truepeak::true_peak_dbfs(result.signal.channels());
        assert!(peak <= -6.0 + 0.05, "{peak} dBTP");
        assert!(result.report.limiter_gain_reduction_db > 0.0);
    }

    #[test]
    fn a_quiet_segment_is_not_boosted_past_the_gain_limit() {
        let speech = synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, -60.0), Spurt::new(4.0, -18.0)],
            pause_sec: 2.0,
            ..SpeechOptions::default()
        });
        let result = level(
            &signal_of(&speech),
            &LevellerOptions {
                max_gain_db: 12.0,
                ..LevellerOptions::default()
            },
        );
        for segment in &result.report.segments {
            assert!(segment.gain_db.abs() <= 12.0 + 1e-9, "{segment:?}");
        }
    }

    #[test]
    fn the_pauses_are_not_boosted_toward_the_target() {
        // The failure mode this guards against: treating a pause as a quiet
        // segment and bringing its noise floor up to −18 LUFS.
        let speech = two_segments_at(-30.0, -30.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());

        let pause = speech.segments[0].end + 24_000;
        let before = f64::from(signal_of(&speech).channel(0).to_vec()[pause]);
        let after = f64::from(result.signal.channel(0)[pause]);
        let applied = 20.0 * (after / before).abs().log10();
        // It rides along at the speech's gain, which is ~12 dB, not at whatever
        // would bring the floor to target (~50 dB).
        assert!(applied < 20.0, "the pause was lifted by {applied} dB");
    }

    #[test]
    fn the_report_says_what_was_done_to_each_segment() {
        let speech = two_segments_at(-30.0, -14.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        let report = &result.report;

        assert_eq!(report.sample_rate, SR);
        assert_eq!(report.silences.len(), 3, "head, middle and tail pauses");
        assert_eq!(report.segments.len(), report.silences.len() + 1);
        assert!(report.segments.iter().filter(|s| s.is_speech).count() >= 2);

        // Segment boundaries tile the file without gaps or overlaps.
        assert_eq!(report.segments[0].start, 0);
        assert_eq!(
            report.segments.last().unwrap().end,
            signal_of(&speech).len()
        );
        for pair in report.segments.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        // The quieter segment gets the larger boost.
        let speech_gains: Vec<f64> = report
            .segments
            .iter()
            .filter(|s| s.is_speech)
            .map(|s| s.gain_db)
            .collect();
        assert!(speech_gains[0] > speech_gains[1], "{speech_gains:?}");
    }

    #[test]
    fn a_room_tone_bed_is_harvested_from_the_pauses() {
        let speech = two_segments_at(-24.0, -20.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        let bed = &result.report.room_tone;

        assert!(!bed.clips.is_empty(), "nothing was harvested");
        assert!(bed.duration_sec >= 10.0, "{} s", bed.duration_sec);
        assert_eq!(result.room_tone.len(), bed.length);
        assert_eq!(result.room_tone.channel_count(), 1);

        // Every clip comes from inside a pause, not from the speech.
        for clip in &bed.clips {
            assert!(
                result
                    .report
                    .silences
                    .iter()
                    .any(|s| clip.start >= s.start && clip.end <= s.end),
                "{clip:?} is not inside a silence"
            );
        }
    }

    #[test]
    fn the_bed_holds_a_steady_level_across_its_joins() {
        let speech = two_segments_at(-24.0, -20.0);
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        let bed = result.room_tone.channel(0);

        // Not sample-to-sample continuity — the bed is broadband noise, where
        // neighbouring samples routinely swing the full peak-to-peak span. What
        // a butt join or a linear crossfade would show is a dip in level where
        // two clips meet, so the level is what to measure.
        let block = 240; // 5 ms
        // Skip the bed's own 20 ms fade in and out, which are deliberate.
        let edge = 2_400; // 50 ms
        let rms: Vec<f64> = bed[edge..bed.len() - edge]
            .chunks_exact(block)
            .map(|c| {
                (c.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / block as f64).sqrt()
            })
            .collect();

        let mean = rms.iter().sum::<f64>() / rms.len() as f64;
        let lowest = rms.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            lowest > mean * 0.5,
            "a level dip at a join: {lowest} against a mean of {mean}"
        );
    }

    #[test]
    fn a_recording_with_no_usable_pause_yields_no_bed_rather_than_a_bad_one() {
        let speech = synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, -20.0)],
            pause_sec: 0.05,
            ..SpeechOptions::default()
        });
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        assert!(result.report.room_tone.clips.is_empty());
        assert_eq!(result.room_tone.len(), 0);
    }

    #[test]
    fn a_file_with_no_silence_at_all_is_still_normalised() {
        // The degenerate case: the threshold can sit at the programme loudness
        // and demote every segment, and the fallback is what keeps the file
        // from passing through at unity.
        let speech = synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(6.0, -30.0)],
            pause_sec: 0.0,
            ..SpeechOptions::default()
        });
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        let measured = Weighted::new(result.signal.channels(), SR).integrated();
        assert!((measured - -18.0).abs() < 1.5, "{measured}");
    }

    #[test]
    fn stereo_keeps_its_image() {
        // One gain for both channels: a per-channel gain would move the image
        // in time with the speech.
        let speech = synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, -30.0), Spurt::new(4.0, -14.0)],
            pause_sec: 2.0,
            channels: 2,
            ..SpeechOptions::default()
        });
        let result = level(&signal_of(&speech), &LevellerOptions::default());
        assert_eq!(result.signal.channel_count(), 2);

        for i in (0..signal_of(&speech).len()).step_by(4_099) {
            let left = f64::from(signal_of(&speech).channel(0).to_vec()[i]);
            let right = f64::from(signal_of(&speech).channel(1).to_vec()[i]);
            if left.abs() < 1e-5 || right.abs() < 1e-5 {
                continue;
            }
            let gain_left = f64::from(result.signal.channel(0)[i]) / left;
            let gain_right = f64::from(result.signal.channel(1)[i]) / right;
            assert!(
                (gain_left - gain_right).abs() < 1e-6,
                "at {i}: {gain_left} against {gain_right}"
            );
        }
    }

    #[test]
    fn silence_passes_through_without_a_panic() {
        let result = level(
            &Signal::silence(SR, 1, SR as usize),
            &LevellerOptions::default(),
        );
        assert_eq!(result.signal.len(), SR as usize);
        assert!(result.signal.channel(0).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn an_empty_signal_passes_through_without_a_panic() {
        let result = level(&Signal::mono(SR, Vec::new()), &LevellerOptions::default());
        assert_eq!(result.signal.len(), 0);
        assert_eq!(result.report.segments.len(), 1);
    }
}
