//! What the listening app is, as state and messages.
//!
//! No window, no audio device: a shell turns clicks into messages and state
//! into pixels, and this decides what any of it means. Which is what lets the
//! things worth being sure about — that answers are kept, that autosave fires,
//! that the key stays sealed until it is asked for — be tested by calling
//! functions.

use std::collections::BTreeMap;
use std::path::PathBuf;

use leveller_listen::{
    Annotation, AnnotationFile, Answer, ClipQuestion, Confidence, Results, Session, SessionKey,
    Store, TrialResult, TrialQuestion,
};

/// Which page is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The list of sessions and annotatable files.
    Home,
    /// One trial of a session.
    Trial { session: String, index: usize },
    Results { session: String },
    Annotate { file: String },
}

/// What is loaded for the page currently showing.
pub enum Loaded {
    Nothing,
    Trial(Box<TrialState>),
    Results(Box<ResultsState>),
    Annotate(Box<AnnotateState>),
}

pub struct TrialState {
    pub session: Session,
    pub index: usize,
    /// The listener's answers so far, saved as they change.
    pub results: Results,
    /// Which clip is selected.
    pub clip: usize,
    /// Where the clips are on disk, in the trial's order.
    pub paths: Vec<PathBuf>,
    /// When this trial was opened, for the time-spent figure.
    pub opened_at: std::time::Instant,
}

impl TrialState {
    pub fn trial(&self) -> Option<&leveller_listen::Trial> {
        self.session.trials.get(self.index)
    }

    pub fn clip_questions(&self) -> &[ClipQuestion] {
        &self.session.clip_questions
    }

    pub fn trial_questions(&self) -> &[TrialQuestion] {
        &self.session.trial_questions
    }

    /// This trial's answers, creating them if this is the first look.
    fn entry(&mut self) -> &mut TrialResult {
        let id = self
            .trial()
            .map(|t| t.id.clone())
            .unwrap_or_else(|| self.index.to_string());
        self.results.trials.entry(id).or_default()
    }

    pub fn clip_answer(&self, label: &str, question: &str) -> Option<&Answer> {
        let id = self.trial()?.id.as_str();
        self.results.trials.get(id)?.clips.get(label)?.get(question)
    }

    pub fn trial_answer(&self, question: &str) -> Option<&Answer> {
        let id = self.trial()?.id.as_str();
        self.results.trials.get(id)?.trial.get(question)
    }

    /// How much of this trial has been answered, for the progress the home page
    /// shows.
    pub fn answered(&self) -> usize {
        let Some(id) = self.trial().map(|t| t.id.as_str()) else {
            return 0;
        };
        self.results.trials.get(id).map_or(0, |t| {
            t.clips.values().map(BTreeMap::len).sum::<usize>() + t.trial.len()
        })
    }
}

pub struct ResultsState {
    pub session: Session,
    /// Everyone who scored it.
    pub results: Vec<Results>,
    /// The blinding — only once it has been asked for.
    ///
    /// `None` is not "missing": it is "not revealed", which is the state a
    /// session spends most of its life in.
    pub key: Option<SessionKey>,
}

pub struct AnnotateState {
    pub file: AnnotationFile,
    pub peaks: leveller_listen::Peaks,
    /// The visible span, in seconds.
    pub view_start: f64,
    pub view_secs: f64,
    pub selected: Option<String>,
    /// The label the next region gets.
    pub label: usize,
    pub confidence: Confidence,
    /// Set when something has changed and not yet been written.
    pub dirty: bool,
}

impl AnnotateState {
    /// The annotations overlapping the visible span, in time order.
    pub fn visible(&self) -> Vec<&Annotation> {
        let end = self.view_start + self.view_secs;
        let mut showing: Vec<&Annotation> = self
            .file
            .annotations
            .iter()
            .filter(|a| a.end >= self.view_start && a.start <= end)
            .collect();
        showing.sort_by(|a, b| a.start.total_cmp(&b.start));
        showing
    }

    pub fn selected_annotation(&self) -> Option<&Annotation> {
        let id = self.selected.as_deref()?;
        self.file.annotations.iter().find(|a| a.id == id)
    }
}

#[derive(Debug)]
pub enum Msg {
    Go(Route),
    /// The listener's name, which decides which results file is theirs.
    SetListener(String),
    SelectClip(usize),
    /// Answer a question about one clip.
    AnswerClip {
        label: String,
        question: String,
        answer: Answer,
    },
    /// Answer a question about the trial as a whole.
    AnswerTrial {
        question: String,
        answer: Answer,
    },
    NextTrial,
    PreviousTrial,
    /// Show the blinding. One way, deliberately.
    Reveal,
    /// Add a region at the current selection.
    AddAnnotation { start: f64, end: f64 },
    SelectAnnotation(Option<String>),
    DeleteAnnotation(String),
    SetAnnotationLabel(usize),
    SetAnnotationConfidence(Confidence),
    /// Scroll and zoom the annotation view.
    SetView { start: f64, secs: f64 },
    /// Write whatever has changed.
    Save,
    WindowActive(bool),
}

/// Something the shell has to do.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Load these clips into the player, in this order.
    LoadClips(Vec<PathBuf>),
    /// Stop the player: the page changed.
    StopPlaying,
}

pub struct Model {
    store: Store,
    route: Route,
    loaded: Loaded,
    listener: String,
    sessions: Vec<String>,
    annotatable: Vec<String>,
    error: Option<String>,
    window_active: bool,
    /// Set of ids for new annotations, so each gets a fresh one.
    next_annotation: usize,
}

impl Model {
    pub fn new(store: Store) -> Self {
        let sessions = store.sessions();
        let annotatable = store.annotatable();
        Self {
            store,
            route: Route::Home,
            loaded: Loaded::Nothing,
            listener: whoami(),
            sessions,
            annotatable,
            error: None,
            window_active: true,
            next_annotation: 0,
        }
    }

    /// The store the pages read from, which the tests check the writes in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn listener(&self) -> &str {
        &self.listener
    }

    pub fn sessions(&self) -> &[String] {
        &self.sessions
    }

    pub fn annotatable(&self) -> &[String] {
        &self.annotatable
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn window_active(&self) -> bool {
        self.window_active
    }

    pub fn trial(&self) -> Option<&TrialState> {
        match &self.loaded {
            Loaded::Trial(state) => Some(state),
            _ => None,
        }
    }

    pub fn results(&self) -> Option<&ResultsState> {
        match &self.loaded {
            Loaded::Results(state) => Some(state),
            _ => None,
        }
    }

    pub fn annotate(&self) -> Option<&AnnotateState> {
        match &self.loaded {
            Loaded::Annotate(state) => Some(state),
            _ => None,
        }
    }

    pub fn annotate_mut(&mut self) -> Option<&mut AnnotateState> {
        match &mut self.loaded {
            Loaded::Annotate(state) => Some(state),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: Msg) -> Option<Effect> {
        match msg {
            Msg::Go(route) => self.go(route),

            Msg::SetListener(name) => {
                self.listener = name;
                // Reload, so the answers shown are the ones belonging to
                // whoever is now sitting there.
                let route = self.route.clone();
                self.go(route)
            }

            Msg::SelectClip(index) => {
                if let Loaded::Trial(state) = &mut self.loaded {
                    let clips = state.trial().map_or(0, |t| t.clips.len());
                    if index < clips {
                        state.clip = index;
                    }
                }
                None
            }

            Msg::AnswerClip {
                label,
                question,
                answer,
            } => {
                if let Loaded::Trial(state) = &mut self.loaded {
                    state
                        .entry()
                        .clips
                        .entry(label)
                        .or_default()
                        .insert(question, answer);
                    self.save_trial();
                }
                None
            }

            Msg::AnswerTrial { question, answer } => {
                if let Loaded::Trial(state) = &mut self.loaded {
                    state.entry().trial.insert(question, answer);
                    self.save_trial();
                }
                None
            }

            Msg::NextTrial | Msg::PreviousTrial => {
                let forwards = matches!(msg, Msg::NextTrial);
                let Loaded::Trial(state) = &self.loaded else {
                    return None;
                };
                let count = state.session.trials.len();
                let index = if forwards {
                    state.index + 1
                } else {
                    state.index.checked_sub(1)?
                };
                if index >= count {
                    // Past the last trial is the results page, which is where
                    // someone who has finished wants to be.
                    return self.go(Route::Results {
                        session: state.session.name.clone(),
                    });
                }
                let session = state.session.name.clone();
                self.go(Route::Trial { session, index })
            }

            Msg::Reveal => {
                if let Loaded::Results(state) = &mut self.loaded {
                    match self.store.key(&state.session.name) {
                        Ok(key) => state.key = Some(key),
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                None
            }

            Msg::AddAnnotation { start, end } => {
                let id = format!("a{}", self.next_annotation);
                self.next_annotation += 1;
                if let Loaded::Annotate(state) = &mut self.loaded {
                    let label = state
                        .file
                        .labels
                        .get(state.label)
                        .cloned()
                        .unwrap_or_else(|| "other".into());
                    state.file.annotations.push(Annotation {
                        id: id.clone(),
                        start: start.min(end),
                        end: start.max(end),
                        label,
                        note: None,
                        confidence: state.confidence,
                    });
                    state.selected = Some(id);
                    state.dirty = true;
                    self.save_annotations();
                }
                None
            }

            Msg::SelectAnnotation(id) => {
                if let Loaded::Annotate(state) = &mut self.loaded {
                    state.selected = id;
                }
                None
            }

            Msg::DeleteAnnotation(id) => {
                if let Loaded::Annotate(state) = &mut self.loaded {
                    state.file.annotations.retain(|a| a.id != id);
                    if state.selected.as_deref() == Some(id.as_str()) {
                        state.selected = None;
                    }
                    state.dirty = true;
                    self.save_annotations();
                }
                None
            }

            Msg::SetAnnotationLabel(index) => {
                if let Loaded::Annotate(state) = &mut self.loaded {
                    if index >= state.file.labels.len() {
                        return None;
                    }
                    state.label = index;
                    // A label chosen while something is selected changes that
                    // one, which is how the keyboard shortcuts are meant to be
                    // used: select, then press b for breath.
                    if let Some(id) = state.selected.clone() {
                        let label = state.file.labels[index].clone();
                        if let Some(a) = state.file.annotations.iter_mut().find(|a| a.id == id) {
                            a.label = label;
                            state.dirty = true;
                        }
                    }
                    self.save_annotations();
                }
                None
            }

            Msg::SetAnnotationConfidence(confidence) => {
                if let Loaded::Annotate(state) = &mut self.loaded {
                    state.confidence = confidence;
                    if let Some(id) = state.selected.clone()
                        && let Some(a) = state.file.annotations.iter_mut().find(|a| a.id == id)
                    {
                        a.confidence = confidence;
                        state.dirty = true;
                    }
                    self.save_annotations();
                }
                None
            }

            Msg::SetView { start, secs } => {
                if let Loaded::Annotate(state) = &mut self.loaded {
                    let duration = state.file.duration.max(0.001);
                    // Never past the ends, and never so far in that the view is
                    // shorter than a millisecond.
                    state.view_secs = secs.clamp(0.001, duration);
                    state.view_start = start.clamp(0.0, (duration - state.view_secs).max(0.0));
                }
                None
            }

            Msg::Save => {
                self.save_trial();
                self.save_annotations();
                None
            }

            Msg::WindowActive(active) => {
                self.window_active = active;
                None
            }
        }
    }

    fn go(&mut self, route: Route) -> Option<Effect> {
        self.error = None;
        // Whatever was open is written before it is closed, so nothing is lost
        // by clicking away.
        self.save_trial();
        self.save_annotations();

        let effect = match &route {
            Route::Home => {
                self.sessions = self.store.sessions();
                self.annotatable = self.store.annotatable();
                self.loaded = Loaded::Nothing;
                Some(Effect::StopPlaying)
            }

            Route::Trial { session, index } => match self.load_trial(session, *index) {
                Ok((state, paths)) => {
                    self.loaded = Loaded::Trial(Box::new(state));
                    Some(Effect::LoadClips(paths))
                }
                Err(error) => {
                    self.error = Some(error);
                    self.loaded = Loaded::Nothing;
                    Some(Effect::StopPlaying)
                }
            },

            Route::Results { session } => {
                match self.store.session(session) {
                    Ok(loaded) => {
                        self.loaded = Loaded::Results(Box::new(ResultsState {
                            results: self.store.all_results(&loaded.name),
                            session: loaded,
                            // Sealed until asked for.
                            key: None,
                        }));
                    }
                    Err(error) => {
                        self.error = Some(error.to_string());
                        self.loaded = Loaded::Nothing;
                    }
                }
                Some(Effect::StopPlaying)
            }

            Route::Annotate { file } => match self.load_annotate(file) {
                Ok(state) => {
                    self.loaded = Loaded::Annotate(Box::new(state));
                    // The same machinery the trials use: one clip rather than
                    // several, so marking a breath can be checked by listening
                    // to it.
                    Some(Effect::LoadClips(vec![self.store.annotate_dir().join(file)]))
                }
                Err(error) => {
                    self.error = Some(error);
                    self.loaded = Loaded::Nothing;
                    Some(Effect::StopPlaying)
                }
            },
        };

        self.route = route;
        effect
    }

    fn load_trial(&self, session: &str, index: usize) -> Result<(TrialState, Vec<PathBuf>), String> {
        let loaded = self.store.session(session).map_err(|e| e.to_string())?;
        let trial = loaded
            .trials
            .get(index)
            .ok_or_else(|| format!("{session} has no trial {index}"))?;

        let dir = self.store.session_dir(session);
        let paths: Vec<PathBuf> = trial.clips.iter().map(|c| dir.join(&c.file)).collect();

        let results = self
            .store
            .results(session, &self.listener)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| Results {
                session: session.to_string(),
                listener: self.listener.clone(),
                started_at: leveller_listen::store::now(),
                updated_at: leveller_listen::store::now(),
                trials: BTreeMap::new(),
            });

        Ok((
            TrialState {
                session: loaded,
                index,
                results,
                clip: 0,
                paths: paths.clone(),
                opened_at: std::time::Instant::now(),
            },
            paths,
        ))
    }

    fn load_annotate(&self, file: &str) -> Result<AnnotateState, String> {
        let path = self.store.annotate_dir().join(file);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let audio = leveller_wav::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;

        let mut annotations = self.store.annotations(file).map_err(|e| e.to_string())?;
        // The file is the authority on its own length and rate; a stored one
        // could be from an earlier version of the recording.
        annotations.sample_rate = audio.signal.sample_rate();
        annotations.duration = audio.signal.duration_secs();

        Ok(AnnotateState {
            view_secs: annotations.duration.clamp(0.001, 30.0),
            view_start: 0.0,
            peaks: leveller_listen::Peaks::build(&audio.signal),
            file: annotations,
            selected: None,
            label: 0,
            confidence: Confidence::Sure,
            dirty: false,
        })
    }

    /// Write the open trial's answers.
    ///
    /// Called on every change rather than on a timer: a listening test is
    /// twenty minutes of someone's attention, and losing it to a crash is not
    /// recoverable by asking them to do it again.
    fn save_trial(&mut self) {
        let Loaded::Trial(state) = &mut self.loaded else {
            return;
        };
        let elapsed = state.opened_at.elapsed().as_secs_f64() * 1000.0;
        let entry = state.entry();
        entry.time_spent_ms = elapsed;
        state.results.updated_at = leveller_listen::store::now();

        if let Err(error) = self.store.save_results(&state.results) {
            self.error = Some(error.to_string());
        }
    }

    fn save_annotations(&mut self) {
        let Loaded::Annotate(state) = &mut self.loaded else {
            return;
        };
        if !state.dirty {
            return;
        }
        state.file.updated_at = leveller_listen::store::now();
        state.file.annotations.sort_by(|a, b| a.start.total_cmp(&b.start));
        match self.store.save_annotations(&state.file) {
            Ok(()) => state.dirty = false,
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

/// Who is sitting there, as far as the system knows.
fn whoami() -> String {
    std::env::var("USER")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "listener".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use leveller_listen::{Clip, Trial};

    pub struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            // A counter rather than the clock: macOS reports the time to the
            // microsecond, so two fixtures built in the same instant get the
            // same directory and one test's cleanup deletes the other's session.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "listen-model-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session() -> Session {
        Session {
            name: "chili".into(),
            created_at: leveller_listen::store::now(),
            loudness_lufs: -28.0,
            match_by: leveller_listen::MatchBy::MomentaryMax,
            clip_questions: leveller_listen::default_clip_questions(),
            trial_questions: leveller_listen::default_trial_questions(),
            trials: vec![
                Trial {
                    id: "t01".into(),
                    title: "loud passage".into(),
                    clips: vec![
                        Clip {
                            label: "A".into(),
                            file: "t01_A.wav".into(),
                        },
                        Clip {
                            label: "B".into(),
                            file: "t01_B.wav".into(),
                        },
                    ],
                },
                Trial {
                    id: "t02".into(),
                    title: "quiet passage".into(),
                    clips: vec![Clip {
                        label: "A".into(),
                        file: "t02_A.wav".into(),
                    }],
                },
            ],
        }
    }

    /// A store with one two-trial session in it, and a model looking at it.
    pub fn fixture() -> (Scratch, Model) {
        let scratch = Scratch::new();
        let store = Store::new(&scratch.0);
        let session = session();
        let dir = store.session_dir(&session.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_string(&session).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("key.json"),
            serde_json::json!({
                "name": "chili",
                "variants": { "ours": { "file": "a.wav" }, "auphonic": { "file": "b.wav" } },
                "clips": { "t01": { "A": "ours", "B": "auphonic" } },
                "windows": { "t01": { "start": 0.0, "dur": 20.0 } }
            })
            .to_string(),
        )
        .unwrap();

        let mut model = Model::new(Store::new(&scratch.0));
        model.update(Msg::SetListener("nino".into()));
        (scratch, model)
    }

    #[test]
    fn it_opens_on_the_list_of_sessions() {
        let (_scratch, model) = fixture();
        assert_eq!(model.route(), &Route::Home);
        assert_eq!(model.sessions(), &["chili".to_string()]);
        assert!(model.trial().is_none());
    }

    #[test]
    fn opening_a_trial_loads_its_clips_in_order() {
        let (_scratch, mut model) = fixture();
        let effect = model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));

        let Some(Effect::LoadClips(paths)) = effect else {
            panic!("no clips: {effect:?}");
        };
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("t01_A.wav"));
        assert!(paths[1].ends_with("t01_B.wav"));

        let trial = model.trial().expect("a trial");
        assert_eq!(trial.trial().unwrap().title, "loud passage");
        assert_eq!(trial.clip, 0);
    }

    #[test]
    fn an_answer_is_kept_and_written_at_once() {
        // A listening test is twenty minutes of someone's attention, and losing
        // it to a crash cannot be fixed by asking them to do it again.
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::AnswerClip {
            label: "A".into(),
            question: "distortion".into(),
            answer: Answer::Number(3.0),
        });

        assert_eq!(
            model.trial().unwrap().clip_answer("A", "distortion"),
            Some(&Answer::Number(3.0))
        );
        // And it is on disk already, not waiting for a save.
        let saved = model
            .store()
            .results("chili", "nino")
            .unwrap()
            .expect("results should exist");
        assert_eq!(
            saved.trials["t01"].clips["A"]["distortion"],
            Answer::Number(3.0)
        );
    }

    #[test]
    fn answers_come_back_when_the_trial_is_opened_again() {
        let (_scratch, mut model) = fixture();
        let open = |model: &mut Model, index: usize| {
            model.update(Msg::Go(Route::Trial {
                session: "chili".into(),
                index,
            }));
        };

        open(&mut model, 0);
        model.update(Msg::AnswerTrial {
            question: "best".into(),
            answer: Answer::Text("B".into()),
        });
        open(&mut model, 1);
        open(&mut model, 0);
        assert_eq!(model.error(), None);

        assert_eq!(
            model.trial().unwrap().trial_answer("best"),
            Some(&Answer::Text("B".into()))
        );
    }

    #[test]
    fn two_listeners_keep_separate_answers() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::AnswerTrial {
            question: "best".into(),
            answer: Answer::Text("A".into()),
        });

        model.update(Msg::SetListener("someone-else".into()));
        assert_eq!(model.trial().unwrap().trial_answer("best"), None);

        model.update(Msg::SetListener("nino".into()));
        assert_eq!(
            model.trial().unwrap().trial_answer("best"),
            Some(&Answer::Text("A".into()))
        );
    }

    #[test]
    fn moving_past_the_last_trial_lands_on_the_results() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 1,
        }));
        model.update(Msg::NextTrial);
        assert_eq!(
            model.route(),
            &Route::Results {
                session: "chili".into()
            }
        );
    }

    #[test]
    fn moving_back_from_the_first_trial_does_nothing() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::PreviousTrial);
        assert_eq!(
            model.route(),
            &Route::Trial {
                session: "chili".into(),
                index: 0
            }
        );
    }

    #[test]
    fn the_key_stays_sealed_until_it_is_asked_for() {
        // The whole point of a blind test. Opening the results does not reveal
        // anything; a listener has to say so.
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Results {
            session: "chili".into(),
        }));
        assert!(model.results().unwrap().key.is_none());

        model.update(Msg::Reveal);
        let key = model.results().unwrap().key.as_ref().expect("revealed");
        assert_eq!(key.clips["t01"]["A"], "ours");
        assert_eq!(key.clips["t01"]["B"], "auphonic");
    }

    #[test]
    fn selecting_a_clip_that_is_not_there_changes_nothing() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        model.update(Msg::SelectClip(1));
        assert_eq!(model.trial().unwrap().clip, 1);
        model.update(Msg::SelectClip(9));
        assert_eq!(model.trial().unwrap().clip, 1, "still the last valid one");
    }

    #[test]
    fn a_session_that_is_not_there_is_an_error_rather_than_a_blank_page() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "nonesuch".into(),
            index: 0,
        }));
        assert!(model.error().is_some(), "it should say what went wrong");
        assert!(model.trial().is_none());
    }

    #[test]
    fn a_trial_index_past_the_end_is_an_error() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 99,
        }));
        assert!(model.error().unwrap().contains("no trial 99"));
    }

    #[test]
    fn leaving_a_page_stops_the_player() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        assert_eq!(model.update(Msg::Go(Route::Home)), Some(Effect::StopPlaying));
    }

    #[test]
    fn time_spent_is_recorded_per_trial() {
        let (_scratch, mut model) = fixture();
        model.update(Msg::Go(Route::Trial {
            session: "chili".into(),
            index: 0,
        }));
        std::thread::sleep(std::time::Duration::from_millis(20));
        model.update(Msg::AnswerTrial {
            question: "best".into(),
            answer: Answer::Text("A".into()),
        });

        let saved = model.store().results("chili", "nino").unwrap().unwrap();
        assert!(saved.trials["t01"].time_spent_ms >= 20.0);
    }
}
