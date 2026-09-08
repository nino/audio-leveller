//! The chain runner: hand each stage the audio the one before it produced.
//!
//! Two things it does that are easy to get wrong.
//!
//! **Sample-rate conversion is lazy.** Rather than forcing everything to a
//! canonical 48 kHz, conversion happens only when an enabled stage actually
//! demands a rate. A chain of rate-agnostic stages on a 44.1 kHz file
//! therefore does no conversion at all, and a fully bypassed chain is
//! bit-identical to its input.
//!
//! **Analysis follows the audio.** Each stage gets an analyzer over the signal
//! as it arrives, not over the original file, because a stage that changes the
//! spectrum invalidates earlier measurements. The stages that truly need the
//! untouched original get it separately.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use leveller_dsp::{Signal, resample};
use serde_json::Value;

use crate::analysis::{Analyzer, Measurement};
use crate::registry::Registry;
use crate::stage::{StageContext, StageError};

/// A stage as a caller asked for it: which one, on or off, with what
/// parameters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageSpec {
    pub name: String,
    /// A disabled stage still appears in the report, marked bypassed.
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
}

fn yes() -> bool {
    true
}

impl StageSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            params: serde_json::Map::new(),
        }
    }

    pub fn bypassed(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn with_params(mut self, params: serde_json::Map<String, Value>) -> Self {
        self.params = params;
        self
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageReport {
    pub name: String,
    pub enabled: bool,
    /// Fully resolved parameters: the defaults with the overrides merged in.
    pub params: Value,
    pub elapsed_ms: f64,
    /// Stage-specific detail. Null when the stage was bypassed.
    pub report: Value,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineReport {
    /// Rate of the file on disk.
    pub source_sample_rate: u32,
    /// Rate the chain actually ran at — differs only when a stage demands it.
    pub working_sample_rate: u32,
    pub channels: usize,
    pub duration_sec: f64,
    pub resampled: bool,
    pub input: Measurement,
    pub output: Measurement,
    pub stages: Vec<StageReport>,
    /// Names of the extra signals produced, such as `roomtone`.
    pub extras: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress<'a> {
    /// Name of the stage currently running.
    pub stage: &'a str,
    /// Its index among the enabled stages.
    pub index: usize,
    /// How many stages will run.
    pub total: usize,
    /// Overall completion, in [0, 1].
    pub overall: f64,
}

#[derive(Debug)]
pub struct PipelineResult {
    pub signal: Arc<Signal>,
    pub extras: HashMap<String, Signal>,
    pub report: PipelineReport,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("unknown stage \"{name}\" (registered: {known})")]
    UnknownStage { name: String, known: String },
    #[error("stages disagree on sample rate ({rates}); mid-chain conversion is not supported")]
    MixedRates { rates: String },
    #[error("stage \"{stage}\" produced a duplicate extra output \"{key}\"")]
    DuplicateExtra { stage: String, key: String },
    #[error(transparent)]
    Stage(#[from] StageError),
}

fn resample_signal(signal: &Arc<Signal>, to_rate: u32) -> Arc<Signal> {
    if signal.sample_rate() == to_rate {
        return signal.clone();
    }
    Arc::new(Signal::new(
        to_rate,
        resample(signal.channels(), signal.sample_rate(), to_rate),
    ))
}

/// What rate the chain runs at.
///
/// Mixed requirements are refused rather than silently resampled between every
/// stage: no stage needs that yet, and guessing would hide the cost.
fn working_rate(
    specs: &[StageSpec],
    registry: &Registry,
    source_rate: u32,
) -> Result<u32, PipelineError> {
    let mut required: Vec<u32> = specs
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            registry
                .get(&s.name)
                .and_then(|st| st.required_sample_rate())
        })
        .collect();
    required.sort_unstable();
    required.dedup();

    match required.as_slice() {
        [] => Ok(source_rate),
        [only] => Ok(*only),
        many => Err(PipelineError::MixedRates {
            rates: many
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// Merge a spec's overrides over a stage's defaults, one level deep — which is
/// what a parameter object is.
fn merged_params(defaults: Value, overrides: &serde_json::Map<String, Value>) -> Value {
    let Value::Object(mut merged) = defaults else {
        return defaults;
    };
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    Value::Object(merged)
}

/// Run a chain.
pub fn run(
    input: &Arc<Signal>,
    specs: &[StageSpec],
    registry: &Registry,
    mut on_progress: impl FnMut(Progress),
) -> Result<PipelineResult, PipelineError> {
    for spec in specs {
        if registry.get(&spec.name).is_none() {
            return Err(PipelineError::UnknownStage {
                name: spec.name.clone(),
                known: registry.names().join(", "),
            });
        }
    }

    let source_rate = input.sample_rate();
    let rate = working_rate(specs, registry, source_rate)?;

    // Convert once on the way in; everything downstream runs at `rate`.
    let source = resample_signal(input, rate);
    let source_analysis = Analyzer::new(source.clone());

    let enabled_count = specs.iter().filter(|s| s.enabled).count();
    let mut completed = 0usize;

    let mut signal = source.clone();
    let mut extras: Vec<(String, Signal)> = Vec::new();
    let mut stage_reports = Vec::with_capacity(specs.len());

    for spec in specs {
        let stage = registry.get(&spec.name).expect("checked above");
        let params = merged_params(stage.default_params(), &spec.params);

        if !spec.enabled {
            stage_reports.push(StageReport {
                name: stage.name().to_string(),
                enabled: false,
                params,
                elapsed_ms: 0.0,
                report: Value::Null,
            });
            continue;
        }

        let index = completed;
        let mut emit = |fraction: f64| {
            on_progress(Progress {
                stage: stage.name(),
                index,
                total: enabled_count,
                overall: if enabled_count == 0 {
                    1.0
                } else {
                    (index as f64 + fraction.clamp(0.0, 1.0)) / enabled_count as f64
                },
            });
        };
        emit(0.0);

        let analysis = Analyzer::new(signal.clone());
        let started = Instant::now();
        let output = {
            let mut ctx = StageContext::new(&analysis, &source, &source_analysis, &mut emit);
            stage.render(signal.clone(), &params, &mut ctx)?
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        for (key, extra) in output.extras {
            if extras.iter().any(|(existing, _)| *existing == key) {
                return Err(PipelineError::DuplicateExtra {
                    stage: stage.name().to_string(),
                    key,
                });
            }
            extras.push((key, extra));
        }

        signal = output.signal;
        stage_reports.push(StageReport {
            name: stage.name().to_string(),
            enabled: true,
            params,
            elapsed_ms,
            report: output.report,
        });

        completed += 1;
        emit(1.0);
    }

    // Back to the file's own rate, so a 44.1 kHz input stays a 44.1 kHz output.
    let output_signal = resample_signal(&signal, source_rate);
    let output_extras: HashMap<String, Signal> = extras
        .into_iter()
        .map(|(key, extra)| {
            let converted = resample_signal(&Arc::new(extra), source_rate);
            (key, (*converted).clone())
        })
        .collect();

    let mut extra_names: Vec<String> = output_extras.keys().cloned().collect();
    extra_names.sort();

    let report = PipelineReport {
        source_sample_rate: source_rate,
        working_sample_rate: rate,
        channels: input.channel_count(),
        duration_sec: input.duration_secs(),
        resampled: rate != source_rate,
        input: source_analysis.measure(),
        // A fully bypassed chain hands back the same signal — do not measure
        // the same audio twice.
        output: if Arc::ptr_eq(&output_signal, &source) {
            source_analysis.measure()
        } else {
            Analyzer::new(output_signal.clone()).measure()
        },
        stages: stage_reports,
        extras: extra_names,
    };

    Ok(PipelineResult {
        signal: output_signal,
        extras: output_extras,
        report,
    })
}
