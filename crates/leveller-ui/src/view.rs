//! The model as a webview sees it, and the messages a webview can send back.
//!
//! The Tauri shell serialises a [`View`] after every message and the frontend
//! draws from it alone, so nothing the frontend shows is worked out twice. What
//! comes back is an [`Input`]: the subset of [`Msg`] that a person can cause by
//! clicking. Drops, focus and progress come from the shell's own events, and a
//! finished run carries a whole `ProcessResult`, so none of those cross.

use std::path::{Path, PathBuf};

use leveller_stages::params::ParamSpec;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::{Model, Msg, Status, Summary};

/// Everything the window shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    /// [`Model::revision`] when this was drawn, so the newest view wins
    /// whichever way it reached the window.
    pub revision: u64,
    pub preset: String,
    pub presets: Vec<PresetView>,
    pub preset_description: &'static str,
    /// Whether any parameter has moved away from the preset.
    pub edited: bool,
    /// The stages switched off, by name, for the note under the presets.
    pub bypassed: Vec<&'static str>,
    pub stages: Vec<StageView>,
    pub status: StatusView,
    pub can_rerender: bool,
    #[serde(serialize_with = "lossy_optional_path")]
    pub last_input: Option<PathBuf>,
    pub window_active: bool,
    pub drag_over: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetView {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageView {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    pub enabled: bool,
    pub expanded: bool,
    pub params: Vec<ParamView>,
}

/// One parameter: what it is, what it holds, and whether that differs from
/// the preset.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamView {
    #[serde(flatten)]
    pub spec: ParamSpec,
    pub value: Value,
    pub edited: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StatusView {
    Idle,
    #[serde(rename_all = "camelCase")]
    Working {
        /// The file's name, which is all the status line has room for.
        file: String,
        stage: String,
        index: usize,
        total: usize,
        overall: f64,
    },
    Done {
        summary: Summary,
    },
    Failed {
        message: String,
    },
}

/// What a person can do in the window.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Input {
    Preset {
        name: String,
    },
    ToggleStage {
        index: usize,
    },
    ToggleExpanded {
        index: usize,
    },
    SetParam {
        stage: String,
        key: String,
        value: Value,
    },
    Revert,
    Rerender,
}

impl From<Input> for Msg {
    fn from(input: Input) -> Self {
        match input {
            Input::Preset { name } => Msg::Preset(name),
            Input::ToggleStage { index } => Msg::ToggleStage(index),
            Input::ToggleExpanded { index } => Msg::ToggleExpanded(index),
            Input::SetParam { stage, key, value } => Msg::SetParam { stage, key, value },
            Input::Revert => Msg::Revert,
            Input::Rerender => Msg::Rerender,
        }
    }
}

/// A path as text, with any bytes that are not UTF-8 replaced. Serde refuses
/// such a path outright, which would stop every view after it from reaching
/// the window; a name with a replacement character is only cosmetic.
pub(crate) fn lossy_path<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

pub(crate) fn lossy_optional_path<S: Serializer>(
    path: &Option<PathBuf>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match path {
        Some(path) => lossy_path(path, serializer),
        None => serializer.serialize_none(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl Model {
    pub fn view(&self) -> View {
        let schema = self.schema();
        View {
            revision: self.revision(),
            preset: self.preset().to_string(),
            presets: schema
                .presets
                .iter()
                .map(|p| PresetView {
                    name: p.name,
                    description: p.description,
                })
                .collect(),
            preset_description: self.preset_description(),
            edited: self.is_edited(),
            bypassed: self
                .stages()
                .iter()
                .filter(|s| !s.enabled)
                .map(|s| s.name)
                .collect(),
            stages: self
                .stages()
                .iter()
                .map(|stage| StageView {
                    name: stage.name,
                    label: stage.label,
                    description: stage.description.clone(),
                    enabled: stage.enabled,
                    expanded: stage.expanded,
                    params: stage
                        .params
                        .iter()
                        .map(|spec| ParamView {
                            spec: spec.clone(),
                            value: self
                                .param(stage.name, spec.key())
                                .cloned()
                                .unwrap_or(Value::Null),
                            edited: self.is_param_edited(stage.name, spec.key()),
                        })
                        .collect(),
                })
                .collect(),
            status: match self.status() {
                Status::Idle => StatusView::Idle,
                Status::Working {
                    path,
                    stage,
                    index,
                    total,
                    overall,
                } => StatusView::Working {
                    file: file_name(path),
                    stage: stage.clone(),
                    index: *index,
                    total: *total,
                    overall: *overall,
                },
                Status::Done(summary) => StatusView::Done {
                    summary: (**summary).clone(),
                },
                Status::Failed(message) => StatusView::Failed {
                    message: message.clone(),
                },
            },
            can_rerender: self.can_rerender(),
            last_input: self.last_input().cloned(),
            window_active: self.window_active(),
            drag_over: self.drag_over(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_fresh_window_serialises_to_what_the_frontend_reads() {
        let view = serde_json::to_value(Model::new().view()).unwrap();

        assert_eq!(view["preset"], "Nino");
        assert_eq!(view["status"], json!({ "kind": "idle" }));
        assert_eq!(view["canRerender"], false);
        assert_eq!(view["stages"].as_array().unwrap().len(), 8);

        // A parameter carries its spec flattened beside its value, so the
        // frontend switches on `kind` without unwrapping anything.
        let level = &view["stages"][7];
        assert_eq!(level["name"], "level");
        let target = level["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == "targetLufs")
            .unwrap();
        assert_eq!(target["kind"], "number");
        assert_eq!(target["value"], json!(-18.0));
        assert_eq!(target["edited"], false);
    }

    #[test]
    fn a_running_file_shows_its_name_not_its_path() {
        let mut model = Model::new();
        model.update(Msg::Dropped(PathBuf::from("/Users/someone/talk.wav")));
        model.update(Msg::Progress {
            stage: "denoise".into(),
            index: 1,
            total: 8,
            overall: 0.2,
        });

        let view = serde_json::to_value(model.view()).unwrap();
        assert_eq!(
            view["status"],
            json!({
                "kind": "working",
                "file": "talk.wav",
                "stage": "denoise",
                "index": 1,
                "total": 8,
                "overall": 0.2,
            })
        );
    }

    #[test]
    fn a_failure_carries_its_message() {
        let mut model = Model::new();
        model.update(Msg::Dropped(PathBuf::from("/tmp/talk.wav")));
        model.update(Msg::Finished(Err("not a WAV file".into())));
        assert_eq!(
            serde_json::to_value(model.view()).unwrap()["status"],
            json!({ "kind": "failed", "message": "not a WAV file" })
        );
    }

    #[test]
    fn inputs_arrive_in_the_shape_the_frontend_sends() {
        let input: Input = serde_json::from_value(json!({
            "kind": "setParam",
            "stage": "level",
            "key": "targetLufs",
            "value": -20.0,
        }))
        .unwrap();

        let mut model = Model::new();
        model.update(input.into());
        assert_eq!(model.param("level", "targetLufs"), Some(&json!(-20.0)));

        let view = model.view();
        assert!(view.edited);
        let edited = view.stages[7]
            .params
            .iter()
            .find(|p| p.spec.key() == "targetLufs")
            .unwrap();
        assert!(edited.edited);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_still_serialises() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut model = Model::new();
        let path = Path::new(OsStr::from_bytes(b"/tmp/Aufnahme-\xE4.wav"));
        model.update(Msg::Dropped(path.to_path_buf()));
        let view = serde_json::to_value(model.view()).unwrap();
        assert_eq!(view["lastInput"], "/tmp/Aufnahme-\u{FFFD}.wav");
    }

    #[test]
    fn every_message_moves_the_revision_on() {
        let mut model = Model::new();
        let before = model.view().revision;
        model.update(Msg::WindowActive(false));
        assert!(model.view().revision > before);
    }

    #[test]
    fn switched_off_stages_are_listed_by_name() {
        let mut model = Model::new();
        let input: Input =
            serde_json::from_value(json!({ "kind": "toggleStage", "index": 1 })).unwrap();
        model.update(input.into());
        assert_eq!(model.view().bypassed, vec!["denoise"]);
    }
}
