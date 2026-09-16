//! The listening-test data model.
//!
//! A **session** is a folder under `listening/sessions/<name>/` holding
//! `session.json` — what the listener sees — `key.json`, the blinding the app
//! never loads until the listener asks to reveal it, the clip WAVs, and one
//! `results.<listener>.json` per person who scored it.
//!
//! Everything here serialises to exactly the JSON the TypeScript wrote, so a
//! session recorded before the rewrite still opens afterwards. That is what the
//! `camelCase` on every struct is for, and there is a test that reads a real
//! file's shape rather than a round-trip of this code's own output.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A question asked about every clip in a trial.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClipQuestion {
    #[serde(rename_all = "camelCase")]
    Scale {
        id: String,
        label: String,
        min: f64,
        max: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        min_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_label: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Tags {
        id: String,
        label: String,
        options: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Text { id: String, label: String },
}

impl ClipQuestion {
    pub fn id(&self) -> &str {
        match self {
            Self::Scale { id, .. } | Self::Tags { id, .. } | Self::Text { id, .. } => id,
        }
    }
}

/// A question asked once per trial, about the set as a whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrialQuestion {
    #[serde(rename_all = "camelCase")]
    Pick { id: String, label: String },
    #[serde(rename_all = "camelCase")]
    Text { id: String, label: String },
}

impl TrialQuestion {
    pub fn id(&self) -> &str {
        match self {
            Self::Pick { id, .. } | Self::Text { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// The blind label shown to the listener: "A", "B", …
    pub label: String,
    /// File name inside the session folder.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
    pub id: String,
    /// Free text shown to the listener, such as "loud passage, 1080 s". Never
    /// names a variant — that is the whole point of the exercise.
    pub title: String,
    pub clips: Vec<Clip>,
}

/// How loudness is matched across clips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchBy {
    /// BS.1770 gated, the broadcast answer — but it lets a peakier clip's loud
    /// words stand out, so two clips with different dynamics can share a value
    /// and still sound unequal.
    Integrated,
    /// The loudest three-second window.
    ShortTermMax,
    /// The loudest 400 ms window, so nothing is ever louder than anything else
    /// — which is what a preference judgement needs.
    #[default]
    MomentaryMax,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub name: String,
    pub created_at: String,
    /// The loudness every clip was matched to, so results are comparable.
    pub loudness_lufs: f64,
    pub match_by: MatchBy,
    pub clip_questions: Vec<ClipQuestion>,
    pub trial_questions: Vec<TrialQuestion>,
    pub trials: Vec<Trial>,
}

/// Where a trial's clips were cut from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub start: f64,
    pub dur: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyVariant {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The blinding: which variant each blind clip actually is.
///
/// Kept in its own file so the app can hold a session open without ever having
/// read it — a listener who has seen the key cannot unsee it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionKey {
    pub name: String,
    pub variants: BTreeMap<String, KeyVariant>,
    /// Trial id → clip label → variant id.
    pub clips: BTreeMap<String, BTreeMap<String, String>>,
    /// Trial id → the window it was cut from.
    pub windows: BTreeMap<String, Window>,
}

/// One listener's answer to one question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Answer {
    Number(f64),
    Text(String),
    Tags(Vec<String>),
    /// Explicitly unanswered, which is different from absent: it means the
    /// listener looked and declined.
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialResult {
    /// Clip label → question id → answer.
    pub clips: BTreeMap<String, BTreeMap<String, Answer>>,
    /// Question id → answer.
    pub trial: BTreeMap<String, Answer>,
    /// Milliseconds the listener spent with this trial open.
    pub time_spent_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Results {
    pub session: String,
    pub listener: String,
    pub started_at: String,
    pub updated_at: String,
    pub trials: BTreeMap<String, TrialResult>,
}

// ------------------------------------------------------- region annotation --

/// How sure the annotator was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    Sure,
    Maybe,
}

/// One marked region: a breath, a click, a plosive.
///
/// These are what a breath-reduction stage would be trained and scored against,
/// which is why the confidence is recorded — a detector should not be punished
/// for missing something the annotator was unsure of either.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    /// Seconds from the start of the file.
    pub start: f64,
    pub end: f64,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationFile {
    /// WAV file name inside `listening/annotate/`.
    pub file: String,
    pub sample_rate: u32,
    pub duration: f64,
    /// The label set offered in the editor. The first letter of each is its
    /// keyboard shortcut, which is why they are stored rather than hard-coded:
    /// a project with different labels gets different shortcuts.
    pub labels: Vec<String>,
    pub annotations: Vec<Annotation>,
    pub updated_at: String,
}

pub fn default_annotation_labels() -> Vec<String> {
    ["breath", "click", "plosive", "mouth-noise", "other"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

pub fn default_clip_questions() -> Vec<ClipQuestion> {
    vec![
        ClipQuestion::Scale {
            id: "distortion".into(),
            label: "Distortion".into(),
            min: 0.0,
            max: 5.0,
            min_label: Some("none".into()),
            max_label: Some("severe".into()),
        },
        ClipQuestion::Tags {
            id: "artefacts".into(),
            label: "Artefacts".into(),
            options: [
                "harsh",
                "pumping",
                "dull",
                "noisy",
                "thin",
                "boomy",
                "clicks",
                "sibilant",
                "unnatural",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        },
        ClipQuestion::Text {
            id: "notes".into(),
            label: "Notes".into(),
        },
    ]
}

pub fn default_trial_questions() -> Vec<TrialQuestion> {
    vec![
        TrialQuestion::Pick {
            id: "best".into(),
            label: "Which sounds best overall?".into(),
        },
        TrialQuestion::Text {
            id: "comment".into(),
            label: "Anything else about this set?".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_round_trips_through_the_json_the_typescript_wrote() {
        // The field names matter more than the values: a session recorded
        // before the rewrite has to open afterwards.
        let raw = json!({
            "name": "chili-15",
            "createdAt": "2026-08-30T12:00:00.000Z",
            "loudnessLufs": -28.0,
            "matchBy": "momentaryMax",
            "clipQuestions": [
                {
                    "kind": "scale",
                    "id": "distortion",
                    "label": "Distortion",
                    "min": 0.0,
                    "max": 5.0,
                    "minLabel": "none",
                    "maxLabel": "severe"
                },
                { "kind": "tags", "id": "artefacts", "label": "Artefacts", "options": ["harsh"] },
                { "kind": "text", "id": "notes", "label": "Notes" }
            ],
            "trialQuestions": [
                { "kind": "pick", "id": "best", "label": "Which sounds best overall?" },
                { "kind": "text", "id": "comment", "label": "Anything else?" }
            ],
            "trials": [
                {
                    "id": "t01",
                    "title": "loud passage, 1080 s",
                    "clips": [
                        { "label": "A", "file": "t01_A.wav" },
                        { "label": "B", "file": "t01_B.wav" }
                    ]
                }
            ]
        });

        let session: Session = serde_json::from_value(raw.clone()).expect("it should parse");
        assert_eq!(session.name, "chili-15");
        assert_eq!(session.match_by, MatchBy::MomentaryMax);
        assert_eq!(session.clip_questions.len(), 3);
        assert_eq!(session.clip_questions[0].id(), "distortion");
        assert_eq!(session.trials[0].clips[1].file, "t01_B.wav");

        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            raw,
            "the JSON came back different from how it went in"
        );
    }

    #[test]
    fn a_scale_written_with_whole_numbers_still_parses() {
        // The TypeScript wrote `"min": 0`, JSON's integer form. Rust writes
        // `0.0` on the way back out, which JSON.parse reads identically — but
        // the two are different `Value`s, so the round-trip test above uses the
        // float form and this one covers the other.
        let question: ClipQuestion = serde_json::from_value(json!({
            "kind": "scale",
            "id": "distortion",
            "label": "Distortion",
            "min": 0,
            "max": 5
        }))
        .expect("it should parse");
        let ClipQuestion::Scale { min, max, .. } = question else {
            panic!("wrong kind");
        };
        assert_eq!((min, max), (0.0, 5.0));
    }

    #[test]
    fn a_key_round_trips_too() {
        let raw = json!({
            "name": "chili-15",
            "variants": {
                "auphonic": { "file": "chili_15_auphonic.wav", "description": "the target" },
                "ours": { "file": "chili_15_raw_processed.wav" }
            },
            "clips": { "t01": { "A": "ours", "B": "auphonic" } },
            "windows": { "t01": { "start": 1080.0, "dur": 20.0, "label": "loud passage" } }
        });

        let key: SessionKey = serde_json::from_value(raw.clone()).expect("it should parse");
        assert_eq!(key.clips["t01"]["B"], "auphonic");
        assert_eq!(key.windows["t01"].start, 1080.0);
        assert_eq!(key.variants["ours"].description, None);
        assert_eq!(serde_json::to_value(&key).unwrap(), raw);
    }

    #[test]
    fn results_round_trip_with_every_kind_of_answer() {
        let raw = json!({
            "session": "chili-15",
            "listener": "nino",
            "startedAt": "2026-08-30T12:00:00.000Z",
            "updatedAt": "2026-08-30T12:30:00.000Z",
            "trials": {
                "t01": {
                    "clips": {
                        "A": { "distortion": 2.0, "artefacts": ["harsh", "thin"], "notes": "a bit hard" },
                        "B": { "distortion": 0.0, "artefacts": [], "notes": "" }
                    },
                    "trial": { "best": "B", "comment": "B by a mile" },
                    "timeSpentMs": 48120.0
                }
            }
        });

        let results: Results = serde_json::from_value(raw.clone()).expect("it should parse");
        let trial = &results.trials["t01"];
        assert_eq!(trial.clips["A"]["distortion"], Answer::Number(2.0));
        assert_eq!(
            trial.clips["A"]["artefacts"],
            Answer::Tags(vec!["harsh".into(), "thin".into()])
        );
        assert_eq!(trial.trial["best"], Answer::Text("B".into()));
        assert_eq!(serde_json::to_value(&results).unwrap(), raw);
    }

    #[test]
    fn an_unanswered_question_is_kept_as_such() {
        // Null is different from absent: the listener looked and declined, and
        // the analysis should be able to tell those apart.
        let raw = json!({ "distortion": null });
        let answers: BTreeMap<String, Answer> = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(answers["distortion"], Answer::None);
        assert_eq!(serde_json::to_value(&answers).unwrap(), raw);
    }

    #[test]
    fn an_annotation_file_round_trips() {
        let raw = json!({
            "file": "chili_15_raw.wav",
            "sampleRate": 44100,
            "duration": 1140.5,
            "labels": ["breath", "click", "plosive", "mouth-noise", "other"],
            "annotations": [
                {
                    "id": "a1",
                    "start": 12.25,
                    "end": 12.51,
                    "label": "breath",
                    "confidence": "sure"
                },
                {
                    "id": "a2",
                    "start": 40.0,
                    "end": 40.02,
                    "label": "click",
                    "note": "might be the chair",
                    "confidence": "maybe"
                }
            ],
            "updatedAt": "2026-08-30T12:00:00.000Z"
        });

        let file: AnnotationFile = serde_json::from_value(raw.clone()).expect("it should parse");
        assert_eq!(file.annotations.len(), 2);
        assert_eq!(file.annotations[0].confidence, Confidence::Sure);
        assert_eq!(
            file.annotations[1].note.as_deref(),
            Some("might be the chair")
        );
        assert_eq!(serde_json::to_value(&file).unwrap(), raw);
    }

    #[test]
    fn an_annotation_with_no_note_writes_no_note_field() {
        // Rather than `"note": null`, which the TypeScript never wrote and
        // which would make a diff of two annotation files noisy.
        let annotation = Annotation {
            id: "a1".into(),
            start: 0.0,
            end: 1.0,
            label: "breath".into(),
            note: None,
            confidence: Confidence::Maybe,
        };
        let json = serde_json::to_value(&annotation).unwrap();
        assert!(json.get("note").is_none(), "{json}");
    }

    #[test]
    fn the_defaults_are_the_ones_the_editor_offers() {
        assert_eq!(default_annotation_labels()[0], "breath");
        assert_eq!(default_clip_questions().len(), 3);
        assert_eq!(default_trial_questions()[0].id(), "best");
        // The default matching is the one a preference judgement needs.
        assert_eq!(MatchBy::default(), MatchBy::MomentaryMax);
    }
}
