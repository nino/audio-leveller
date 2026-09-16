//! Small things more than one stage needs.

use leveller_dsp::Signal;
use leveller_dsp::denoise::SampleRange;
use leveller_dsp::loudness::Weighted;
use leveller_dsp::silence::SilenceRegion;

/// Mean loudness across the pause regions — the noise floor, measured rather
/// than assumed.
///
/// Averaged in power and weighted by length, so a long quiet pause counts for
/// more than a short one, which is what "the floor of this recording" means.
pub fn pause_loudness(signal: &Signal, pauses: &[SampleRange]) -> f64 {
    if pauses.is_empty() {
        return f64::NEG_INFINITY;
    }
    let weighted = Weighted::new(signal.channels(), signal.sample_rate());

    let mut total_power = 0.0;
    let mut total_samples = 0usize;
    for pause in pauses {
        let length = pause.end.saturating_sub(pause.start);
        if length == 0 {
            continue;
        }
        let loudness = weighted.loudness_of_range(pause.start, pause.end);
        if !loudness.is_finite() {
            continue;
        }
        total_power += 10f64.powf(loudness / 10.0) * length as f64;
        total_samples += length;
    }

    if total_samples > 0 {
        10.0 * (total_power / total_samples as f64).log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The silence regions as plain sample ranges.
pub fn pause_ranges(regions: &[SilenceRegion]) -> Vec<SampleRange> {
    regions.iter().map(|r| r.start..r.end).collect()
}

/// The speech between the silences: the gaps, edge to edge.
///
/// Note this is not the leveller's segmentation, which cuts at each pause's
/// *midpoint* so a gain ramp has somewhere to happen. Spectral measurement
/// wants the opposite — only material that is certainly speech — so it takes
/// the gaps as they stand.
pub fn speech_ranges(regions: &[SilenceRegion], length: usize) -> Vec<SampleRange> {
    let mut ranges = Vec::new();
    let mut at = 0usize;
    for silence in regions {
        if silence.start > at {
            ranges.push(at..silence.start);
        }
        at = silence.end;
    }
    if at < length {
        ranges.push(at..length);
    }
    ranges
}

#[cfg(test)]
// `&[a..b]` here is a one-element slice of ranges, which is what the pause
// measurement takes; clippy reads it as a mistyped `vec![a; b]`.
#[expect(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn regions(pairs: &[(usize, usize)]) -> Vec<SilenceRegion> {
        pairs
            .iter()
            .map(|(start, end)| SilenceRegion {
                start: *start,
                end: *end,
                mid: (start + end) / 2,
            })
            .collect()
    }

    #[test]
    fn speech_is_the_gaps_between_the_silences() {
        let found = speech_ranges(&regions(&[(100, 200), (500, 600)]), 1_000);
        assert_eq!(found, vec![0..100, 200..500, 600..1_000]);
    }

    #[test]
    fn a_silence_at_either_end_leaves_no_empty_range() {
        assert_eq!(speech_ranges(&regions(&[(0, 100)]), 500), vec![100..500]);
        assert_eq!(speech_ranges(&regions(&[(400, 500)]), 500), vec![0..400]);
        assert!(speech_ranges(&regions(&[(0, 500)]), 500).is_empty());
    }

    #[test]
    fn a_file_with_no_silence_is_all_speech() {
        assert_eq!(speech_ranges(&[], 500), vec![0..500]);
    }

    /// A tone, since K-weighting high-passes a constant away to nothing.
    fn tone(n: usize, amplitude: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amplitude * (std::f64::consts::TAU * 400.0 * i as f64 / 48_000.0).sin() as f32)
            .collect()
    }

    #[test]
    fn the_floor_of_a_quiet_pause_measures_quiet() {
        // Two recordings rather than two halves of one. Splicing levels
        // together puts a step at the join, and a step is broadband enough to
        // lift the very measurement this is checking — by 25 dB, as it turned
        // out.
        let loud = pause_loudness(&Signal::mono(48_000, tone(48_000, 0.5)), &[0..10_000]);
        let quiet = pause_loudness(&Signal::mono(48_000, tone(48_000, 0.0005)), &[0..10_000]);
        assert!((loud - quiet - 60.0).abs() < 0.5, "{quiet} against {loud}");
    }

    #[test]
    fn with_no_pauses_there_is_no_floor_to_report() {
        let signal = Signal::mono(48_000, tone(48_000, 0.5));
        assert_eq!(pause_loudness(&signal, &[]), f64::NEG_INFINITY);
        assert_eq!(pause_loudness(&signal, &[100..100]), f64::NEG_INFINITY);
    }

    #[test]
    fn two_pauses_at_one_level_measure_that_level() {
        let signal = Signal::mono(48_000, tone(48_000, 0.02));
        let one = pause_loudness(&signal, &[1_000..11_000]);
        let both = pause_loudness(&signal, &[1_000..11_000, 20_000..40_000]);
        assert!((one - both).abs() < 0.1, "{one} against {both}");
    }

    #[test]
    fn a_longer_pause_at_the_same_level_carries_more_weight() {
        // Length-weighting, checked where power-averaging cannot explain it: a
        // long quiet stretch and a short loud one, with the long one made
        // longer. The answer must move toward the quiet end.
        // A tone that fades up at the very end, so the "loud" pause is loud
        // without a step anywhere.
        let mut samples = tone(96_000, 0.001);
        for (i, sample) in samples[90_000..].iter_mut().enumerate() {
            *sample *= 1.0 + 300.0 * (i as f32 / 6_000.0);
        }
        let signal = Signal::mono(48_000, samples);

        let short_quiet = pause_loudness(&signal, &[0..5_000, 95_000..96_000]);
        let long_quiet = pause_loudness(&signal, &[0..85_000, 95_000..96_000]);
        assert!(
            long_quiet < short_quiet,
            "more quiet material should pull the floor down: {long_quiet} against {short_quiet}"
        );
    }
}
