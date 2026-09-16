//! What VoiceOver and the accessibility inspector see.
//!
//! The window is one custom view, so nothing in it is an element by default —
//! a screen reader would find a rectangle called "Audio Leveller" and stop. The
//! answer is to build the elements by hand from the same layout the drawing and
//! the hit-testing use, so what is announced is exactly what is on screen and
//! exactly what a click would find.
//!
//! The descriptions themselves are worth the trouble. Each stage already
//! carries a one-line explanation of what it does — the same line the window
//! shows in grey on the right — and that is its accessibility help, so someone
//! listening gets the same information someone looking gets rather than a bare
//! "De-click, checkbox, checked".

use leveller_stages::params::ParamSpec;
use leveller_ui::{Model, Status};

use crate::layout::{Hit, Layout, Rect};

/// One thing a screen reader can find.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub rect: Rect,
    pub role: Role,
    /// What it is called.
    pub label: String,
    /// What it currently says, where that is separate from its name.
    pub value: Option<String>,
    /// The longer explanation, where there is one.
    pub help: Option<String>,
    /// What activating it does, or `None` for something that only reports.
    pub hit: Option<Hit>,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Button,
    Checkbox,
    Disclosure,
    RadioGroup,
    Radio,
    Slider,
    StaticText,
    Group,
    ProgressIndicator,
}

/// Every element in the window, in reading order — which is also the order Tab
/// moves through them.
pub fn elements(model: &Model, layout: &Layout) -> Vec<Element> {
    let mut elements = Vec::new();

    for (frame, light) in aqua::chrome::light_frames(layout.titlebar.height)
        .into_iter()
        .zip(aqua::palette::Light::ALL)
    {
        elements.push(Element {
            rect: Rect::new(
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            ),
            role: Role::Button,
            label: light.label().to_string(),
            value: None,
            help: None,
            hit: Some(Hit::Light(light)),
            enabled: true,
        });
    }

    // The drop zone is a button as well as a target, so it is reachable without
    // a mouse and without a drag.
    elements.push(Element {
        rect: layout.dropzone,
        role: Role::Button,
        label: "Drop a .wav file here".into(),
        value: model
            .last_input()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        help: Some(
            "Each speech segment is normalised to its target loudness. \
             Activate this to choose a file instead of dragging one."
                .into(),
        ),
        hit: Some(Hit::DropZone),
        enabled: true,
    });

    if let Some(rect) = layout.status {
        elements.push(status_element(model, rect));
    }

    elements.push(Element {
        rect: layout.preset_segments,
        role: Role::RadioGroup,
        label: "Preset".into(),
        value: Some(model.preset().to_string()),
        help: Some(model.preset_description().to_string()),
        hit: None,
        enabled: true,
    });
    let names = model.preset_names();
    for (i, name) in names.iter().enumerate() {
        let width = layout.preset_segments.width / names.len() as f64;
        elements.push(Element {
            rect: Rect::new(
                layout.preset_segments.x + i as f64 * width,
                layout.preset_segments.y,
                width,
                layout.preset_segments.height,
            ),
            role: Role::Radio,
            label: (*name).to_string(),
            value: Some(
                if *name == model.preset() {
                    "selected"
                } else {
                    ""
                }
                .into(),
            ),
            help: None,
            hit: Some(Hit::Preset(i)),
            enabled: true,
        });
    }

    elements.push(Element {
        rect: layout.revert,
        role: Role::Button,
        label: "Revert".into(),
        value: None,
        help: Some("Put every parameter back to the preset's".into()),
        hit: Some(Hit::Revert),
        enabled: model.is_edited(),
    });
    elements.push(Element {
        rect: layout.rerender,
        role: Role::Button,
        label: "Re-render".into(),
        value: None,
        help: Some("Run the last file again with the settings as they are now".into()),
        hit: Some(Hit::Rerender),
        enabled: model.can_rerender(),
    });

    for row in &layout.rows {
        let stage = &model.stages()[row.index];

        elements.push(Element {
            rect: row.checkbox,
            role: Role::Checkbox,
            label: stage.label.to_string(),
            value: Some(if stage.enabled { "on" } else { "off" }.into()),
            // The stage's own one-liner, so listening tells you as much as
            // looking.
            help: Some(stage.description.clone()),
            hit: Some(Hit::StageCheckbox(row.index)),
            enabled: true,
        });
        elements.push(Element {
            rect: row.disclosure,
            role: Role::Disclosure,
            label: format!("{} parameters", stage.label),
            value: Some(if stage.expanded { "shown" } else { "hidden" }.into()),
            help: None,
            hit: Some(Hit::StageDisclosure(row.index)),
            enabled: !stage.params.is_empty(),
        });

        for param in &row.params {
            let Some(spec) = stage.params.get(param.index) else {
                continue;
            };
            elements.push(param_element(model, stage.name, spec, param, row.index));
        }
    }

    elements
}

fn status_element(model: &Model, rect: Rect) -> Element {
    match model.status() {
        Status::Idle => Element {
            rect,
            role: Role::Group,
            label: "Status".into(),
            value: None,
            help: None,
            hit: None,
            enabled: true,
        },

        Status::Working {
            stage,
            index,
            total,
            overall,
            ..
        } => Element {
            rect,
            role: Role::ProgressIndicator,
            label: "Processing".into(),
            // Spoken rather than shown: a percentage on its own tells a
            // listener nothing about how far through the chain it is.
            value: Some(format!(
                "{:.0}%, {stage}, stage {} of {total}",
                overall * 100.0,
                index + 1
            )),
            help: None,
            hit: None,
            enabled: true,
        },

        Status::Failed(message) => Element {
            rect,
            role: Role::StaticText,
            label: "Could not process that file".into(),
            value: Some(message.clone()),
            help: None,
            hit: None,
            enabled: true,
        },

        Status::Done(summary) => {
            // The whole result as one sentence, because a screen reader user
            // should not have to walk eight rows to learn whether it worked.
            let decisions: Vec<String> = summary
                .stages
                .iter()
                .map(|s| format!("{}: {}", s.name, s.decision))
                .collect();
            Element {
                rect,
                role: Role::StaticText,
                label: format!(
                    "Wrote {}",
                    summary
                        .output_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                value: Some(format!(
                    "{:.1} LUFS in, {:.1} out, peak {:.1} dBFS, {} segments. {}",
                    summary.input_lufs,
                    summary.output_lufs,
                    summary.output_peak_dbfs,
                    summary.segments,
                    decisions.join(". ")
                )),
                help: None,
                hit: None,
                enabled: true,
            }
        }
    }
}

fn param_element(
    model: &Model,
    stage: &str,
    spec: &ParamSpec,
    row: &crate::layout::ParamRow,
    stage_index: usize,
) -> Element {
    let (key, label, help, role) = match spec {
        ParamSpec::Number {
            key, label, help, ..
        } => (*key, *label, *help, Role::Slider),
        ParamSpec::Boolean {
            key, label, help, ..
        } => (*key, *label, *help, Role::Checkbox),
        ParamSpec::Choice {
            key, label, help, ..
        } => (*key, *label, *help, Role::RadioGroup),
    };

    let value = model.param(stage, key);
    let spoken = match spec {
        ParamSpec::Number { unit, .. } => {
            let number = value.and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            if unit.is_empty() {
                format!("{number}")
            } else {
                format!("{number} {unit}")
            }
        }
        ParamSpec::Boolean { .. } => value
            .and_then(serde_json::Value::as_bool)
            .map(|on| if on { "on" } else { "off" })
            .unwrap_or("off")
            .to_string(),
        ParamSpec::Choice { options, .. } => {
            let current = value.and_then(|v| v.as_str()).unwrap_or("");
            options
                .iter()
                .find(|(v, _)| *v == current)
                .map(|(_, label)| (*label).to_string())
                .unwrap_or_default()
        }
    };

    Element {
        rect: Rect::new(
            row.label.x,
            row.label.y,
            row.value.x + row.value.width - row.label.x,
            row.label.height,
        ),
        role,
        // The edited state is spoken, not only coloured — a listener cannot see
        // that the label went amber.
        label: if model.is_param_edited(stage, key) {
            format!("{label}, edited")
        } else {
            label.to_string()
        },
        value: Some(spoken),
        help: Some(help.to_string()),
        hit: Some(Hit::Slider(stage_index, row.index)),
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use leveller_ui::Msg;

    fn built(model: &Model) -> Vec<Element> {
        elements(model, &layout::compute(model, 760.0, 900.0))
    }

    #[test]
    fn every_element_has_a_name() {
        // An unnamed element is one a screen reader reads as "group" and moves
        // past, which is worse than not exposing it at all.
        let mut model = Model::new();
        model.update(Msg::ToggleExpanded(7));
        for element in built(&model) {
            assert!(!element.label.is_empty(), "{element:?}");
        }
    }

    #[test]
    fn the_three_lights_are_named_for_what_they_do() {
        let labels: Vec<String> = built(&Model::new())
            .into_iter()
            .filter(|e| matches!(e.hit, Some(Hit::Light(_))))
            .map(|e| e.label)
            .collect();
        assert_eq!(labels, vec!["Close", "Minimise", "Zoom"]);
    }

    #[test]
    fn every_stage_is_a_checkbox_carrying_its_own_explanation() {
        let model = Model::new();
        let checkboxes: Vec<Element> = built(&model)
            .into_iter()
            .filter(|e| matches!(e.hit, Some(Hit::StageCheckbox(_))))
            .collect();

        assert_eq!(checkboxes.len(), model.stages().len());
        for (element, stage) in checkboxes.iter().zip(model.stages()) {
            assert_eq!(element.role, Role::Checkbox);
            assert_eq!(element.label, stage.label);
            assert_eq!(element.value.as_deref(), Some("on"));
            // The same line the window shows in grey, so listening tells you as
            // much as looking.
            assert_eq!(element.help.as_deref(), Some(stage.description.as_str()));
        }
    }

    #[test]
    fn turning_a_stage_off_changes_what_it_reports() {
        let mut model = Model::new();
        model.update(Msg::ToggleStage(0));
        let checkbox = built(&model)
            .into_iter()
            .find(|e| e.hit == Some(Hit::StageCheckbox(0)))
            .unwrap();
        assert_eq!(checkbox.value.as_deref(), Some("off"));
    }

    #[test]
    fn the_drop_zone_is_reachable_without_a_mouse() {
        let element = built(&Model::new())
            .into_iter()
            .find(|e| e.hit == Some(Hit::DropZone))
            .expect("the drop zone should be an element");
        assert_eq!(element.role, Role::Button);
        assert!(
            element
                .help
                .as_deref()
                .unwrap_or_default()
                .contains("choose a file"),
            "it should say it can be activated: {element:?}"
        );
    }

    #[test]
    fn a_button_that_cannot_do_anything_says_so() {
        let model = Model::new();
        let of = |hit: Hit| {
            built(&model)
                .into_iter()
                .find(|e| e.hit == Some(hit))
                .unwrap()
        };
        assert!(!of(Hit::Revert).enabled, "nothing is edited yet");
        assert!(!of(Hit::Rerender).enabled, "there is no file yet");
    }

    #[test]
    fn an_edited_parameter_says_it_is_edited_rather_than_only_looking_it() {
        let mut model = Model::new();
        model.update(Msg::ToggleExpanded(7));
        model.update(Msg::SetParam {
            stage: "level".into(),
            key: "targetLufs".into(),
            value: serde_json::json!(-21.0),
        });

        let target = built(&model)
            .into_iter()
            .find(|e| e.label.starts_with("Target"))
            .expect("the target slider");
        assert_eq!(target.label, "Target, edited");
        assert_eq!(target.role, Role::Slider);
        assert_eq!(target.value.as_deref(), Some("-21 LUFS"));
    }

    #[test]
    fn a_parameter_carries_its_help_text() {
        let mut model = Model::new();
        model.update(Msg::ToggleExpanded(7));
        let target = built(&model)
            .into_iter()
            .find(|e| e.label == "Target")
            .unwrap();
        assert!(
            target.help.unwrap().contains("normalised"),
            "the slider should explain itself"
        );
    }

    #[test]
    fn the_preset_switch_is_a_radio_group_with_one_selected() {
        let elements = built(&Model::new());
        let group = elements
            .iter()
            .find(|e| e.role == Role::RadioGroup && e.label == "Preset")
            .expect("a preset group");
        assert_eq!(group.value.as_deref(), Some("Nino"));

        let radios: Vec<&Element> = elements
            .iter()
            .filter(|e| matches!(e.hit, Some(Hit::Preset(_))))
            .collect();
        assert_eq!(radios.len(), 2);
        assert_eq!(radios[0].value.as_deref(), Some("selected"));
        assert_eq!(radios[1].value.as_deref(), Some(""));
    }

    #[test]
    fn progress_is_spoken_as_more_than_a_percentage() {
        let mut model = Model::new();
        model.update(Msg::Dropped(std::path::PathBuf::from("/tmp/talk.wav")));
        model.update(Msg::Progress {
            stage: "denoise".into(),
            index: 1,
            total: 8,
            overall: 0.25,
        });

        let status = built(&model)
            .into_iter()
            .find(|e| e.role == Role::ProgressIndicator)
            .expect("a progress element");
        let value = status.value.unwrap();
        assert!(value.contains("25%"), "{value}");
        assert!(value.contains("denoise"), "{value}");
        assert!(value.contains("stage 2 of 8"), "{value}");
    }

    #[test]
    fn a_failure_is_announced_with_its_reason() {
        let mut model = Model::new();
        model.update(Msg::Dropped(std::path::PathBuf::from("/tmp/talk.wav")));
        model.update(Msg::Finished(Err("that is not a wav file".into())));

        let status = built(&model)
            .into_iter()
            .find(|e| e.role == Role::StaticText)
            .expect("a status element");
        assert_eq!(status.label, "Could not process that file");
        assert_eq!(status.value.as_deref(), Some("that is not a wav file"));
    }

    #[test]
    fn every_element_can_be_reached_by_clicking_where_it_says_it_is() {
        // The point of building these from the layout: what is announced at a
        // position is what a click there would find.
        let mut model = Model::new();
        model.update(Msg::ToggleExpanded(0));
        let layout = layout::compute(&model, 760.0, 900.0);

        for element in elements(&model, &layout) {
            let Some(hit) = element.hit else {
                continue;
            };
            let x = element.rect.x + element.rect.width / 2.0;
            let y = element.rect.y + element.rect.height / 2.0;
            // A slider's element spans its label as well as its track, so its
            // centre may land on the label — checking the track is the honest
            // version.
            let (x, y) = match hit {
                Hit::Slider(stage, param) => {
                    let control = layout.rows[stage].params[param].control;
                    (control.x + control.width / 2.0, control.y)
                }
                _ => (x, y),
            };
            assert_eq!(
                layout.hit(x, y),
                Some(hit),
                "{} is announced at ({x}, {y}) but a click there finds {:?}",
                element.label,
                layout.hit(x, y)
            );
        }
    }
}
