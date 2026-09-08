//! BS.1770 / EBU R128 loudness measurement.
//!
//! Everything here works on *pre-filtered* (K-weighted) channels, so the
//! expensive filtering happens once and any sub-range can then be measured for
//! the cost of a sum of squares. That matters: the leveller asks for the
//! loudness of every speech segment in a file, and a naive implementation
//! K-weights the whole recording once per question.

use crate::biquad::kweighting;

/// The BS.1770 loudness calibration offset, in dB.
const OFFSET: f64 = -0.691;

/// Absolute silence gate, in LUFS. Blocks quieter than this never count.
const ABSOLUTE_GATE: f64 = -70.0;

/// Relative gate, in LU below the ungated mean loudness.
const RELATIVE_GATE: f64 = -10.0;

/// K-weighted channels, ready to be measured over and over.
///
/// Holding this rather than the raw channels is the whole point of the module:
/// construct once, measure freely.
#[derive(Clone, Debug)]
pub struct Weighted {
    channels: Vec<Vec<f32>>,
    sample_rate: u32,
}

impl Weighted {
    /// K-weight every channel.
    pub fn new(channels: &[Vec<f32>], sample_rate: u32) -> Self {
        Self {
            channels: channels
                .iter()
                .map(|c| kweighting::apply(c, f64::from(sample_rate)))
                .collect(),
            sample_rate,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The filtered channels, for callers that need the samples themselves —
    /// silence detection runs on the K-weighted signal, not the raw one.
    pub fn channels(&self) -> &[Vec<f32>] {
        &self.channels
    }

    /// Weighted mean square ("z" in the spec) over the half-open range
    /// `[start, end)`. Out-of-range ends are clamped.
    pub fn mean_square(&self, start: usize, end: usize) -> f64 {
        let end = end.min(self.len());
        if start >= end {
            return 0.0;
        }
        let n = (end - start) as f64;
        self.channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let acc: f64 = channel[start..end]
                    .iter()
                    .map(|s| f64::from(*s) * f64::from(*s))
                    .sum();
                channel_weight(index) * (acc / n)
            })
            .sum()
    }

    /// Ungated loudness over `[start, end)`, in LUFS.
    pub fn loudness_of_range(&self, start: usize, end: usize) -> f64 {
        loudness_from_mean_square(self.mean_square(start, end))
    }

    /// Ungated loudness of the whole signal.
    pub fn loudness(&self) -> f64 {
        self.loudness_of_range(0, self.len())
    }

    /// Gated integrated loudness over `[start, end)`, in LUFS, by the BS.1770
    /// block-gating procedure: 400 ms blocks at a 100 ms hop, blocks below the
    /// absolute −70 LUFS gate dropped, then blocks below 10 LU under the mean
    /// of the survivors dropped as well.
    ///
    /// A range too short to hold one block falls back to plain ungated
    /// loudness — the gate has nothing to gate.
    pub fn integrated_over(&self, start: usize, end: usize) -> f64 {
        let end = end.min(self.len());
        let block = (0.4 * f64::from(self.sample_rate)).round() as usize;
        let hop = (0.1 * f64::from(self.sample_rate)).round() as usize;

        if start >= end || end - start < block || hop == 0 {
            return self.loudness_of_range(start, end);
        }

        let blocks: Vec<f64> = (start..)
            .step_by(hop)
            .take_while(|s| s + block <= end)
            .map(|s| self.mean_square(s, s + block))
            .collect();

        let above_absolute = |z: &f64| loudness_from_mean_square(*z) >= ABSOLUTE_GATE;

        let absolute: Vec<f64> = blocks.iter().copied().filter(above_absolute).collect();
        if absolute.is_empty() {
            return f64::NEG_INFINITY;
        }
        let absolute_mean = absolute.iter().sum::<f64>() / absolute.len() as f64;
        let relative_threshold = loudness_from_mean_square(absolute_mean) + RELATIVE_GATE;

        let survivors: Vec<f64> = absolute
            .into_iter()
            .filter(|z| loudness_from_mean_square(*z) >= relative_threshold)
            .collect();
        if survivors.is_empty() {
            return f64::NEG_INFINITY;
        }
        loudness_from_mean_square(survivors.iter().sum::<f64>() / survivors.len() as f64)
    }

    /// Gated integrated loudness of the whole signal, in LUFS.
    pub fn integrated(&self) -> f64 {
        self.integrated_over(0, self.len())
    }

    /// Loudness range (EBU Tech 3342), in LU.
    ///
    /// The spread between a recording's quiet and loud passages, measured on
    /// three-second blocks: the 10th to 95th percentile of the blocks that
    /// survive a gate 20 LU below the 95th. It answers the question integrated
    /// loudness cannot — whether a programme sitting on target does so evenly,
    /// or by averaging a shout and a mumble.
    ///
    /// Which is to say it is what tells the compressor whether it has anything
    /// to do.
    pub fn range(&self) -> f64 {
        let window = f64::from(self.sample_rate).round() as usize * 3;
        let hop = f64::from(self.sample_rate).round() as usize;
        if self.len() < window || hop == 0 {
            return 0.0;
        }

        let mut blocks: Vec<f64> = (0..)
            .step_by(hop)
            .take_while(|s| s + window <= self.len())
            .map(|s| self.loudness_of_range(s, s + window))
            .filter(|v| *v >= ABSOLUTE_GATE)
            .collect();
        if blocks.len() < 2 {
            return 0.0;
        }
        blocks.sort_by(f64::total_cmp);

        let gate = percentile(&blocks, 0.95) - 20.0;
        let gated: Vec<f64> = blocks.into_iter().filter(|v| *v > gate).collect();
        if gated.len() < 2 {
            return 0.0;
        }
        percentile(&gated, 0.95) - percentile(&gated, 0.10)
    }
}

/// Index-based percentile of an already-sorted slice, matching EBU Tech 3342's
/// definition rather than interpolating between neighbours.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let index = ((sorted.len() as f64 * p) as usize).min(sorted.len() - 1);
    sorted[index]
}

/// Per-channel weighting from BS.1770: L/R/C weight 1.0, surround ~1.41.
///
/// The full spec also excludes LFE; for the mono and stereo spoken word this
/// pipeline sees, that distinction has never come up.
fn channel_weight(index: usize) -> f64 {
    if index >= 3 { 1.41 } else { 1.0 }
}

/// Convert a weighted mean square to loudness in LUFS.
pub fn loudness_from_mean_square(z: f64) -> f64 {
    if z <= 0.0 {
        f64::NEG_INFINITY
    } else {
        OFFSET + 10.0 * z.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine(freq: f64, secs: f64, sample_rate: u32, amplitude: f64) -> Vec<f32> {
        let n = (secs * f64::from(sample_rate)) as usize;
        (0..n)
            .map(|i| (amplitude * (TAU * freq * i as f64 / f64::from(sample_rate)).sin()) as f32)
            .collect()
    }

    const SR: u32 = 48_000;

    #[test]
    fn a_full_scale_sine_lands_where_the_arithmetic_says_it_should() {
        // RMS of a full-scale sine is −3 dBFS, K-weighting is ~+0.7 dB at
        // 1 kHz, and the calibration offset takes 0.691 back off.
        let w = Weighted::new(&[sine(1000.0, 1.0, SR, 1.0)], SR);
        let l = w.loudness();
        assert!(l > -5.0 && l < -2.0, "{l}");
    }

    #[test]
    fn doubling_a_mono_signal_into_stereo_adds_three_db() {
        let s = sine(1000.0, 1.0, SR, 0.5);
        let mono = Weighted::new(std::slice::from_ref(&s), SR).loudness();
        let stereo = Weighted::new(&[s.clone(), s], SR).loudness();
        assert!((stereo - mono - 3.0103).abs() < 0.01, "{stereo} vs {mono}");
    }

    #[test]
    fn halving_the_amplitude_costs_six_db() {
        let loud = Weighted::new(&[sine(1000.0, 1.0, SR, 0.5)], SR).loudness();
        let quiet = Weighted::new(&[sine(1000.0, 1.0, SR, 0.25)], SR).loudness();
        assert!((loud - quiet - 6.0206).abs() < 0.01);
    }

    #[test]
    fn silence_is_minus_infinity_rather_than_a_very_large_negative_number() {
        let w = Weighted::new(&[vec![0.0; SR as usize]], SR);
        assert_eq!(w.loudness(), f64::NEG_INFINITY);
        assert_eq!(w.integrated(), f64::NEG_INFINITY);
    }

    #[test]
    fn the_relative_gate_ignores_the_silence_between_the_speech() {
        // Two seconds of tone, then eight of silence. Ungated, the silence
        // drags the average down by ~7 dB; gated, it is dropped entirely and
        // the answer is the loudness of the tone.
        let mut signal = sine(1000.0, 2.0, SR, 0.5);
        signal.extend(std::iter::repeat_n(0.0, SR as usize * 8));
        let w = Weighted::new(&[signal], SR);

        let tone_only = Weighted::new(&[sine(1000.0, 2.0, SR, 0.5)], SR).integrated();
        // Not exact: the blocks straddling the boundary are part tone, part
        // silence, and survive the gate at a lower level than the tone alone.
        assert!(
            (w.integrated() - tone_only).abs() < 0.5,
            "gated {} vs tone {tone_only}",
            w.integrated()
        );
        assert!(
            w.loudness() < w.integrated() - 5.0,
            "the ungated mean should sag"
        );
    }

    #[test]
    fn a_range_too_short_for_a_block_falls_back_to_ungated_loudness() {
        // 200 ms: half of one 400 ms block, so there is nothing to gate.
        let w = Weighted::new(&[sine(1000.0, 0.2, SR, 0.5)], SR);
        assert_eq!(w.integrated(), w.loudness());
    }

    #[test]
    fn a_steady_programme_has_essentially_no_loudness_range() {
        let w = Weighted::new(&[sine(1000.0, 12.0, SR, 0.5)], SR);
        assert!(w.range() < 0.2, "{}", w.range());
    }

    #[test]
    fn a_shout_and_a_mumble_show_up_as_range() {
        let mut signal = sine(1000.0, 8.0, SR, 0.5);
        signal.extend(sine(1000.0, 8.0, SR, 0.05));
        let range = Weighted::new(&[signal], SR).range();
        // 20 dB apart in amplitude, but the 20 LU gate clips the bottom of the
        // spread, so expect a large range rather than exactly 20.
        assert!(range > 10.0, "{range}");
    }

    #[test]
    fn a_signal_shorter_than_one_range_window_has_no_range() {
        let w = Weighted::new(&[sine(1000.0, 2.0, SR, 0.5)], SR);
        assert_eq!(w.range(), 0.0);
    }

    #[test]
    fn measuring_a_sub_range_agrees_with_measuring_it_alone() {
        // The whole point of pre-filtering: a segment measured inside a longer
        // recording must give the same answer as that segment on its own. It is
        // not exactly equal — the filter carries state across the boundary —
        // but a tenth of a dB is the honest tolerance.
        let mut signal = vec![0.0f32; SR as usize];
        signal.extend(sine(1000.0, 2.0, SR, 0.4));
        let inside = Weighted::new(&[signal], SR).loudness_of_range(SR as usize, SR as usize * 3);
        let alone = Weighted::new(&[sine(1000.0, 2.0, SR, 0.4)], SR).loudness();
        assert!((inside - alone).abs() < 0.1, "{inside} vs {alone}");
    }

    #[test]
    fn an_end_past_the_signal_is_clamped_rather_than_panicking() {
        let w = Weighted::new(&[sine(1000.0, 0.5, SR, 0.5)], SR);
        assert_eq!(w.loudness_of_range(0, usize::MAX), w.loudness());
        assert_eq!(w.mean_square(100, 10), 0.0);
    }
}
