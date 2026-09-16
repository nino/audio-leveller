//! What a stage is.
//!
//! A stage is pure: the same signal and the same parameters give the same
//! output, which is what makes per-stage caching and golden-file tests
//! possible. It takes audio, decides for itself whether it has anything to do,
//! and says in its report what it decided.

use std::sync::Arc;

use leveller_dsp::Signal;
use serde_json::Value;

use crate::analysis::Analyzer;

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error("stage \"{stage}\" was given parameters it does not understand: {source}")]
    Params {
        stage: &'static str,
        source: serde_json::Error,
    },
    #[error("{0}")]
    Failed(String),
}

/// What a stage hands back.
pub struct StageOutput {
    pub signal: Arc<Signal>,
    /// Whatever the stage decided and did, as JSON.
    pub report: Value,
    /// Extra signals produced alongside the main output, keyed by a short name
    /// — the leveller emits `roomtone`. Written out as sibling files, so a
    /// stage added later gets an output file without anything else changing.
    /// Keys must be unique across the whole chain.
    pub extras: Vec<(String, Signal)>,
}

impl StageOutput {
    /// A stage that decided to do nothing, handing back the signal it was
    /// given.
    ///
    /// The `Arc` is passed through rather than copied, and the runner checks
    /// pointer identity — which is what makes a fully bypassed chain provably
    /// bit-identical to its input rather than merely very close.
    pub fn unchanged(signal: Arc<Signal>, report: Value) -> Self {
        Self {
            signal,
            report,
            extras: Vec::new(),
        }
    }

    pub fn new(signal: Signal, report: Value) -> Self {
        Self {
            signal: Arc::new(signal),
            report,
            extras: Vec::new(),
        }
    }

    pub fn with_extra(mut self, name: impl Into<String>, signal: Signal) -> Self {
        self.extras.push((name.into(), signal));
        self
    }
}

/// Everything a stage can ask about the audio it was handed.
pub struct StageContext<'a> {
    /// Measurements of the signal *as it enters this stage*.
    ///
    /// Stages must use this rather than measuring the original file: an earlier
    /// stage may have changed the spectrum or the noise floor out from under
    /// them.
    pub analysis: &'a Analyzer,
    /// The untouched decoded signal, at the chain's working rate, for the
    /// stages that genuinely need original audio — room-tone harvesting has to
    /// happen before a denoiser deletes the room tone.
    pub source: &'a Arc<Signal>,
    /// Measurements of [`source`](Self::source).
    pub source_analysis: &'a Analyzer,
    progress: &'a mut dyn FnMut(f64),
}

impl StageContext<'_> {
    pub fn new<'a>(
        analysis: &'a Analyzer,
        source: &'a Arc<Signal>,
        source_analysis: &'a Analyzer,
        progress: &'a mut dyn FnMut(f64),
    ) -> StageContext<'a> {
        StageContext {
            analysis,
            source,
            source_analysis,
            progress,
        }
    }

    /// Report progress within this stage, as a fraction in [0, 1].
    pub fn progress(&mut self, fraction: f64) {
        (self.progress)(fraction.clamp(0.0, 1.0));
    }
}

pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    /// One line, shown by `--list-stages` and in the app's chain list.
    fn description(&self) -> &'static str;
    /// Parameter defaults, which also define the parameter shape for callers.
    fn default_params(&self) -> Value;
    /// Sample rate this stage must run at.
    ///
    /// When any enabled stage names one, the chain converts once on the way in
    /// and once on the way out. Stages that work at any rate — the leveller
    /// derives its filter coefficients analytically — leave this alone and cost
    /// nothing.
    fn required_sample_rate(&self) -> Option<u32> {
        None
    }
    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError>;
}

/// Deserialise a stage's parameters from the merged JSON, naming the stage when
/// it goes wrong.
pub fn params_of<P: serde::de::DeserializeOwned>(
    stage: &'static str,
    params: &Value,
) -> Result<P, StageError> {
    serde_json::from_value(params.clone()).map_err(|source| StageError::Params { stage, source })
}

/// Serialise a stage's report, which cannot fail for any report type here.
pub fn report_of<R: serde::Serialize>(report: &R) -> Value {
    serde_json::to_value(report).unwrap_or(Value::Null)
}
