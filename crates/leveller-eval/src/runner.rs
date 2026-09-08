//! Running the corpus: render each case, measure it, check it against its
//! bounds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use leveller_dsp::Signal;
use leveller_pipeline::{Registry, run};
use leveller_stages::{ChainOptions, backend::Backends, build_chain};
use serde_json::{Map, Value, json};

use crate::cases::{CaseInput, EvalCase, Expectation};
use crate::metrics::{self, Metrics, References};

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Wav {
        path: PathBuf,
        #[source]
        source: leveller_wav::WavError,
    },
    #[error(transparent)]
    Pipeline(#[from] leveller_pipeline::PipelineError),
    #[error("{path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// One bound, and what the run made of it.
#[derive(Clone, Debug)]
pub struct Check {
    pub expectation: Expectation,
    pub value: f64,
    pub passed: bool,
}

#[derive(Clone, Debug)]
pub struct CaseResult {
    pub name: String,
    pub description: String,
    pub metrics: Metrics,
    pub checks: Vec<Check>,
    pub passed: bool,
    pub elapsed_ms: f64,
    /// Set when the case could not run at all; it is then neither passed nor
    /// failed.
    pub skipped: Option<String>,
}

fn check(metrics: &Metrics, expectations: &[Expectation]) -> Vec<Check> {
    expectations
        .iter()
        .map(|expectation| {
            let Some(value) = metrics.get(expectation.metric).copied() else {
                // A typo in a metric name must fail the run, not silently pass
                // it.
                return Check {
                    expectation: expectation.clone(),
                    value: f64::NAN,
                    passed: false,
                };
            };
            let above = expectation.min.is_none_or(|min| value >= min);
            let below = expectation.max.is_none_or(|max| value <= max);
            Check {
                expectation: expectation.clone(),
                value,
                passed: above && below,
            }
        })
        .collect()
}

/// Render one case and measure it.
pub fn run_case(
    case: &EvalCase,
    input: &CaseInput,
    registry: &Registry,
) -> Result<(Metrics, Arc<Signal>), EvalError> {
    let result = run(&input.input, &case.chain, registry, |_| {})?;

    // The reference goes through the same chain, so scores reflect what the
    // degradation did rather than what the chain was asked to do.
    let reference = match &input.reference {
        Some(reference) => Some(References {
            for_input: reference.clone(),
            for_output: run(reference, &case.chain, registry, |_| {})?.signal,
        }),
        None => None,
    };

    let mut measured = metrics::compute(&metrics::Inputs {
        input: Some(&input.input),
        output: Some(&result.signal),
        reference: reference.as_ref(),
        segments: &input.segments,
        click_positions: &input.click_positions,
        target_lufs: input.target_lufs,
    });

    // Report-derived facts worth asserting on. A stage's own account of what it
    // did belongs in the corpus alongside the measurements taken from the
    // audio: the denoiser measuring that it cost 10 dB of programme is the same
    // finding as the audio being 10 dB quieter, caught one layer earlier.
    measured.insert("resampled".into(), f64::from(u8::from(result.report.resampled)));

    let stage = |name: &str| -> Option<&Value> {
        result
            .report
            .stages
            .iter()
            .find(|s| s.name == name)
            .map(|s| &s.report)
    };
    let number = |report: Option<&Value>, key: &str| -> Option<f64> {
        report?.get(key)?.as_f64()
    };

    if let Some(loss) = number(stage("denoise"), "programmeLossDb") {
        measured.insert("programmeLossDb".into(), loss);
    }
    let compress = stage("compress");
    if let (Some(before), Some(after)) = (
        number(compress, "loudnessRangeBeforeLu"),
        number(compress, "loudnessRangeAfterLu"),
    ) {
        measured.insert("loudnessRangeReductionLu".into(), before - after);
        measured.insert("loudnessRangeAfterLu".into(), after);
    }
    if let Some(reduction) = number(compress, "maxReductionDb") {
        measured.insert("compressorMaxReductionDb".into(), reduction);
    }
    if let Some(reduction) = number(stage("expand"), "floorReductionDb") {
        measured.insert("floorReductionDb".into(), reduction);
    }

    Ok((measured, result.signal))
}

/// Run every case, in order.
pub fn run_all(
    cases: &[EvalCase],
    registry: &Registry,
    mut on_result: impl FnMut(&CaseResult),
    wav_dir: Option<&Path>,
) -> Result<Vec<CaseResult>, EvalError> {
    let mut results = Vec::new();
    for case in cases {
        // A case whose prerequisites are missing is reported as skipped rather
        // than quietly dropped: a check nobody runs must not look like one that
        // passed.
        if let Some(reason) = &case.unavailable {
            let result = CaseResult {
                name: case.name.clone(),
                description: case.description.clone(),
                metrics: Metrics::new(),
                checks: Vec::new(),
                passed: true,
                elapsed_ms: 0.0,
                skipped: Some(reason.clone()),
            };
            on_result(&result);
            results.push(result);
            continue;
        }

        let input = (case.build)();
        let started = Instant::now();
        let (metrics, output) = run_case(case, &input, registry)?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        let checks = check(&metrics, &case.expectations);
        let result = CaseResult {
            name: case.name.clone(),
            description: case.description.clone(),
            passed: checks.iter().all(|c| c.passed),
            metrics,
            checks,
            elapsed_ms,
            skipped: None,
        };

        if let Some(dir) = wav_dir {
            dump_wav(dir, &format!("{}_input", case.name), &input.input)?;
            dump_wav(dir, &format!("{}_output", case.name), &output)?;
        }

        on_result(&result);
        results.push(result);
    }
    Ok(results)
}

fn dump_wav(dir: &Path, name: &str, signal: &Signal) -> Result<(), EvalError> {
    std::fs::create_dir_all(dir).map_err(|source| EvalError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(format!("{name}.wav"));
    let audio = leveller_wav::Audio {
        signal: signal.clone(),
        bit_depth: 24,
        format: leveller_wav::SampleFormat::Int,
    };
    let bytes = leveller_wav::encode(&audio).map_err(|source| EvalError::Wav {
        path: path.clone(),
        source,
    })?;
    std::fs::write(&path, bytes).map_err(|source| EvalError::Io { path, source })
}

/// The pipeline writes its results next to its input, so processing a fixture
/// in place leaves `<name>_processed.wav` and `<name>_roomtone.wav` in the
/// fixtures directory — where the next run would pick them up as fixtures in
/// their own right. That is not a corpus, it is a feedback loop: the harness
/// would end up grading the chain on its own output, and on a room-tone bed
/// that no amount of levelling can bring to the loudness target.
fn is_derived(stem: &str) -> bool {
    stem.ends_with("_processed") || stem.ends_with("_roomtone")
}

/// The first `seconds` of a signal, or all of it when it is shorter.
fn clip(signal: &Signal, seconds: f64) -> Signal {
    let length = signal
        .len()
        .min((seconds * f64::from(signal.sample_rate())).round() as usize);
    Signal::new(
        signal.sample_rate(),
        signal
            .channels()
            .iter()
            .map(|channel| channel[..length].to_vec())
            .collect(),
    )
}

fn spectral_params() -> Map<String, Value> {
    match json!({ "backends": ["spectral"] }) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Real recordings dropped into the fixtures directory, if any.
///
/// A trained denoiser and the de-clicker's sensitivity can only be judged on
/// real speech, and a fixture is the only real speech this harness has.
pub fn fixture_cases(dir: &Path, backends: &Backends) -> Result<Vec<EvalCase>, EvalError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| EvalError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
                && path
                    .file_stem()
                    .is_some_and(|stem| !is_derived(&stem.to_string_lossy()))
        })
        .collect();
    names.sort();

    let onnx_unavailable = match backends.get("onnx") {
        None => Some("the onnx backend is not registered in this build".to_string()),
        Some(backend) => backend.unavailable_reason(48_000),
    };

    let mut cases = Vec::new();
    for path in names {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Read up front so `build` stays a plain closure like the synthetic
        // cases.
        let bytes = std::fs::read(&path).map_err(|source| EvalError::Io {
            path: path.clone(),
            source,
        })?;
        let audio = leveller_wav::decode(&bytes).map_err(|source| EvalError::Wav {
            path: path.clone(),
            source,
        })?;
        let whole = Arc::new(audio.signal);
        let excerpt = Arc::new(clip(&whole, 60.0));

        {
            let whole = whole.clone();
            cases.push(EvalCase {
                name: format!("fixture:{stem}"),
                description: format!("Real recording from {}", dir.display()),
                // Pinned to the classical backend for the same reason the
                // synthetic cases are: a fixture's numbers should not depend on
                // whether the machine running it happens to have model weights
                // installed.
                chain: build_chain(&ChainOptions {
                    params: vec![("denoise".into(), spectral_params())],
                    ..ChainOptions::default()
                })
                .expect("the default chain names only stages that exist"),
                build: Box::new(move || CaseInput {
                    input: whole.clone(),
                    reference: None,
                    segments: Vec::new(),
                    click_positions: Vec::new(),
                    target_lufs: Some(-18.0),
                }),
                // No clean reference exists for real material, so only the
                // self-consistent measurements apply.
                expectations: vec![
                    Expectation {
                        metric: "lufsError",
                        min: None,
                        max: Some(1.5),
                        because: "the programme should land on target regardless of \
                                  source material",
                    },
                    Expectation {
                        metric: "outputPeakDbfs",
                        min: None,
                        max: Some(-0.95),
                        because: "the −1 dBFS limiter ceiling must hold on real \
                                  material too",
                    },
                ],
                unavailable: None,
            });
        }

        // Degrading real speech with known noise supplies the clean reference
        // the fixture cases otherwise lack: the recording itself.
        for backend in ["spectral", "onnx"] {
            let excerpt = excerpt.clone();
            let mut expectations = vec![Expectation {
                metric: "programmeLossDb",
                min: None,
                max: Some(3.0),
                because: "whatever a backend removes, it must not be the voice. This is \
                          the same limit the stage itself enforces, asserted here on real \
                          material so that a backend which starts eating speech fails the \
                          run rather than being quietly reverted every time",
            }];
            if backend == "onnx" {
                expectations.push(Expectation {
                    metric: "siSdrGainDb",
                    min: Some(2.0),
                    max: None,
                    because: "the whole case for a 9 MB download and a native runtime. On \
                              real speech at 20 dB SNR the classical suppressor removes \
                              11.8 dB of noise and still ends up 0.2 dB *further* from the \
                              clean reference — it trades signal for quiet. The model gains \
                              4.97 dB on the same material. The bound sits well below that \
                              so it tracks a different recording, but above zero, because a \
                              model that cannot beat the classical backend here has no \
                              reason to be preferred over it",
                });
            }

            cases.push(EvalCase {
                name: format!("fixture:{stem}:{backend}"),
                description: format!(
                    "That recording plus noise at 20 dB SNR, {backend} denoiser alone"
                ),
                chain: build_chain(&ChainOptions {
                    only: vec!["denoise".into()],
                    params: vec![(
                        "denoise".into(),
                        match json!({ "backends": [backend] }) {
                            Value::Object(map) => map,
                            _ => Map::new(),
                        },
                    )],
                    ..ChainOptions::default()
                })
                .expect("denoise is a stage"),
                build: Box::new(move || CaseInput {
                    input: Arc::new(leveller_corpus::add_noise(&excerpt, 20.0, 991)),
                    reference: Some(excerpt.clone()),
                    segments: Vec::new(),
                    click_positions: Vec::new(),
                    target_lufs: None,
                }),
                expectations,
                unavailable: if backend == "onnx" {
                    onnx_unavailable.clone()
                } else {
                    None
                },
            });
        }

        // The de-clicker's sensitivity can only be judged on real speech as
        // well: real voicing is what its pulse-train veto is calibrated
        // against, and the synthetic voice is far more impulsive than any real
        // one. Two cases: clicks injected at half the excerpt's peak — were
        // they repaired without touching anything else — and the untouched
        // excerpt, where the stage should do almost nothing.
        let declick = build_chain(&ChainOptions {
            only: vec!["declick".into()],
            ..ChainOptions::default()
        })
        .expect("declick is a stage");

        {
            let excerpt = excerpt.clone();
            cases.push(EvalCase {
                name: format!("fixture:{stem}:clicks"),
                description: "That recording plus 40 clicks at half its peak, de-click alone"
                    .into(),
                chain: declick.clone(),
                build: Box::new(move || {
                    let clicked = leveller_corpus::add_clicks(
                        &excerpt,
                        &leveller_corpus::ClickOptions {
                            count: 40,
                            relative_amplitude: 0.5,
                            width_samples: 3,
                            min_gap_sec: 0.05,
                            seed: 4771,
                        },
                    );
                    CaseInput {
                        input: Arc::new(clicked.signal),
                        reference: Some(excerpt.clone()),
                        segments: Vec::new(),
                        click_positions: clicked.positions,
                        target_lufs: None,
                    }
                }),
                expectations: vec![
                    Expectation {
                        metric: "outputClickResidualDb",
                        min: None,
                        max: Some(2.0),
                        because: "every injected click must be repaired to within the local \
                                  peaks around it; a missed one reads +20 dB or more on this \
                                  measure",
                    },
                    Expectation {
                        metric: "changeDb",
                        min: None,
                        max: Some(-20.0),
                        because: "the change should be the clicks and nothing else. A correct \
                                  repair of these 40 lands near −26 dB; a detector that also \
                                  fires on the voice's own glottal pulses adds broad change \
                                  on top of that",
                    },
                ],
                unavailable: None,
            });
        }

        {
            let excerpt = excerpt.clone();
            cases.push(EvalCase {
                name: format!("fixture:{stem}:clean-declick"),
                description: "That recording untouched, de-click alone — the real \
                              transparency check"
                    .into(),
                chain: declick,
                build: Box::new(move || CaseInput {
                    input: excerpt.clone(),
                    reference: Some(excerpt.clone()),
                    segments: Vec::new(),
                    click_positions: Vec::new(),
                    target_lufs: None,
                }),
                expectations: vec![Expectation {
                    metric: "changeDb",
                    min: None,
                    max: Some(-40.0),
                    because: "clean real speech has no clicks, so the stage should barely \
                              act. Before the pulse-train veto it repaired ~12 glottal \
                              pulses a second here and this read −34 dB — audible as \
                              smeared, roomy consonants in blind listening; with the veto \
                              it reads −49 dB",
                }],
                unavailable: None,
            });
        }
    }

    Ok(cases)
}

/// A run's metrics, keyed by case — what a baseline file holds.
pub type Baseline = BTreeMap<String, Metrics>;

pub fn baseline_of(results: &[CaseResult]) -> Baseline {
    results
        .iter()
        .map(|r| (r.name.clone(), r.metrics.clone()))
        .collect()
}

/// Read a baseline, dropping metrics written as `null`.
///
/// JSON has no infinity, so a bypass case's `changeDb` is stored as null — by
/// this harness and by the TypeScript one before it. A missing key is what the
/// diff already treats as nothing to compare, so dropping them here means an
/// older baseline still loads instead of failing to parse.
pub fn read_baseline(path: &Path) -> Result<Baseline, EvalError> {
    let text = std::fs::read_to_string(path).map_err(|source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: BTreeMap<String, BTreeMap<String, Option<f64>>> =
        serde_json::from_str(&text).map_err(|source| EvalError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(raw
        .into_iter()
        .map(|(case, metrics)| {
            (
                case,
                metrics
                    .into_iter()
                    .filter_map(|(key, value)| Some((key, value?)))
                    .collect(),
            )
        })
        .collect())
}

pub fn write_baseline(path: &Path, baseline: &Baseline) -> Result<(), EvalError> {
    let text = serde_json::to_string_pretty(baseline).map_err(|source| EvalError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, text).map_err(|source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// One metric that moved between two runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub metric: String,
    pub before: f64,
    pub after: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseDiff {
    pub name: String,
    /// True when the baseline has never seen this case.
    pub is_new: bool,
    pub moves: Vec<Move>,
}

/// What moved since the baseline, ignoring anything smaller than `threshold`.
///
/// Two values that are both infinite count as unchanged: `-inf` to `-inf` is
/// "still bit-identical", and subtracting them would give a NaN that looks like
/// a change.
pub fn diff(results: &[CaseResult], baseline: &Baseline, threshold: f64) -> Vec<CaseDiff> {
    let mut diffs = Vec::new();
    for result in results {
        let Some(before) = baseline.get(&result.name) else {
            diffs.push(CaseDiff {
                name: result.name.clone(),
                is_new: true,
                moves: Vec::new(),
            });
            continue;
        };

        let mut moves = Vec::new();
        for (metric, after) in &result.metrics {
            let Some(before) = before.get(metric).copied() else {
                continue;
            };
            if !before.is_finite() && !after.is_finite() {
                continue;
            }
            if (after - before).abs() > threshold {
                moves.push(Move {
                    metric: metric.clone(),
                    before,
                    after: *after,
                });
            }
        }
        if !moves.is_empty() {
            diffs.push(CaseDiff {
                name: result.name.clone(),
                is_new: false,
                moves,
            });
        }
    }
    diffs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &str, metrics: &[(&str, f64)]) -> CaseResult {
        CaseResult {
            name: name.into(),
            description: String::new(),
            metrics: metrics
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
            checks: Vec::new(),
            passed: true,
            elapsed_ms: 0.0,
            skipped: None,
        }
    }

    #[test]
    fn a_bound_with_no_such_metric_fails_rather_than_passes() {
        // A typo in a metric name is the one failure mode a harness must not
        // absorb: it would turn a check into a no-op that reads as green.
        let metrics: Metrics = [("outputLufs".to_string(), -18.0)].into_iter().collect();
        let checks = check(
            &metrics,
            &[Expectation {
                metric: "outputLufsTypo",
                min: None,
                max: Some(1.0),
                because: "a long enough reason to satisfy the corpus test",
            }],
        );
        assert!(!checks[0].passed);
        assert!(checks[0].value.is_nan());
    }

    #[test]
    fn a_bound_holds_at_its_edge() {
        let metrics: Metrics = [("peak".to_string(), -0.95)].into_iter().collect();
        let checks = check(
            &metrics,
            &[Expectation {
                metric: "peak",
                min: None,
                max: Some(-0.95),
                because: "a long enough reason to satisfy the corpus test",
            }],
        );
        assert!(checks[0].passed, "the bound is inclusive");
    }

    #[test]
    fn the_diff_ignores_movement_under_the_threshold() {
        let results = [result("clean", &[("outputLufs", -18.02)])];
        let baseline: Baseline = [("clean".to_string(), [("outputLufs".to_string(), -18.0)].into_iter().collect())]
            .into_iter()
            .collect();
        assert!(diff(&results, &baseline, 0.05).is_empty());
    }

    #[test]
    fn the_diff_reports_what_moved() {
        let results = [result("clean", &[("outputLufs", -17.0)])];
        let baseline: Baseline = [("clean".to_string(), [("outputLufs".to_string(), -18.0)].into_iter().collect())]
            .into_iter()
            .collect();
        let moved = diff(&results, &baseline, 0.05);
        assert_eq!(moved[0].moves[0].before, -18.0);
        assert_eq!(moved[0].moves[0].after, -17.0);
    }

    #[test]
    fn two_infinities_are_not_a_change() {
        // `changeDb` is −∞ for every bypass case, and subtracting one from the
        // other gives a NaN that would read as movement on every run.
        let results = [result("bypass-null", &[("changeDb", f64::NEG_INFINITY)])];
        let baseline: Baseline = [(
            "bypass-null".to_string(),
            [("changeDb".to_string(), f64::NEG_INFINITY)]
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect();
        assert!(diff(&results, &baseline, 0.05).is_empty());
    }

    #[test]
    fn a_baseline_written_with_nulls_still_loads() {
        // JSON has no infinity, so every harness that has ever written one of
        // these files wrote null for a bypass case's changeDb.
        let path = std::env::temp_dir().join(format!("eval-baseline-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"bypass-null": {"changeDb": null, "outputLufs": -18.0}}"#,
        )
        .unwrap();
        let baseline = read_baseline(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(baseline["bypass-null"].get("changeDb"), None);
        assert_eq!(baseline["bypass-null"]["outputLufs"], -18.0);
    }

    #[test]
    fn a_case_the_baseline_has_never_seen_is_called_out() {
        let results = [result("brand-new", &[("outputLufs", -18.0)])];
        let moved = diff(&results, &Baseline::new(), 0.05);
        assert!(moved[0].is_new);
    }

    #[test]
    fn derived_outputs_are_not_picked_up_as_fixtures() {
        // Otherwise the harness ends up grading the chain on its own output.
        assert!(is_derived("interview_processed"));
        assert!(is_derived("interview_roomtone"));
        assert!(!is_derived("interview"));
        assert!(!is_derived("processed_interview"));
    }
}
