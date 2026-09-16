//! What the Audio Leveller app *is*, with no window in sight.
//!
//! The state, the messages that change it, and the effects that fall out. A
//! platform shell — AppKit today, something else later — turns clicks into
//! messages and state into pixels, and nothing in here knows which one is
//! doing it. That is what makes the interesting behaviour testable: which
//! stages are on, what a preset resolves to, whether a parameter counts as
//! edited, when the Re-render button lights up.

use std::path::PathBuf;

use leveller_io::ProcessResult;
use leveller_pipeline::StageSpec;
use leveller_stages::params::{ParamSpec, Schema};
use leveller_stages::{ChainOptions, build_chain, default_registry, params};
use serde_json::{Map, Value};

/// What the app is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing yet, or the last thing finished and was dismissed.
    Idle,
    Working {
        /// The file being processed.
        path: PathBuf,
        stage: String,
        /// Which stage of how many.
        index: usize,
        total: usize,
        /// Overall completion, in [0, 1].
        overall: f64,
    },
    Done(Box<Summary>),
    Failed(String),
}

impl Status {
    pub fn is_working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }
}

/// The part of a finished run the interface shows.
///
/// A summary rather than the whole report, because the interface needs a dozen
/// numbers and the report is thousands of lines — including one entry per
/// detected click.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub output_path: PathBuf,
    pub room_tone_path: Option<PathBuf>,
    pub input_lufs: f64,
    pub output_lufs: f64,
    pub output_peak_dbfs: f64,
    pub duration_sec: f64,
    pub channels: usize,
    pub sample_rate: u32,
    /// One line per stage: its name, whether it ran, how long it took, and what
    /// it decided in a few words.
    pub stages: Vec<StageLine>,
    pub segments: usize,
    pub silences: usize,
    pub room_tone_sec: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageLine {
    pub name: String,
    pub enabled: bool,
    pub elapsed_ms: f64,
    /// What the stage decided, in the fewest words that say it.
    pub decision: String,
}

/// One stage as the interface sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    pub enabled: bool,
    /// Whether its parameters are showing.
    pub expanded: bool,
    pub params: Vec<ParamSpec>,
}

/// Neither `Clone` nor `PartialEq`: [`Msg::Finished`] carries a whole
/// `ProcessResult`, which holds a pipeline report and is not worth copying to
/// satisfy a derive.
#[derive(Debug)]
pub enum Msg {
    /// A file was dropped, or chosen from the open panel.
    Dropped(PathBuf),
    /// The pointer is over the drop zone with a file in hand.
    DragOver(bool),
    Preset(String),
    ToggleStage(usize),
    ToggleExpanded(usize),
    SetParam {
        stage: String,
        key: String,
        value: Value,
    },
    /// Put every parameter back to the current preset's.
    Revert,
    /// Run the last file again with whatever is set now.
    Rerender,
    Progress {
        stage: String,
        index: usize,
        total: usize,
        overall: f64,
    },
    Finished(Result<Box<ProcessResult>, String>),
    WindowActive(bool),
}

/// Something for the shell to do, which the model cannot do itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Run this file through this chain, off the main thread, reporting
    /// progress and then a result.
    Process {
        path: PathBuf,
        stages: Vec<StageSpec>,
    },
}

pub struct Model {
    schema: Schema,
    stages: Vec<Stage>,
    preset: String,
    /// The parameters in force, per stage. Starts as the preset's and diverges
    /// as sliders move.
    settings: Vec<(&'static str, Map<String, Value>)>,
    status: Status,
    last_input: Option<PathBuf>,
    window_active: bool,
    drag_over: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        let registry = default_registry();
        let schema = params::schema(&registry);

        let stages = schema
            .chain
            .iter()
            .map(|name| {
                let group = schema.groups.iter().find(|g| g.stage == *name);
                Stage {
                    name,
                    label: group.map_or(*name, |g| g.label),
                    description: registry
                        .get(name)
                        .map_or_else(String::new, |s| s.description().to_string()),
                    enabled: true,
                    expanded: false,
                    params: group.map(|g| g.params.clone()).unwrap_or_default(),
                }
            })
            .collect();

        let preset = schema.default_preset.to_string();
        let settings = schema
            .settings_for(&preset)
            .unwrap_or_else(|| schema.defaults.clone());

        Self {
            schema,
            stages,
            preset,
            settings,
            status: Status::Idle,
            last_input: None,
            window_active: true,
            drag_over: false,
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    pub fn preset(&self) -> &str {
        &self.preset
    }

    pub fn preset_names(&self) -> Vec<&'static str> {
        self.schema.presets.iter().map(|p| p.name).collect()
    }

    pub fn preset_description(&self) -> &'static str {
        self.schema
            .presets
            .iter()
            .find(|p| p.name == self.preset)
            .map_or("", |p| p.description)
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn window_active(&self) -> bool {
        self.window_active
    }

    pub fn drag_over(&self) -> bool {
        self.drag_over
    }

    pub fn last_input(&self) -> Option<&PathBuf> {
        self.last_input.as_ref()
    }

    /// The value a parameter currently holds.
    pub fn param(&self, stage: &str, key: &str) -> Option<&Value> {
        self.settings
            .iter()
            .find(|(name, _)| *name == stage)
            .and_then(|(_, values)| values.get(key))
    }

    /// Whether anything has been moved away from the preset.
    ///
    /// This is what the Revert button watches, and what puts "edited" next to a
    /// parameter's name.
    pub fn is_edited(&self) -> bool {
        self.schema
            .settings_for(&self.preset)
            .is_none_or(|preset| preset != self.settings)
    }

    pub fn is_param_edited(&self, stage: &str, key: &str) -> bool {
        let preset = self.schema.settings_for(&self.preset);
        let original = preset
            .as_ref()
            .and_then(|s| s.iter().find(|(name, _)| *name == stage))
            .and_then(|(_, values)| values.get(key));
        original != self.param(stage, key)
    }

    /// Whether Re-render can do anything: there has to be a file, and nothing
    /// already running.
    pub fn can_rerender(&self) -> bool {
        self.last_input.is_some() && !self.status.is_working()
    }

    /// The chain as currently set: the stages that are on, with the parameters
    /// in force.
    pub fn chain(&self) -> Vec<StageSpec> {
        let options = ChainOptions {
            only: Vec::new(),
            bypass: self
                .stages
                .iter()
                .filter(|s| !s.enabled)
                .map(|s| s.name.to_string())
                .collect(),
            params: self
                .settings
                .iter()
                .map(|(stage, values)| ((*stage).to_string(), values.clone()))
                .collect(),
        };
        // The stage names come from the registry, so they are known good; a
        // failure here would be a bug rather than a condition.
        build_chain(&options).unwrap_or_default()
    }

    pub fn update(&mut self, msg: Msg) -> Option<Effect> {
        match msg {
            Msg::Dropped(path) => {
                self.last_input = Some(path.clone());
                self.drag_over = false;
                self.status = Status::Working {
                    path: path.clone(),
                    stage: String::new(),
                    index: 0,
                    total: 0,
                    overall: 0.0,
                };
                Some(Effect::Process {
                    path,
                    stages: self.chain(),
                })
            }

            Msg::DragOver(over) => {
                self.drag_over = over;
                None
            }

            Msg::Preset(name) => {
                if let Some(settings) = self.schema.settings_for(&name) {
                    self.preset = name;
                    self.settings = settings;
                }
                None
            }

            Msg::ToggleStage(index) => {
                if let Some(stage) = self.stages.get_mut(index) {
                    stage.enabled = !stage.enabled;
                }
                None
            }

            Msg::ToggleExpanded(index) => {
                if let Some(stage) = self.stages.get_mut(index) {
                    stage.expanded = !stage.expanded;
                }
                None
            }

            Msg::SetParam { stage, key, value } => {
                if let Some((_, values)) = self.settings.iter_mut().find(|(name, _)| *name == stage)
                {
                    values.insert(key, value);
                }
                None
            }

            Msg::Revert => {
                if let Some(settings) = self.schema.settings_for(&self.preset) {
                    self.settings = settings;
                }
                None
            }

            Msg::Rerender => {
                let path = self.last_input.clone()?;
                if self.status.is_working() {
                    return None;
                }
                self.status = Status::Working {
                    path: path.clone(),
                    stage: String::new(),
                    index: 0,
                    total: 0,
                    overall: 0.0,
                };
                Some(Effect::Process {
                    path,
                    stages: self.chain(),
                })
            }

            Msg::Progress {
                stage,
                index,
                total,
                overall,
            } => {
                if let Status::Working {
                    stage: current,
                    index: at,
                    total: of,
                    overall: done,
                    ..
                } = &mut self.status
                {
                    *current = stage;
                    *at = index;
                    *of = total;
                    *done = overall;
                }
                None
            }

            Msg::Finished(Ok(result)) => {
                self.status = Status::Done(Box::new(summarise(&result)));
                None
            }

            Msg::Finished(Err(message)) => {
                self.status = Status::Failed(message);
                None
            }

            Msg::WindowActive(active) => {
                self.window_active = active;
                None
            }
        }
    }
}

/// Turn a finished run into the dozen numbers the interface shows.
fn summarise(result: &ProcessResult) -> Summary {
    let report = &result.report;
    let leveller = leveller_io::leveller_report(report);

    Summary {
        output_path: result.output_path.clone(),
        room_tone_path: result.room_tone_path.clone(),
        input_lufs: report.input.integrated_lufs,
        output_lufs: report.output.integrated_lufs,
        output_peak_dbfs: report.output.peak_dbfs,
        duration_sec: report.duration_sec,
        channels: report.channels,
        sample_rate: report.source_sample_rate,
        stages: report
            .stages
            .iter()
            .map(|stage| StageLine {
                name: stage.name.clone(),
                enabled: stage.enabled,
                elapsed_ms: stage.elapsed_ms,
                decision: decision_of(&stage.name, &stage.report, stage.enabled),
            })
            .collect(),
        segments: leveller.map_or(0, |l| l["segments"].as_array().map_or(0, Vec::len)),
        silences: leveller.map_or(0, |l| l["silences"].as_array().map_or(0, Vec::len)),
        room_tone_sec: leveller.map_or(0.0, |l| {
            l["roomTone"]["durationSec"].as_f64().unwrap_or(0.0)
        }),
    }
}

/// What a stage decided, in the fewest words that say it.
///
/// Each stage reports different things, and the interesting thing is nearly
/// always whether it declined and why — a chain where six stages say "nothing
/// to do" is telling you the recording was already good, which is worth reading
/// at a glance.
fn decision_of(name: &str, report: &Value, enabled: bool) -> String {
    if !enabled {
        return "bypassed".into();
    }
    let number = |key: &str| report[key].as_f64().unwrap_or(0.0);
    let flag = |key: &str| report[key].as_bool().unwrap_or(false);

    match name {
        "declick" if flag("aborted") => "declined — too much of the file looked damaged".into(),
        "declick" => match report["repaired"].as_u64().unwrap_or(0) {
            0 => "no clicks found".into(),
            1 => "repaired 1 click".into(),
            n => format!("repaired {n} clicks"),
        },

        "denoise" if flag("skippedEntirely") => "already clean".into(),
        "denoise" if report["rejected"].is_string() => {
            "output rejected — it was removing the speech".into()
        }
        "denoise" => format!(
            "{} took {:.1} dB off the floor",
            report["backend"].as_str().unwrap_or("a backend"),
            number("reductionAchievedDb")
        ),

        "dereverb" if flag("skipped") => "dry enough to leave alone".into(),
        "dereverb" => format!(
            "{:.0} ms of decay down to {:.0} ms",
            number("decayBeforeMs"),
            number("decayAfterMs")
        ),

        "eq" if flag("skipped") => "too little speech to measure".into(),
        "eq" => {
            let bands = report["bands"].as_array().map_or(0, Vec::len);
            let rumble = report["rumbleFreq"]
                .as_f64()
                .map(|f| format!(", rumble filter at {f:.0} Hz"))
                .unwrap_or_default();
            match bands {
                0 => format!("spectrum already even{rumble}"),
                1 => format!("1 corrective band{rumble}"),
                n => format!("{n} corrective bands{rumble}"),
            }
        }

        "dyneq" => format!(
            "{:.1}% of cells touched, up to {:.1} dB",
            number("activeFraction") * 100.0,
            number("maxReductionDb")
        ),

        "expand" if flag("skipped") => "floor already deep enough".into(),
        "expand" => format!("floor pushed down {:.1} dB", number("floorReductionDb")),

        "compress" if flag("skipped") => "already even".into(),
        "compress" => format!(
            "{:.1} LU of range down to {:.1}",
            number("loudnessRangeBeforeLu"),
            number("loudnessRangeAfterLu")
        ),

        "level" => {
            let segments = report["segments"].as_array().map_or(0, Vec::len);
            let limiter = number("limiterGainReductionDb");
            let limiting = if limiter > 0.01 {
                format!(", limiter to {limiter:.1} dB")
            } else {
                String::new()
            };
            format!("{segments} segments levelled{limiting}")
        }

        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> Model {
        Model::new()
    }

    #[test]
    fn it_starts_on_the_default_preset_with_every_stage_on() {
        let model = model();
        assert_eq!(model.preset(), "Nino");
        assert_eq!(model.stages().len(), 8);
        assert!(model.stages().iter().all(|s| s.enabled));
        assert!(model.stages().iter().all(|s| !s.expanded));
        assert_eq!(model.status(), &Status::Idle);
        assert!(!model.is_edited());
        assert!(!model.can_rerender(), "there is no file yet");
    }

    #[test]
    fn every_stage_carries_its_own_description() {
        for stage in model().stages() {
            assert!(!stage.description.is_empty(), "{} has none", stage.name);
        }
    }

    #[test]
    fn dropping_a_file_starts_a_run() {
        let mut model = model();
        let effect = model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));

        assert!(matches!(effect, Some(Effect::Process { .. })));
        assert!(model.status().is_working());
        assert_eq!(model.last_input(), Some(&PathBuf::from("/tmp/talk.wav")));
    }

    #[test]
    fn a_dropped_file_is_processed_with_the_chain_as_set() {
        let mut model = model();
        model.update(Msg::ToggleStage(1)); // denoise off
        let Some(Effect::Process { stages, .. }) =
            model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")))
        else {
            panic!("no effect");
        };

        assert_eq!(stages.len(), 8);
        let denoise = stages.iter().find(|s| s.name == "denoise").unwrap();
        assert!(!denoise.enabled);
        assert!(
            stages
                .iter()
                .filter(|s| s.name != "denoise")
                .all(|s| s.enabled)
        );
    }

    #[test]
    fn a_preset_moves_every_parameter_it_names() {
        let mut model = model();
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-18.0)));

        model.update(Msg::Preset("ACX".into()));
        assert_eq!(model.preset(), "ACX");
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-20.0)));
        assert_eq!(model.param("level", "ceilingDb"), Some(&json!(-3.5)));
        assert_eq!(model.param("eq", "voicing"), Some(&json!("neutral")));
        // And switching preset is not an edit.
        assert!(!model.is_edited());
    }

    #[test]
    fn an_unknown_preset_changes_nothing() {
        let mut model = model();
        model.update(Msg::Preset("Norwegian".into()));
        assert_eq!(model.preset(), "Nino");
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-18.0)));
    }

    #[test]
    fn moving_a_parameter_marks_it_and_the_whole_thing_as_edited() {
        let mut model = model();
        model.update(Msg::SetParam {
            stage: "level".into(),
            key: "targetLufs".into(),
            value: json!(-21.0),
        });

        assert!(model.is_edited());
        assert!(model.is_param_edited("level", "targetLufs"));
        assert!(
            !model.is_param_edited("level", "ceilingDb"),
            "only that one"
        );
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-21.0)));
    }

    #[test]
    fn reverting_puts_the_preset_back() {
        let mut model = model();
        model.update(Msg::Preset("ACX".into()));
        model.update(Msg::SetParam {
            stage: "level".into(),
            key: "targetLufs".into(),
            value: json!(-9.0),
        });
        assert!(model.is_edited());

        model.update(Msg::Revert);
        assert!(!model.is_edited());
        // Back to the *preset's* value, not to the stage's default.
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-20.0)));
    }

    #[test]
    fn a_moved_parameter_reaches_the_chain() {
        let mut model = model();
        model.update(Msg::SetParam {
            stage: "level".into(),
            key: "targetLufs".into(),
            value: json!(-23.0),
        });

        let chain = model.chain();
        let level = chain.iter().find(|s| s.name == "level").unwrap();
        assert_eq!(level.params.get("targetLufs"), Some(&json!(-23.0)));
    }

    #[test]
    fn re_render_needs_a_file_and_a_free_moment() {
        let mut model = model();
        assert!(
            model.update(Msg::Rerender).is_none(),
            "nothing to re-render"
        );

        model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));
        assert!(!model.can_rerender(), "not while it is running");
        assert!(model.update(Msg::Rerender).is_none());

        model.update(Msg::Finished(Err("stopped".into())));
        assert!(model.can_rerender());
        assert!(matches!(
            model.update(Msg::Rerender),
            Some(Effect::Process { .. })
        ));
    }

    #[test]
    fn progress_updates_the_status_without_losing_the_file() {
        let mut model = model();
        model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));
        model.update(Msg::Progress {
            stage: "denoise".into(),
            index: 1,
            total: 8,
            overall: 0.25,
        });

        let Status::Working {
            path,
            stage,
            index,
            total,
            overall,
        } = model.status()
        else {
            panic!("not working: {:?}", model.status());
        };
        assert_eq!(path, &PathBuf::from("/tmp/talk.wav"));
        assert_eq!(stage, "denoise");
        assert_eq!((*index, *total), (1, 8));
        assert!((overall - 0.25).abs() < 1e-12);
    }

    #[test]
    fn progress_arriving_when_nothing_is_running_is_ignored() {
        let mut model = model();
        model.update(Msg::Progress {
            stage: "eq".into(),
            index: 3,
            total: 8,
            overall: 0.4,
        });
        assert_eq!(model.status(), &Status::Idle);
    }

    #[test]
    fn a_failure_is_shown_rather_than_swallowed() {
        let mut model = model();
        model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));
        model.update(Msg::Finished(Err("that is not a wav file".into())));
        assert_eq!(
            model.status(),
            &Status::Failed("that is not a wav file".into())
        );
    }

    #[test]
    fn toggling_a_stage_and_its_disclosure_are_separate_things() {
        let mut model = model();
        model.update(Msg::ToggleExpanded(3));
        assert!(model.stages()[3].expanded);
        assert!(
            model.stages()[3].enabled,
            "showing it is not turning it off"
        );

        model.update(Msg::ToggleStage(3));
        assert!(!model.stages()[3].enabled);
        assert!(
            model.stages()[3].expanded,
            "and turning it off leaves it open"
        );
    }

    #[test]
    fn a_toggle_out_of_range_does_nothing() {
        let mut model = model();
        model.update(Msg::ToggleStage(99));
        assert!(model.stages().iter().all(|s| s.enabled));
    }

    #[test]
    fn the_window_going_to_the_back_is_remembered() {
        let mut model = model();
        assert!(model.window_active());
        model.update(Msg::WindowActive(false));
        assert!(!model.window_active());
    }

    #[test]
    fn dragging_over_the_zone_is_remembered_and_cleared_by_the_drop() {
        let mut model = model();
        model.update(Msg::DragOver(true));
        assert!(model.drag_over());
        model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));
        assert!(!model.drag_over(), "the drop ends the drag");
    }

    #[test]
    fn a_stage_that_declined_says_so_in_a_few_words() {
        // The summary line is what someone reads instead of the report, so
        // "already clean" has to survive being the only thing they see.
        let cases = [
            ("declick", json!({ "aborted": true }), "declined"),
            ("declick", json!({ "repaired": 0 }), "no clicks found"),
            ("declick", json!({ "repaired": 1 }), "repaired 1 click"),
            ("declick", json!({ "repaired": 7 }), "repaired 7 clicks"),
            (
                "denoise",
                json!({ "skippedEntirely": true }),
                "already clean",
            ),
            ("dereverb", json!({ "skipped": true }), "dry enough"),
            ("eq", json!({ "skipped": true }), "too little speech"),
            ("expand", json!({ "skipped": true }), "already deep enough"),
            ("compress", json!({ "skipped": true }), "already even"),
        ];
        for (name, report, expected) in cases {
            let line = decision_of(name, &report, true);
            assert!(line.contains(expected), "{name}: {line:?}");
        }
    }

    #[test]
    fn a_bypassed_stage_says_bypassed_whatever_its_report_holds() {
        assert_eq!(decision_of("denoise", &json!({}), false), "bypassed");
        assert_eq!(decision_of("level", &Value::Null, false), "bypassed");
    }

    #[test]
    fn a_stage_that_acted_says_what_it_measured() {
        let line = decision_of(
            "denoise",
            &json!({ "backend": "spectral", "reductionAchievedDb": 8.32 }),
            true,
        );
        assert!(line.contains("spectral"), "{line}");
        assert!(line.contains("8.3 dB"), "{line}");

        let line = decision_of("eq", &json!({ "bands": [1, 2], "rumbleFreq": 80.0 }), true);
        assert!(line.contains("2 corrective bands"), "{line}");
        assert!(line.contains("rumble filter at 80 Hz"), "{line}");
    }

    #[test]
    fn an_unknown_stage_gets_an_empty_line_rather_than_nonsense() {
        assert_eq!(decision_of("something-new", &json!({}), true), "");
    }
}
