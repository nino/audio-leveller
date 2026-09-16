//! Reading and writing the folders on disk.
//!
//! A listening root holds `sessions/<name>/` and `annotate/`. Nothing here
//! reaches for a network or a database: the whole thing is files, which is what
//! makes a session something you can copy to another machine, put in a
//! repository, or hand to someone.
//!
//! The one rule worth stating: [`Store::key`] is the only way to read a
//! session's blinding, and nothing calls it except the reveal. A listener who
//! has seen the key cannot unsee it.

use std::path::{Path, PathBuf};

use crate::types::{AnnotationFile, Results, Session, SessionKey, default_annotation_labels};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not the shape it should be: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

type Result<T> = std::result::Result<T, StoreError>;

/// A listening root.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn session_dir(&self, name: &str) -> PathBuf {
        self.sessions_dir().join(name)
    }

    pub fn annotate_dir(&self) -> PathBuf {
        self.root.join("annotate")
    }

    /// Every session name, sorted.
    ///
    /// A directory with no `session.json` is not a session, and is skipped
    /// rather than reported as a broken one — the folder may be a stray, or a
    /// session halfway through being built.
    pub fn sessions(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.sessions_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().join("session.json").is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }

    pub fn session(&self, name: &str) -> Result<Session> {
        read_json(&self.session_dir(name).join("session.json"))
    }

    /// The blinding.
    ///
    /// Separate from [`Store::session`] on purpose, and called from exactly one
    /// place: the reveal.
    pub fn key(&self, name: &str) -> Result<SessionKey> {
        read_json(&self.session_dir(name).join("key.json"))
    }

    pub fn results_path(&self, session: &str, listener: &str) -> PathBuf {
        self.session_dir(session)
            .join(format!("results.{}.json", slug(listener)))
    }

    /// One listener's results, or `None` when they have not started.
    pub fn results(&self, session: &str, listener: &str) -> Result<Option<Results>> {
        let path = self.results_path(session, listener);
        if !path.is_file() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    /// Every set of results for a session, sorted by listener.
    pub fn all_results(&self, session: &str) -> Vec<Results> {
        let mut all: Vec<Results> = std::fs::read_dir(self.session_dir(session))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("results.") && name.ends_with(".json")
            })
            .filter_map(|entry| read_json::<Results>(&entry.path()).ok())
            .collect();
        all.sort_by(|a, b| a.listener.cmp(&b.listener));
        all
    }

    pub fn save_results(&self, results: &Results) -> Result<()> {
        write_json(
            &self.results_path(&results.session, &results.listener),
            results,
        )
    }

    /// The WAV files under `annotate/`, sorted.
    pub fn annotatable(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.annotate_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.to_ascii_lowercase().ends_with(".wav"))
            .collect();
        names.sort();
        names
    }

    pub fn annotations_path(&self, wav: &str) -> PathBuf {
        self.annotate_dir().join(annotations_file_for(wav))
    }

    /// The annotations for a file, or an empty set when there are none yet.
    ///
    /// An absent file is not an error: it is the ordinary state of a recording
    /// nobody has marked up.
    pub fn annotations(&self, wav: &str) -> Result<AnnotationFile> {
        let path = self.annotations_path(wav);
        if !path.is_file() {
            return Ok(empty_annotations(wav, 0, 0.0));
        }
        read_json(&path)
    }

    pub fn save_annotations(&self, file: &AnnotationFile) -> Result<()> {
        write_json(&self.annotations_path(&file.file), file)
    }
}

/// `foo.wav` becomes `foo.annotations.json`, kept next to the WAV.
pub fn annotations_file_for(wav: &str) -> String {
    let stem = wav
        .strip_suffix(".wav")
        .or_else(|| wav.strip_suffix(".WAV"))
        .unwrap_or(wav);
    format!("{stem}.annotations.json")
}

pub fn empty_annotations(file: &str, sample_rate: u32, duration: f64) -> AnnotationFile {
    AnnotationFile {
        file: file.to_string(),
        sample_rate,
        duration,
        labels: default_annotation_labels(),
        annotations: Vec::new(),
        updated_at: now(),
    }
}

/// A listener's name, made safe for a file name.
///
/// Not a general slug: it only has to stop a name with a slash or a colon in it
/// from writing somewhere it should not.
pub fn slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "anonymous".into()
    } else {
        trimmed
    }
}

/// An ISO 8601 timestamp, as the TypeScript's `new Date().toISOString()` wrote.
///
/// Hand-rolled rather than pulling in a date library for one format: this is
/// the only place the project needs a clock.
pub fn now() -> String {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();

    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let time = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days`: a day count since the epoch to a date.
///
/// Twenty lines, no leap-year table, and correct for any date this program will
/// ever see.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|source| StoreError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| StoreError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    std::fs::write(path, text).map_err(|source| StoreError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Answer, TrialResult};
    use std::collections::BTreeMap;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            // A counter rather than the clock: macOS reports the time to the
            // microsecond, so two scratch directories made in the same instant
            // collide and one test's cleanup deletes the other's files.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "leveller-listen-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn store(&self) -> Store {
            Store::new(&self.0)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_session(store: &Store, name: &str) {
        let dir = store.session_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            serde_json::json!({
                "name": name,
                "createdAt": "2026-08-30T12:00:00.000Z",
                "loudnessLufs": -28.0,
                "matchBy": "momentaryMax",
                "clipQuestions": [],
                "trialQuestions": [],
                "trials": [{ "id": "t01", "title": "one", "clips": [] }]
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn sessions_are_listed_by_name() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "beta");
        write_session(&store, "alpha");
        assert_eq!(store.sessions(), vec!["alpha", "beta"]);
    }

    #[test]
    fn a_folder_with_no_session_in_it_is_not_a_session() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "real");
        std::fs::create_dir_all(store.session_dir("half-built")).unwrap();
        assert_eq!(store.sessions(), vec!["real"]);
    }

    #[test]
    fn a_missing_root_lists_nothing_rather_than_failing() {
        // The ordinary state before anyone has made a session.
        let store = Store::new("/nowhere/at/all");
        assert!(store.sessions().is_empty());
        assert!(store.annotatable().is_empty());
        assert!(store.all_results("anything").is_empty());
    }

    #[test]
    fn a_session_is_read_back_as_it_was_written() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "chili");
        let session = store.session("chili").expect("it should read");
        assert_eq!(session.name, "chili");
        assert_eq!(session.trials.len(), 1);
    }

    #[test]
    fn a_broken_session_says_which_file_and_why() {
        let scratch = Scratch::new();
        let store = scratch.store();
        std::fs::create_dir_all(store.session_dir("broken")).unwrap();
        std::fs::write(store.session_dir("broken").join("session.json"), "{ nope").unwrap();

        let error = store.session("broken").unwrap_err();
        assert!(matches!(error, StoreError::Parse { .. }), "{error}");
        assert!(error.to_string().contains("session.json"), "{error}");
    }

    #[test]
    fn results_round_trip_through_the_store() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "chili");

        let mut trial = TrialResult::default();
        trial.trial.insert("best".into(), Answer::Text("B".into()));
        let results = Results {
            session: "chili".into(),
            listener: "Nino".into(),
            started_at: now(),
            updated_at: now(),
            trials: BTreeMap::from([("t01".into(), trial)]),
        };

        store.save_results(&results).expect("saving");
        let read = store
            .results("chili", "Nino")
            .expect("reading")
            .expect("some");
        assert_eq!(read, results);
    }

    #[test]
    fn a_listener_who_has_not_started_has_no_results() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "chili");
        assert_eq!(store.results("chili", "nobody").unwrap(), None);
    }

    #[test]
    fn every_listener_is_gathered_and_sorted() {
        let scratch = Scratch::new();
        let store = scratch.store();
        write_session(&store, "chili");

        for listener in ["zoe", "adam"] {
            store
                .save_results(&Results {
                    session: "chili".into(),
                    listener: listener.into(),
                    started_at: now(),
                    updated_at: now(),
                    trials: BTreeMap::new(),
                })
                .unwrap();
        }
        let all = store.all_results("chili");
        assert_eq!(
            all.iter().map(|r| r.listener.as_str()).collect::<Vec<_>>(),
            vec!["adam", "zoe"]
        );
    }

    #[test]
    fn a_listener_name_cannot_write_outside_the_session() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let path = store.results_path("chili", "../../etc/passwd");
        assert!(
            path.starts_with(store.session_dir("chili")),
            "{} escaped the session folder",
            path.display()
        );
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("Nino Annighöfer"), "nino-annigh-fer");
        assert_eq!(slug("  "), "anonymous");
    }

    #[test]
    fn annotations_default_to_an_empty_set() {
        let scratch = Scratch::new();
        let store = scratch.store();
        std::fs::create_dir_all(store.annotate_dir()).unwrap();

        let file = store.annotations("talk.wav").expect("reading");
        assert!(file.annotations.is_empty());
        assert_eq!(file.file, "talk.wav");
        assert_eq!(file.labels, default_annotation_labels());
    }

    #[test]
    fn annotations_round_trip_and_live_beside_the_wav() {
        let scratch = Scratch::new();
        let store = scratch.store();
        std::fs::create_dir_all(store.annotate_dir()).unwrap();

        let mut file = empty_annotations("talk.wav", 44_100, 60.0);
        file.annotations.push(crate::types::Annotation {
            id: "a1".into(),
            start: 1.0,
            end: 1.2,
            label: "breath".into(),
            note: None,
            confidence: crate::types::Confidence::Sure,
        });
        store.save_annotations(&file).expect("saving");

        assert!(store.annotate_dir().join("talk.annotations.json").is_file());
        assert_eq!(store.annotations("talk.wav").unwrap(), file);
    }

    #[test]
    fn the_annotation_file_name_is_derived_from_the_wav() {
        assert_eq!(annotations_file_for("talk.wav"), "talk.annotations.json");
        assert_eq!(annotations_file_for("talk.WAV"), "talk.annotations.json");
        // Something that is not a wav still gets a name rather than nothing.
        assert_eq!(annotations_file_for("talk"), "talk.annotations.json");
    }

    #[test]
    fn only_wav_files_are_offered_for_annotation() {
        let scratch = Scratch::new();
        let store = scratch.store();
        std::fs::create_dir_all(store.annotate_dir()).unwrap();
        for name in ["b.wav", "a.WAV", "notes.txt", "a.annotations.json"] {
            std::fs::write(store.annotate_dir().join(name), b"").unwrap();
        }
        assert_eq!(store.annotatable(), vec!["a.WAV", "b.wav"]);
    }

    #[test]
    fn the_timestamp_is_the_shape_javascript_wrote() {
        let stamp = now();
        assert_eq!(stamp.len(), 24, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(&stamp[23..24], "Z");
        // And it is this century, which catches an epoch mistake.
        let year: i64 = stamp[..4].parse().unwrap();
        assert!((2020..2100).contains(&year), "{stamp}");
    }

    #[test]
    fn the_calendar_is_right_about_the_awkward_days() {
        // Day zero, a leap day, and the day after one.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        // And 1900 was not a leap year, which is the case a naive rule misses.
        assert_eq!(civil_from_days(-25_508), (1900, 3, 1));
    }
}
