//! The currency every stage deals in: bare audio.
//!
//! Deliberately not a file. Bit depth and sample format belong to the thing on
//! disk, and live with the IO code, so no stage ever has to think about
//! encoding.

/// De-interleaved audio: one buffer per channel, all the same length.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    sample_rate: u32,
    channels: Vec<Vec<f32>>,
}

impl Signal {
    /// # Panics
    /// If the channels are not all the same length. A ragged signal has no
    /// meaningful duration, and every consumer here assumes one.
    pub fn new(sample_rate: u32, channels: Vec<Vec<f32>>) -> Self {
        let len = channels.first().map_or(0, Vec::len);
        assert!(
            channels.iter().all(|c| c.len() == len),
            "channels differ in length"
        );
        Self {
            sample_rate,
            channels,
        }
    }

    /// `frames` frames of silence.
    pub fn silence(sample_rate: u32, channel_count: usize, frames: usize) -> Self {
        Self::new(sample_rate, vec![vec![0.0; frames]; channel_count])
    }

    /// A single channel.
    pub fn mono(sample_rate: u32, samples: Vec<f32>) -> Self {
        Self::new(sample_rate, vec![samples])
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of sample frames, per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn channels(&self) -> &[Vec<f32>] {
        &self.channels
    }

    pub fn channels_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.channels
    }

    pub fn channel(&self, index: usize) -> &[f32] {
        &self.channels[index]
    }

    pub fn channel_mut(&mut self, index: usize) -> &mut [f32] {
        &mut self.channels[index]
    }

    /// Give up the channel buffers, avoiding a copy when the caller is going to
    /// rebuild a signal from them anyway.
    pub fn into_channels(self) -> Vec<Vec<f32>> {
        self.channels
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.len() as f64 / f64::from(self.sample_rate)
        }
    }

    /// A new signal of the same shape and rate, filled with silence.
    pub fn like(&self) -> Self {
        Self::silence(self.sample_rate, self.channel_count(), self.len())
    }

    /// Every channel summed and divided by the channel count.
    ///
    /// Analysis that has no business being stereo — silence detection, the
    /// long-term average spectrum — runs on this rather than on channel 0, so a
    /// hard-panned recording is not measured on the empty side.
    pub fn to_mono(&self) -> Vec<f32> {
        match self.channels.as_slice() {
            [] => Vec::new(),
            [only] => only.clone(),
            channels => {
                let scale = 1.0 / channels.len() as f32;
                (0..self.len())
                    .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() * scale)
                    .collect()
            }
        }
    }

    /// Highest absolute sample across all channels, as a linear amplitude.
    pub fn peak(&self) -> f32 {
        self.channels
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Highest absolute sample across all channels, in dBFS.
    ///
    /// Digital silence is −∞ dB, and says so.
    pub fn peak_dbfs(&self) -> f64 {
        let peak = f64::from(self.peak());
        if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            f64::NEG_INFINITY
        }
    }

    /// Multiply every sample by `gain`.
    pub fn scale(&mut self, gain: f32) {
        for channel in &mut self.channels {
            for sample in channel.iter_mut() {
                *sample *= gain;
            }
        }
    }
}

/// Convert a linear amplitude to decibels, with digital silence at −∞.
pub fn to_db(amplitude: f64) -> f64 {
    if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Convert decibels to a linear amplitude.
pub fn from_db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_knows_its_shape() {
        let s = Signal::new(48_000, vec![vec![0.0; 480], vec![0.0; 480]]);
        assert_eq!(s.sample_rate(), 48_000);
        assert_eq!(s.len(), 480);
        assert_eq!(s.channel_count(), 2);
        assert!((s.duration_secs() - 0.01).abs() < 1e-12);
    }

    #[test]
    #[should_panic(expected = "channels differ in length")]
    fn ragged_channels_are_rejected() {
        Signal::new(48_000, vec![vec![0.0; 10], vec![0.0; 11]]);
    }

    #[test]
    fn mono_folding_averages_rather_than_taking_the_first_channel() {
        let s = Signal::new(48_000, vec![vec![1.0, 1.0], vec![0.0, -1.0]]);
        assert_eq!(s.to_mono(), vec![0.5, 0.0]);
    }

    #[test]
    fn a_lone_channel_is_folded_by_copying_it() {
        let s = Signal::mono(48_000, vec![0.25, -0.5]);
        assert_eq!(s.to_mono(), vec![0.25, -0.5]);
    }

    #[test]
    fn peak_looks_at_every_channel_and_both_signs() {
        let s = Signal::new(48_000, vec![vec![0.1, 0.2], vec![0.0, -0.8]]);
        assert!((s.peak() - 0.8).abs() < 1e-7);
        assert!((s.peak_dbfs() - -1.9382).abs() < 1e-3);
    }

    #[test]
    fn digital_silence_peaks_at_minus_infinity() {
        assert_eq!(
            Signal::silence(48_000, 1, 16).peak_dbfs(),
            f64::NEG_INFINITY
        );
        assert_eq!(to_db(0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn decibels_round_trip() {
        for db in [-60.0, -18.0, -0.5, 0.0, 6.0] {
            assert!((to_db(from_db(db)) - db).abs() < 1e-12);
        }
    }

    #[test]
    fn an_empty_signal_has_no_duration_and_does_not_divide_by_zero() {
        let s = Signal::new(0, vec![]);
        assert!(s.is_empty());
        assert_eq!(s.duration_secs(), 0.0);
        assert!(s.to_mono().is_empty());
    }
}
