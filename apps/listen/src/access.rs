//! What VoiceOver and the accessibility inspector see.
//!
//! Built from the same layout the drawing and the hit-testing use, so what is
//! announced is exactly what is on screen and exactly what a click would find.
//!
//! A listening test is a thing you do with your ears, so this one matters more
//! than most: someone should be able to run a whole session without looking at
//! it. That is why the clip buttons announce which one is playing, why every
//! scale step says what its number means, and why the annotation regions are
//! elements in their own right with their times read out.

use leveller_listen::{Answer, ClipQuestion, Confidence, TrialQuestion};

use crate::layout::{Hit, Layout, Page, Rect};
use crate::model::Model;

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
    RadioGroup,
    Radio,
    StaticText,
    Group,
}

struct Builder {
    elements: Vec<Element>,
}

impl Builder {
    fn button(&mut self, rect: Rect, hit: Hit, label: impl Into<String>, enabled: bool) {
        self.elements.push(Element {
            rect,
            role: Role::Button,
            label: label.into(),
            value: None,
            help: None,
            hit: Some(hit),
            enabled,
        });
    }

    fn radio(&mut self, rect: Rect, hit: Hit, label: impl Into<String>, on: bool) {
        self.elements.push(Element {
            rect,
            role: Role::Radio,
            label: label.into(),
            value: Some(if on { "selected" } else { "not selected" }.into()),
            help: None,
            hit: Some(hit),
            enabled: true,
        });
    }

    fn check(&mut self, rect: Rect, hit: Hit, label: impl Into<String>, on: bool) {
        self.elements.push(Element {
            rect,
            role: Role::Checkbox,
            label: label.into(),
            value: Some(if on { "on" } else { "off" }.into()),
            help: None,
            hit: Some(hit),
            enabled: true,
        });
    }

    fn text(&mut self, rect: Rect, label: impl Into<String>) {
        self.elements.push(Element {
            rect,
            role: Role::StaticText,
            label: label.into(),
            value: None,
            help: None,
            hit: None,
            enabled: true,
        });
    }

    fn group(&mut self, rect: Rect, label: impl Into<String>, value: Option<String>) {
        self.push_group(rect, Role::Group, label, value);
    }

    /// A set of choices where exactly one is picked, which is what makes
    /// VoiceOver say "1 of 6" as it moves along.
    fn radio_group(&mut self, rect: Rect, label: impl Into<String>, value: Option<String>) {
        self.push_group(rect, Role::RadioGroup, label, value);
    }

    fn push_group(
        &mut self,
        rect: Rect,
        role: Role,
        label: impl Into<String>,
        value: Option<String>,
    ) {
        self.elements.push(Element {
            rect,
            role,
            label: label.into(),
            value,
            help: None,
            hit: None,
            enabled: true,
        });
    }
}

/// Every element in the window, in reading order — which is also the order Tab
/// moves through them.
pub fn elements(model: &Model, layout: &Layout) -> Vec<Element> {
    let mut b = Builder {
        elements: Vec::new(),
    };

    for (frame, light) in aqua::chrome::light_frames(layout.titlebar.height)
        .into_iter()
        .zip(aqua::palette::Light::ALL)
    {
        let rect = Rect::new(
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        );
        b.button(rect, Hit::Light(light), light.label(), true);
    }

    if let (Some(rect), Some(message)) = (layout.error, model.error()) {
        b.text(rect, format!("Error: {message}"));
    }

    match &layout.page {
        Page::Home(page) => {
            b.text(page.heading, "Listening tests");
            for row in &page.sessions {
                let name = &model.sessions()[row.index];
                b.button(row.open, Hit::OpenSession(row.index), format!("Listen to {name}"), true);
                b.button(
                    row.results,
                    Hit::OpenResults(row.index),
                    format!("Results for {name}"),
                    true,
                );
            }
            for row in &page.files {
                let name = &model.annotatable()[row.index];
                b.button(row.open, Hit::OpenAnnotate(row.index), format!("Annotate {name}"), true);
            }
        }

        Page::Trial(page) => {
            let Some(state) = model.trial() else {
                return b.elements;
            };
            b.button(page.back, Hit::Home, "Back to sessions", true);
            b.text(
                page.heading,
                format!(
                    "{}, trial {} of {}: {}",
                    state.session.name,
                    state.index + 1,
                    state.session.trials.len(),
                    state.trial().map_or("", |t| t.title.as_str())
                ),
            );

            b.group(
                Rect::new(
                    page.clips.first().map_or(0.0, |r| r.x),
                    page.clips.first().map_or(0.0, |r| r.y),
                    page.clips.last().map_or(0.0, |r| r.right())
                        - page.clips.first().map_or(0.0, |r| r.x),
                    page.clips.first().map_or(0.0, |r| r.height),
                ),
                "Clips",
                Some("switching keeps its place in the passage".into()),
            );
            for (i, rect) in page.clips.iter().enumerate() {
                let label = state
                    .trial()
                    .and_then(|t| t.clips.get(i))
                    .map_or_else(|| i.to_string(), |c| c.label.clone());
                b.radio(*rect, Hit::Clip(i), format!("Clip {label}"), i == state.clip);
            }
            b.button(page.play, Hit::PlayPause, "Play or pause", true);

            let label = state
                .trial()
                .and_then(|t| t.clips.get(state.clip))
                .map_or(String::new(), |c| c.label.clone());
            for row in &page.clip_questions {
                let Some(question) = state.clip_questions().get(row.index) else {
                    continue;
                };
                let answer = state.clip_answer(&label, question.id());
                match question {
                    ClipQuestion::Scale {
                        label: name,
                        min,
                        max,
                        min_label,
                        max_label,
                        ..
                    } => {
                        b.radio_group(
                            row.rect,
                            format!("{name} of clip {label}"),
                            Some(match answer {
                                Some(Answer::Number(n)) => format!("{n:.0} out of {max:.0}"),
                                _ => "not answered".into(),
                            }),
                        );
                        for (i, rect) in row.choices.iter().enumerate() {
                            let value = min + i as f64;
                            // The ends say what they mean: "0" on its own tells
                            // a listener nothing about which way the scale runs.
                            let meaning = if i == 0 {
                                min_label.clone()
                            } else if i + 1 == row.choices.len() {
                                max_label.clone()
                            } else {
                                None
                            };
                            let name = match meaning {
                                Some(word) => format!("{name} {value:.0}, {word}"),
                                None => format!("{name} {value:.0}"),
                            };
                            b.radio(
                                *rect,
                                Hit::ClipScale(row.index, i),
                                name,
                                matches!(answer, Some(Answer::Number(n)) if (n - value).abs() < 0.5),
                            );
                        }
                    }

                    ClipQuestion::Tags {
                        label: name,
                        options,
                        ..
                    } => {
                        b.group(row.rect, format!("{name} of clip {label}"), None);
                        let chosen: &[String] = match answer {
                            Some(Answer::Tags(tags)) => tags,
                            _ => &[],
                        };
                        for (i, rect) in row.choices.iter().enumerate() {
                            let Some(option) = options.get(i) else { continue };
                            b.check(
                                *rect,
                                Hit::ClipTag(row.index, i),
                                option.clone(),
                                chosen.contains(option),
                            );
                        }
                    }

                    ClipQuestion::Text { label: name, .. } => {
                        // The field itself is a real NSTextField, and announces
                        // itself; all that is left is what it is for.
                        b.text(row.label, format!("{name} about clip {label}"));
                    }
                }
            }

            for row in &page.trial_questions {
                let Some(question) = state.trial_questions().get(row.index) else {
                    continue;
                };
                let answer = state.trial_answer(question.id());
                match question {
                    TrialQuestion::Pick { label: name, .. } => {
                        b.radio_group(row.rect, name.clone(), None);
                        for (i, rect) in row.choices.iter().enumerate() {
                            let clip = state
                                .trial()
                                .and_then(|t| t.clips.get(i))
                                .map_or_else(|| i.to_string(), |c| c.label.clone());
                            let chosen =
                                matches!(answer, Some(Answer::Text(t)) if t.as_str() == clip);
                            b.radio(
                                *rect,
                                Hit::TrialPick(row.index, i),
                                format!("Clip {clip}"),
                                chosen,
                            );
                        }
                    }
                    TrialQuestion::Text { label: name, .. } => b.text(row.label, name.clone()),
                }
            }

            b.button(
                page.previous,
                Hit::PreviousTrial,
                "Previous trial",
                state.index > 0,
            );
            b.button(
                page.next,
                Hit::NextTrial,
                if state.index + 1 >= state.session.trials.len() {
                    "Finish and see the results"
                } else {
                    "Next trial"
                },
                true,
            );
        }

        Page::Results(page) => {
            let Some(state) = model.results() else {
                return b.elements;
            };
            b.button(page.back, Hit::Home, "Back to sessions", true);
            b.text(
                page.heading,
                format!(
                    "Results for {}, scored by {} listeners",
                    state.session.name,
                    state.results.len()
                ),
            );
            for ((rect, _), trial) in page.rows.iter().zip(&state.session.trials) {
                b.text(*rect, trial.title.clone());
            }
            b.elements.push(Element {
                rect: page.reveal,
                role: Role::Button,
                label: "Reveal the key".into(),
                value: None,
                help: Some(
                    "Shows which processing made which clip. There is no going back to \
                     not knowing, so leave it until the scoring is finished."
                        .into(),
                ),
                hit: Some(Hit::Reveal),
                enabled: state.key.is_none(),
            });
        }

        Page::Annotate(page) => {
            let Some(state) = model.annotate() else {
                return b.elements;
            };
            b.button(page.back, Hit::Home, "Back to sessions", true);
            b.text(page.heading, state.file.file.clone());
            b.button(page.zoom_out, Hit::ZoomOut, "Zoom out", true);
            b.button(page.zoom_in, Hit::ZoomIn, "Zoom in", true);

            b.group(
                page.waveform,
                "Waveform",
                Some(format!(
                    "{} to {} of {:.1} seconds",
                    fmt(state.view_start),
                    fmt(state.view_start + state.view_secs),
                    state.file.duration
                )),
            );
            let visible = state.visible();
            for (i, rect) in page.regions.iter().enumerate() {
                let Some(annotation) = visible.get(i) else {
                    continue;
                };
                b.elements.push(Element {
                    rect: *rect,
                    role: Role::Button,
                    label: format!(
                        "{} from {} to {}",
                        annotation.label,
                        fmt(annotation.start),
                        fmt(annotation.end)
                    ),
                    value: Some(
                        match annotation.confidence {
                            Confidence::Sure => "sure",
                            Confidence::Maybe => "unsure",
                        }
                        .into(),
                    ),
                    help: annotation.note.clone(),
                    hit: Some(Hit::Region(i)),
                    enabled: true,
                });
            }

            for (i, rect) in page.labels.iter().enumerate() {
                let Some(name) = state.file.labels.get(i) else {
                    continue;
                };
                b.radio(*rect, Hit::Label(i), name.clone(), i == state.label);
            }
            b.radio(
                page.sure,
                Hit::Confidence(Confidence::Sure),
                "Sure",
                state.confidence == Confidence::Sure,
            );
            b.radio(
                page.maybe,
                Hit::Confidence(Confidence::Maybe),
                "Maybe",
                state.confidence == Confidence::Maybe,
            );
            b.button(
                page.delete,
                Hit::DeleteRegion,
                "Delete the selected region",
                state.selected.is_some(),
            );
        }
    }

    b.elements
}

/// Seconds, said the way a person would say them.
fn fmt(secs: f64) -> String {
    format!("{:.0} minutes {:.1} seconds", (secs / 60.0).floor(), secs % 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::model::{Msg, Route};

    fn check(model: &Model) -> Vec<Element> {
        let layout = layout::compute(model, 900.0, 760.0);
        let elements = elements(model, &layout);

        // Everything that does something must be findable by clicking where it
        // says it is: an announced rectangle that hit-tests to something else
        // is worse than no announcement at all.
        for element in &elements {
            let Some(hit) = element.hit else { continue };
            let (x, y) = element.rect.centre();
            assert_eq!(
                layout.hit(x, y),
                Some(hit),
                "{:?} at {:?} is not where it says it is",
                element.label,
                element.rect
            );
        }
        elements
    }

    #[test]
    fn the_home_page_offers_every_session_and_file() {
        let (_scratch, model) = crate::model::tests::fixture();
        let elements = check(&model);
        assert!(elements.iter().any(|e| e.label == "Listen to chili"));
        assert!(elements.iter().any(|e| e.label == "Results for chili"));
    }

    #[test]
    fn the_clip_buttons_say_which_one_is_playing() {
        // Someone running the test with their eyes shut has to be able to tell
        // A from B, which is the entire task.
        let (_scratch, mut model) = crate::model::tests::fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::SelectClip(1));

        let elements = check(&model);
        let a = elements.iter().find(|e| e.label == "Clip A").unwrap();
        let b = elements.iter().find(|e| e.label == "Clip B").unwrap();
        assert_eq!(a.value.as_deref(), Some("not selected"));
        assert_eq!(b.value.as_deref(), Some("selected"));
    }

    #[test]
    fn the_ends_of_a_scale_say_what_they_mean() {
        let (_scratch, mut model) = crate::model::tests::fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        let elements = check(&model);
        assert!(
            elements
                .iter()
                .any(|e| e.label == "Distortion 0, none"),
            "the bottom of the scale should say which way it runs"
        );
        assert!(elements.iter().any(|e| e.label == "Distortion 5, severe"));
    }

    #[test]
    fn an_answered_scale_reports_its_answer() {
        let (_scratch, mut model) = crate::model::tests::fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::AnswerClip {
            label: "A".into(),
            question: "distortion".into(),
            answer: Answer::Number(3.0),
        });
        let elements = check(&model);
        let group = elements
            .iter()
            .find(|e| e.label == "Distortion of clip A")
            .unwrap();
        assert_eq!(group.value.as_deref(), Some("3 out of 5"));
        let chosen = elements
            .iter()
            .find(|e| e.label == "Distortion 3")
            .unwrap();
        assert_eq!(chosen.value.as_deref(), Some("selected"));
    }

    #[test]
    fn reveal_warns_that_it_cannot_be_undone() {
        let (_scratch, mut model) = crate::model::tests::fixture();
        model.update(Msg::Go(Route::Results {
            session: "chili".into(),
        }));
        let elements = check(&model);
        let reveal = elements
            .iter()
            .find(|e| e.label == "Reveal the key")
            .unwrap();
        assert!(reveal.enabled);
        assert!(reveal.help.as_deref().unwrap().contains("no going back"));

        model.update(Msg::Reveal);
        let elements = check(&model);
        let reveal = elements
            .iter()
            .find(|e| e.label == "Reveal the key")
            .unwrap();
        assert!(!reveal.enabled, "there is nothing left to reveal");
    }

    #[test]
    fn the_light_buttons_are_named_rather_than_coloured() {
        let (_scratch, model) = crate::model::tests::fixture();
        let elements = check(&model);
        for name in ["Close", "Minimise", "Zoom"] {
            assert!(elements.iter().any(|e| e.label == name), "missing {name}");
        }
    }
}
