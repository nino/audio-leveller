//! Memoised measurement of one signal.
//!
//! K-weighting a long recording is the expensive part of every loudness
//! question, and by the time there are eight stages in the chain several of
//! them want the answer to overlapping ones. An [`Analyzer`] is bound to a
//! single signal and computes each measurement at most once.
//!
//! Analyzers are deliberately *not* shared across stages that modify audio: a
//! stage that changes the spectrum invalidates every measurement taken before
//! it, so the runner hands each stage a fresh one over the signal as it
//! actually arrives.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use leveller_dsp::loudness::Weighted;
use leveller_dsp::silence::{self, SilenceAnalysis, SilenceOptions};
use leveller_dsp::{Signal, to_db};

/// A loudness snapshot, reported before and after the chain.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    pub integrated_lufs: f64,
    pub peak_dbfs: f64,
}

pub struct Analyzer {
    signal: Arc<Signal>,
    weighted: RefCell<Option<Arc<Weighted>>>,
    integrated: RefCell<Option<f64>>,
    peak: RefCell<Option<f64>>,
    range: RefCell<Option<f64>>,
    // Keyed by the options, rendered as a string. There are at most a handful
    // of distinct option sets in a run, so a map beats anything cleverer.
    silence: RefCell<HashMap<String, Arc<SilenceAnalysis>>>,
}

impl Analyzer {
    pub fn new(signal: Arc<Signal>) -> Self {
        Self {
            signal,
            weighted: RefCell::new(None),
            integrated: RefCell::new(None),
            peak: RefCell::new(None),
            range: RefCell::new(None),
            silence: RefCell::new(HashMap::new()),
        }
    }

    pub fn signal(&self) -> &Arc<Signal> {
        &self.signal
    }

    /// The K-weighted channels, computed once and reused by every measurement.
    pub fn weighted(&self) -> Arc<Weighted> {
        let mut slot = self.weighted.borrow_mut();
        slot.get_or_insert_with(|| {
            Arc::new(Weighted::new(
                self.signal.channels(),
                self.signal.sample_rate(),
            ))
        })
        .clone()
    }

    /// Gated integrated loudness of the whole signal, in LUFS.
    pub fn integrated_lufs(&self) -> f64 {
        let mut slot = self.integrated.borrow_mut();
        *slot.get_or_insert_with(|| self.weighted().integrated())
    }

    /// Loudness range, in LU.
    pub fn loudness_range(&self) -> f64 {
        let mut slot = self.range.borrow_mut();
        *slot.get_or_insert_with(|| self.weighted().range())
    }

    /// Highest absolute sample across all channels, in dBFS.
    pub fn peak_dbfs(&self) -> f64 {
        let mut slot = self.peak.borrow_mut();
        *slot.get_or_insert_with(|| to_db(f64::from(self.signal.peak())))
    }

    /// Silence segmentation, memoised per distinct set of options.
    pub fn silence(&self, options: &SilenceOptions) -> Arc<SilenceAnalysis> {
        // The options are a handful of floats and an enum; formatting them is
        // both cheaper and less error-prone than hand-rolling a hash.
        let key = format!("{options:?}");
        self.silence
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Arc::new(silence::analyze(&self.weighted(), options)))
            .clone()
    }

    /// Silence segmentation at the defaults, which is what most stages want.
    pub fn default_silence(&self) -> Arc<SilenceAnalysis> {
        self.silence(&SilenceOptions::default())
    }

    /// The summary that goes into the pipeline report.
    pub fn measure(&self) -> Measurement {
        Measurement {
            integrated_lufs: self.integrated_lufs(),
            peak_dbfs: self.peak_dbfs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{SpeechOptions, Spurt, synthetic_speech};

    fn analyzer() -> Analyzer {
        let speech = synthetic_speech(&SpeechOptions {
            spurts: vec![Spurt::new(3.0, -23.0), Spurt::new(3.0, -23.0)],
            pause_sec: 1.5,
            ..SpeechOptions::default()
        });
        Analyzer::new(Arc::new(Signal::new(
            48_000,
            speech.signal.channels().to_vec(),
        )))
    }

    #[test]
    fn measurements_agree_with_the_dsp_they_wrap() {
        let analyzer = analyzer();
        let direct = Weighted::new(
            analyzer.signal().channels(),
            analyzer.signal().sample_rate(),
        );
        assert_eq!(analyzer.integrated_lufs(), direct.integrated());
        assert_eq!(analyzer.loudness_range(), direct.range());
        assert_eq!(
            analyzer.peak_dbfs(),
            to_db(f64::from(analyzer.signal().peak()))
        );
    }

    #[test]
    fn the_weighting_is_computed_once_and_shared() {
        let analyzer = analyzer();
        assert!(Arc::ptr_eq(&analyzer.weighted(), &analyzer.weighted()));
    }

    #[test]
    fn silence_is_memoised_per_option_set() {
        let analyzer = analyzer();
        let a = analyzer.default_silence();
        let b = analyzer.silence(&SilenceOptions::default());
        assert!(Arc::ptr_eq(&a, &b), "the same options should hit the cache");

        let other = analyzer.silence(&SilenceOptions {
            min_silence_sec: 0.25,
            ..SilenceOptions::default()
        });
        assert!(
            !Arc::ptr_eq(&a, &other),
            "different options, different answer"
        );
    }

    #[test]
    fn the_summary_carries_both_numbers() {
        let analyzer = analyzer();
        let measurement = analyzer.measure();
        assert_eq!(measurement.integrated_lufs, analyzer.integrated_lufs());
        assert_eq!(measurement.peak_dbfs, analyzer.peak_dbfs());
    }

    #[test]
    fn silence_is_found_where_the_pauses_are() {
        let analyzer = analyzer();
        let analysis = analyzer.default_silence();
        assert_eq!(analysis.regions.len(), 3, "head, middle and tail");
    }
}
