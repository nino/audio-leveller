//! Where everything goes, computed from the model and the window's size.
//!
//! The same reason as in the leveller app: drawing, hit-testing and
//! accessibility all need the rectangles, and three copies of the arithmetic
//! would be three chances to disagree.

use aqua::palette::Light;
use leveller_listen::{ClipQuestion, Confidence, TrialQuestion};

use crate::model::{Model, Route};

/// A rectangle, in points, with the origin at the top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    /// Used by the accessibility tests, which click the middle of every
    /// element they announce.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn inset(&self, dx: f64, dy: f64) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.width - dx * 2.0).max(0.0),
            (self.height - dy * 2.0).max(0.0),
        )
    }
}

/// Everything that can be clicked, and what clicking it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Light(Light),
    /// Anywhere else in the title strip, which drags the window.
    TitleBar,
    /// Back to the list of sessions.
    Home,
    /// Start session `n` at its first trial.
    OpenSession(usize),
    /// Open session `n`'s results.
    OpenResults(usize),
    /// Open annotatable file `n`.
    OpenAnnotate(usize),

    /// Switch to clip `n` of the trial, without stopping.
    Clip(usize),
    PlayPause,
    /// Move the playhead by clicking the waveform.
    Scrub,
    /// One step of a scale question: which question, which value.
    ClipScale(usize, usize),
    /// One tag of a tag question: which question, which option.
    ClipTag(usize, usize),
    /// One option of a "which is best" question: which question, which clip.
    TrialPick(usize, usize),
    PreviousTrial,
    NextTrial,
    Reveal,

    /// The annotation waveform: click to select, drag to make a region.
    Waveform,
    Region(usize),
    Label(usize),
    Confidence(Confidence),
    DeleteRegion,
    ZoomIn,
    ZoomOut,
}

/// A place where a real text field goes.
///
/// Drawing a text box is easy; editing text is not, and reimplementing
/// selection, the clipboard and input methods would be worse than the problem.
/// The shell puts an ordinary `NSTextField` over each of these instead, which
/// also means the caret, the spelling checker and VoiceOver all behave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextKey {
    /// Who is listening.
    Listener,
    /// A free-text question about the selected clip.
    Clip { label: String, question: String },
    /// A free-text question about the trial.
    Trial { question: String },
    /// The selected region's note.
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextField {
    pub rect: Rect,
    pub key: TextKey,
    pub placeholder: &'static str,
}

/// One placed clickable thing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Element {
    pub rect: Rect,
    pub hit: Hit,
}

/// A row in the list of sessions.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub index: usize,
    pub rect: Rect,
    pub name: Rect,
    pub open: Rect,
    pub results: Rect,
}

/// A row in the list of annotatable files.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRow {
    pub index: usize,
    pub rect: Rect,
    pub name: Rect,
    pub open: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomePage {
    pub heading: Rect,
    pub listener_label: Rect,
    pub listener_field: Rect,
    pub sessions_heading: Rect,
    pub sessions_panel: Rect,
    pub sessions: Vec<SessionRow>,
    pub files_heading: Rect,
    pub files_panel: Rect,
    pub files: Vec<FileRow>,
}

/// A question and the controls that answer it.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestionRow {
    pub index: usize,
    pub label: Rect,
    /// Scale steps, or tag chips, or the pick buttons — whichever the question
    /// is. Empty for a text question, which has a field instead.
    pub choices: Vec<Rect>,
    /// The whole row, for the accessibility group.
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrialPage {
    pub heading: Rect,
    pub subheading: Rect,
    pub back: Rect,
    pub clips: Vec<Rect>,
    pub play: Rect,
    pub waveform: Rect,
    pub time: Rect,
    pub clip_panel: Rect,
    pub clip_heading: Rect,
    pub clip_questions: Vec<QuestionRow>,
    pub trial_panel: Rect,
    pub trial_questions: Vec<QuestionRow>,
    pub previous: Rect,
    pub next: Rect,
    pub progress: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResultsPage {
    pub heading: Rect,
    pub back: Rect,
    pub panel: Rect,
    /// One per trial: the row's rectangle and one cell per clip.
    pub rows: Vec<(Rect, Vec<Rect>)>,
    pub reveal: Rect,
    pub key_panel: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnnotatePage {
    pub heading: Rect,
    pub back: Rect,
    pub waveform: Rect,
    pub ruler: Rect,
    pub zoom_out: Rect,
    pub zoom_in: Rect,
    /// The visible regions, in the order `AnnotateState::visible` gives them.
    pub regions: Vec<Rect>,
    pub labels: Vec<Rect>,
    pub sure: Rect,
    pub maybe: Rect,
    pub delete: Rect,
    pub note_label: Rect,
    pub note_field: Rect,
    pub counts: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Home(Box<HomePage>),
    Trial(Box<TrialPage>),
    Results(Box<ResultsPage>),
    Annotate(Box<AnnotatePage>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub titlebar: Rect,
    pub page: Page,
    pub error: Option<Rect>,
    /// Everything clickable, front to back — hit-testing takes the first match,
    /// so a control on top of a panel wins.
    pub elements: Vec<Element>,
    pub fields: Vec<TextField>,
    pub content_height: f64,
}

pub const TITLEBAR: f64 = aqua::palette::TITLEBAR_HEIGHT;
const MARGIN: f64 = 20.0;
const PAD: f64 = 14.0;
const ROW: f64 = 32.0;
const BUTTON: f64 = 24.0;
const FIELD: f64 = 22.0;

/// Work out where everything goes for a window of this size.
pub fn compute(model: &Model, width: f64, height: f64) -> Layout {
    let mut elements = Vec::new();
    let mut fields = Vec::new();
    let inner = width - MARGIN * 2.0;

    // Back to front, reversed at the end: the strip goes down before the lights
    // that sit on it, so a click on a light closes the window rather than
    // starting a drag.
    let titlebar = Rect::new(0.0, 0.0, width, TITLEBAR);
    elements.push(Element {
        rect: titlebar,
        hit: Hit::TitleBar,
    });
    for (frame, light) in aqua::chrome::light_frames(TITLEBAR)
        .into_iter()
        .zip(Light::ALL)
    {
        elements.push(Element {
            rect: Rect::new(
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            ),
            hit: Hit::Light(light),
        });
    }

    let mut y = TITLEBAR + 16.0;

    // An error sits above the page, where it cannot be missed.
    let error = model.error().map(|_| {
        let rect = Rect::new(MARGIN, y, inner, 40.0);
        y = rect.bottom() + 12.0;
        rect
    });

    let page = match model.route() {
        Route::Home => Page::Home(Box::new(home(
            model, inner, y, height, &mut elements, &mut fields,
        ))),
        Route::Trial { .. } => Page::Trial(Box::new(trial(
            model,
            inner,
            y,
            height,
            &mut elements,
            &mut fields,
        ))),
        Route::Results { .. } => Page::Results(Box::new(results(
            model,
            inner,
            y,
            &mut elements,
        ))),
        Route::Annotate { .. } => Page::Annotate(Box::new(annotate(
            model,
            inner,
            y,
            height,
            &mut elements,
            &mut fields,
        ))),
    };

    let content_height = match &page {
        Page::Home(p) => p.files_panel.bottom(),
        Page::Trial(p) => p.next.bottom(),
        Page::Results(p) => p
            .key_panel
            .map_or(p.reveal.bottom(), |k| k.bottom()),
        Page::Annotate(p) => p.counts.bottom(),
    } + MARGIN;

    // Hit-testing walks this front to back.
    elements.reverse();

    Layout {
        titlebar,
        page,
        error,
        elements,
        fields,
        content_height: content_height.max(height),
    }
}

fn home(
    model: &Model,
    inner: f64,
    top: f64,
    _height: f64,
    elements: &mut Vec<Element>,
    fields: &mut Vec<TextField>,
) -> HomePage {
    let mut y = top;
    let heading = Rect::new(MARGIN, y, inner, 22.0);
    y = heading.bottom() + 10.0;

    let listener_label = Rect::new(MARGIN, y, 64.0, FIELD);
    let listener_field = Rect::new(MARGIN + 70.0, y, 220.0, FIELD);
    fields.push(TextField {
        rect: listener_field,
        key: TextKey::Listener,
        placeholder: "your name",
    });
    y = listener_field.bottom() + 18.0;

    let sessions_heading = Rect::new(MARGIN, y, inner, 18.0);
    y = sessions_heading.bottom() + 6.0;

    let sessions_top = y;
    let mut row_y = y + PAD;
    let mut sessions = Vec::new();
    for (index, _) in model.sessions().iter().enumerate() {
        let rect = Rect::new(MARGIN + PAD, row_y, inner - PAD * 2.0, ROW);
        let results = Rect::new(rect.right() - 78.0, rect.y + 4.0, 78.0, BUTTON);
        let open = Rect::new(results.x - 76.0, rect.y + 4.0, 68.0, BUTTON);
        let name = Rect::new(rect.x, rect.y, open.x - rect.x - 8.0, ROW);
        elements.push(Element {
            rect: open,
            hit: Hit::OpenSession(index),
        });
        elements.push(Element {
            rect: results,
            hit: Hit::OpenResults(index),
        });
        sessions.push(SessionRow {
            index,
            rect,
            name,
            open,
            results,
        });
        row_y += ROW;
    }
    // A panel with nothing in it still needs to be tall enough to say so.
    let sessions_panel = Rect::new(
        MARGIN,
        sessions_top,
        inner,
        (row_y - sessions_top + PAD).max(56.0),
    );
    y = sessions_panel.bottom() + 18.0;

    let files_heading = Rect::new(MARGIN, y, inner, 18.0);
    y = files_heading.bottom() + 6.0;

    let files_top = y;
    let mut row_y = y + PAD;
    let mut files = Vec::new();
    for (index, _) in model.annotatable().iter().enumerate() {
        let rect = Rect::new(MARGIN + PAD, row_y, inner - PAD * 2.0, ROW);
        let open = Rect::new(rect.right() - 82.0, rect.y + 4.0, 82.0, BUTTON);
        let name = Rect::new(rect.x, rect.y, open.x - rect.x - 8.0, ROW);
        elements.push(Element {
            rect: open,
            hit: Hit::OpenAnnotate(index),
        });
        files.push(FileRow {
            index,
            rect,
            name,
            open,
        });
        row_y += ROW;
    }
    let files_panel = Rect::new(MARGIN, files_top, inner, (row_y - files_top + PAD).max(56.0));

    HomePage {
        heading,
        listener_label,
        listener_field,
        sessions_heading,
        sessions_panel,
        sessions,
        files_heading,
        files_panel,
        files,
    }
}

fn trial(
    model: &Model,
    inner: f64,
    top: f64,
    _height: f64,
    elements: &mut Vec<Element>,
    fields: &mut Vec<TextField>,
) -> TrialPage {
    let state = model.trial();
    let clip_count = state
        .and_then(|s| s.trial())
        .map_or(0, |t| t.clips.len())
        .max(1);
    let selected_label = state
        .and_then(|s| s.trial())
        .and_then(|t| t.clips.get(state.map_or(0, |s| s.clip)))
        .map(|c| c.label.clone())
        .unwrap_or_default();

    let mut y = top;
    let back = Rect::new(MARGIN, y, 74.0, BUTTON);
    elements.push(Element {
        rect: back,
        hit: Hit::Home,
    });
    let heading = Rect::new(back.right() + 12.0, y, inner - back.width - 12.0, 22.0);
    y = heading.bottom() + 4.0;
    let subheading = Rect::new(MARGIN, y, inner, 16.0);
    y = subheading.bottom() + 16.0;

    // The clip buttons. Big, evenly spaced, and always in the same place from
    // one trial to the next — switching between them is the whole test, and it
    // has to be doable without looking.
    let clip_width = ((inner - 120.0) / clip_count as f64).min(120.0);
    let mut clips = Vec::new();
    for i in 0..clip_count {
        let rect = Rect::new(MARGIN + i as f64 * (clip_width + 8.0), y, clip_width, 34.0);
        elements.push(Element {
            rect,
            hit: Hit::Clip(i),
        });
        clips.push(rect);
    }
    let play = Rect::new(MARGIN + inner - 96.0, y + 5.0, 96.0, BUTTON);
    elements.push(Element {
        rect: play,
        hit: Hit::PlayPause,
    });
    y += 34.0 + 12.0;

    let waveform = Rect::new(MARGIN, y, inner, 90.0);
    elements.push(Element {
        rect: waveform,
        hit: Hit::Scrub,
    });
    y = waveform.bottom() + 4.0;
    let time = Rect::new(MARGIN, y, inner, 14.0);
    y = time.bottom() + 16.0;

    // Questions about the clip that is playing.
    let clip_top = y;
    let clip_heading = Rect::new(MARGIN + PAD, y + PAD, inner - PAD * 2.0, 18.0);
    let mut row_y = clip_heading.bottom() + 10.0;
    let mut clip_questions = Vec::new();
    for (index, question) in state.map(|s| s.clip_questions()).unwrap_or(&[]).iter().enumerate() {
        let (row, next) =
            question_row(index, question_shape(question), inner, row_y, 108.0, elements);
        if let ClipQuestion::Text { id, .. } = question {
            fields.push(TextField {
                rect: Rect::new(
                    row.label.right() + 8.0,
                    row.label.y,
                    row.rect.right() - row.label.right() - 8.0,
                    FIELD,
                ),
                key: TextKey::Clip {
                    label: selected_label.clone(),
                    question: id.clone(),
                },
                placeholder: "notes",
            });
        }
        clip_questions.push(row);
        row_y = next;
    }
    let clip_panel = Rect::new(MARGIN, clip_top, inner, row_y - clip_top + PAD);
    y = clip_panel.bottom() + 14.0;

    // Questions about the set as a whole.
    let trial_top = y;
    let mut row_y = y + PAD;
    let mut trial_questions = Vec::new();
    for (index, question) in state
        .map(|s| s.trial_questions())
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let shape = match question {
            TrialQuestion::Pick { .. } => Shape::Pick(clip_count),
            TrialQuestion::Text { .. } => Shape::Text,
        };
        // The trial questions are whole sentences rather than one-word names,
        // so they get a wider column than the clip questions do.
        let (row, next) = question_row(index, shape, inner, row_y, 210.0, elements);
        if let TrialQuestion::Text { id, .. } = question {
            fields.push(TextField {
                rect: Rect::new(
                    row.label.right() + 8.0,
                    row.label.y,
                    row.rect.right() - row.label.right() - 8.0,
                    FIELD,
                ),
                key: TextKey::Trial {
                    question: id.clone(),
                },
                placeholder: "comment",
            });
        }
        trial_questions.push(row);
        row_y = next;
    }
    let trial_panel = Rect::new(MARGIN, trial_top, inner, (row_y - trial_top + PAD).max(56.0));
    y = trial_panel.bottom() + 14.0;

    let previous = Rect::new(MARGIN, y, 92.0, BUTTON);
    let next = Rect::new(MARGIN + inner - 92.0, y, 92.0, BUTTON);
    let progress = Rect::new(previous.right() + 12.0, y, inner - 208.0, BUTTON);
    elements.push(Element {
        rect: previous,
        hit: Hit::PreviousTrial,
    });
    elements.push(Element {
        rect: next,
        hit: Hit::NextTrial,
    });

    TrialPage {
        heading,
        subheading,
        back,
        clips,
        play,
        waveform,
        time,
        clip_panel,
        clip_heading,
        clip_questions,
        trial_panel,
        trial_questions,
        previous,
        next,
        progress,
    }
}

/// What kind of control a question needs, and how many of it.
#[derive(Clone, Copy)]
enum Shape {
    /// A scale from min to max, one button per whole step: with six of them a
    /// listener can hit the one they mean without aiming.
    Scale(usize),
    Tags(usize),
    /// Pick one of the trial's clips.
    Pick(usize),
    Text,
}

impl Shape {
    fn count(self) -> usize {
        match self {
            Self::Scale(n) | Self::Tags(n) | Self::Pick(n) => n,
            Self::Text => 0,
        }
    }

    fn hit(self, question: usize, choice: usize) -> Hit {
        match self {
            Self::Scale(_) => Hit::ClipScale(question, choice),
            Self::Tags(_) => Hit::ClipTag(question, choice),
            Self::Pick(_) | Self::Text => Hit::TrialPick(question, choice),
        }
    }
}

fn question_shape(question: &ClipQuestion) -> Shape {
    match question {
        ClipQuestion::Scale { min, max, .. } => Shape::Scale(((max - min).round() as usize) + 1),
        ClipQuestion::Tags { options, .. } => Shape::Tags(options.len()),
        ClipQuestion::Text { .. } => Shape::Text,
    }
}

/// Place one question's label and its controls, returning the row and the y to
/// carry on from.
fn question_row(
    index: usize,
    shape: Shape,
    inner: f64,
    y: f64,
    label_width: f64,
    elements: &mut Vec<Element>,
) -> (QuestionRow, f64) {
    let label = Rect::new(MARGIN + PAD, y, label_width, FIELD);
    let left = label.right() + 8.0;
    let available = inner - PAD * 2.0 - label_width - 8.0;

    let mut choices = Vec::new();
    let mut height = FIELD;
    let count = shape.count();
    if count > 0 {
        // Chips wrap; a scale does not, so both are laid out the same way and
        // the wrapping just never happens when they fit.
        // The gaps come out of the width rather than being added to it, or the
        // last chip in a full row hangs over the edge of the panel.
        let gaps = 4.0 * (count.max(1) - 1) as f64;
        let chip = ((available - gaps) / count.max(1) as f64).clamp(30.0, 96.0);
        let per_row = (((available + 4.0) / (chip + 4.0)).floor() as usize).max(1);
        for i in 0..count {
            let row = i / per_row;
            let column = i % per_row;
            let rect = Rect::new(
                left + column as f64 * (chip + 4.0),
                y + row as f64 * (FIELD + 4.0),
                chip,
                FIELD,
            );
            elements.push(Element {
                rect,
                hit: shape.hit(index, i),
            });
            choices.push(rect);
        }
        let rows = count.div_ceil(per_row);
        height = rows as f64 * (FIELD + 4.0) - 4.0;
        // A scale carries a word under each end saying which way it runs, and
        // the next question has to start below those rather than on top of them.
        if matches!(shape, Shape::Scale(_)) {
            height += 12.0;
        }
    }

    let rect = Rect::new(MARGIN + PAD, y, inner - PAD * 2.0, height);
    (
        QuestionRow {
            index,
            label,
            choices,
            rect,
        },
        y + height + 10.0,
    )
}

fn results(model: &Model, inner: f64, top: f64, elements: &mut Vec<Element>) -> ResultsPage {
    let mut y = top;
    let back = Rect::new(MARGIN, y, 74.0, BUTTON);
    elements.push(Element {
        rect: back,
        hit: Hit::Home,
    });
    let heading = Rect::new(back.right() + 12.0, y, inner - back.width - 12.0, 22.0);
    y = heading.bottom() + 16.0;

    let state = model.results();
    let trials = state.map_or(0, |s| s.session.trials.len());
    let columns = state
        .and_then(|s| s.session.trials.first())
        .map_or(0, |t| t.clips.len())
        .max(1);

    let panel_top = y;
    let mut row_y = y + PAD + 24.0;
    let mut rows = Vec::new();
    // The titles are sentences, so they get most of the width and the columns
    // take what is left.
    let label_width = 340.0;
    let cell = ((inner - PAD * 2.0 - label_width) / columns as f64).min(140.0);
    for _ in 0..trials {
        let rect = Rect::new(MARGIN + PAD, row_y, label_width - 12.0, 24.0);
        let cells = (0..columns)
            .map(|c| {
                Rect::new(
                    MARGIN + PAD + label_width + c as f64 * cell,
                    row_y,
                    cell,
                    24.0,
                )
            })
            .collect();
        rows.push((rect, cells));
        row_y += 24.0;
    }
    let panel = Rect::new(MARGIN, panel_top, inner, (row_y - panel_top + PAD).max(70.0));
    y = panel.bottom() + 14.0;

    let reveal = Rect::new(MARGIN, y, 150.0, BUTTON);
    elements.push(Element {
        rect: reveal,
        hit: Hit::Reveal,
    });
    y = reveal.bottom() + 14.0;

    let key_panel = state.and_then(|s| s.key.as_ref()).map(|key| {
        Rect::new(
            MARGIN,
            y,
            inner,
            (PAD * 2.0 + 24.0 + key.variants.len() as f64 * 20.0).max(60.0),
        )
    });

    ResultsPage {
        heading,
        back,
        panel,
        rows,
        reveal,
        key_panel,
    }
}

fn annotate(
    model: &Model,
    inner: f64,
    top: f64,
    _height: f64,
    elements: &mut Vec<Element>,
    fields: &mut Vec<TextField>,
) -> AnnotatePage {
    let mut y = top;
    let back = Rect::new(MARGIN, y, 74.0, BUTTON);
    elements.push(Element {
        rect: back,
        hit: Hit::Home,
    });
    let heading = Rect::new(back.right() + 12.0, y, inner - back.width - 190.0, 22.0);
    let zoom_out = Rect::new(MARGIN + inner - 92.0, y, 44.0, BUTTON);
    let zoom_in = Rect::new(MARGIN + inner - 44.0, y, 44.0, BUTTON);
    elements.push(Element {
        rect: zoom_out,
        hit: Hit::ZoomOut,
    });
    elements.push(Element {
        rect: zoom_in,
        hit: Hit::ZoomIn,
    });
    y = zoom_in.bottom() + 14.0;

    let waveform = Rect::new(MARGIN, y, inner, 300.0);
    // The waveform goes down first, so a region drawn on it takes the click.
    elements.push(Element {
        rect: waveform,
        hit: Hit::Waveform,
    });

    let state = model.annotate();
    let mut regions = Vec::new();
    if let Some(state) = state {
        let secs = state.view_secs.max(1e-9);
        for (i, annotation) in state.visible().iter().enumerate() {
            let x0 = ((annotation.start - state.view_start) / secs).clamp(0.0, 1.0);
            let x1 = ((annotation.end - state.view_start) / secs).clamp(0.0, 1.0);
            let rect = Rect::new(
                waveform.x + x0 * waveform.width,
                waveform.y,
                // Always wide enough to click, however short the region is.
                ((x1 - x0) * waveform.width).max(3.0),
                waveform.height,
            );
            elements.push(Element {
                rect,
                hit: Hit::Region(i),
            });
            regions.push(rect);
        }
    }
    y = waveform.bottom() + 4.0;
    let ruler = Rect::new(MARGIN, y, inner, 14.0);
    y = ruler.bottom() + 16.0;

    // The label buttons, whose first letters are the keyboard shortcuts.
    let names = state.map(|s| s.file.labels.as_slice()).unwrap_or(&[]);
    let mut labels = Vec::new();
    let chip = 104.0;
    for (i, _) in names.iter().enumerate() {
        let rect = Rect::new(MARGIN + i as f64 * (chip + 6.0), y, chip, BUTTON);
        elements.push(Element {
            rect,
            hit: Hit::Label(i),
        });
        labels.push(rect);
    }
    y += BUTTON + 12.0;

    let sure = Rect::new(MARGIN, y, 72.0, BUTTON);
    let maybe = Rect::new(sure.right() + 6.0, y, 72.0, BUTTON);
    let delete = Rect::new(MARGIN + inner - 90.0, y, 90.0, BUTTON);
    elements.push(Element {
        rect: sure,
        hit: Hit::Confidence(Confidence::Sure),
    });
    elements.push(Element {
        rect: maybe,
        hit: Hit::Confidence(Confidence::Maybe),
    });
    elements.push(Element {
        rect: delete,
        hit: Hit::DeleteRegion,
    });
    y = delete.bottom() + 14.0;

    let note_label = Rect::new(MARGIN, y, 44.0, FIELD);
    let note_field = Rect::new(note_label.right() + 8.0, y, inner - 52.0, FIELD);
    fields.push(TextField {
        rect: note_field,
        key: TextKey::Note,
        placeholder: "note on the selected region",
    });
    y = note_field.bottom() + 12.0;
    let counts = Rect::new(MARGIN, y, inner, 16.0);

    AnnotatePage {
        heading,
        back,
        waveform,
        ruler,
        zoom_out,
        zoom_in,
        regions,
        labels,
        sure,
        maybe,
        delete,
        note_label,
        note_field,
        counts,
    }
}

/// What a text field should be showing.
///
/// Here rather than in the shell because the offscreen renderer needs it too:
/// there are no real text fields in a PNG, so the drawing puts the values in
/// the wells itself.
pub fn value_of(model: &Model, key: &TextKey) -> String {
    use leveller_listen::Answer;
    match key {
        TextKey::Listener => model.listener().to_string(),
        TextKey::Clip { label, question } => {
            match model.trial().and_then(|s| s.clip_answer(label, question)) {
                Some(Answer::Text(text)) => text.clone(),
                _ => String::new(),
            }
        }
        TextKey::Trial { question } => {
            match model.trial().and_then(|s| s.trial_answer(question)) {
                Some(Answer::Text(text)) => text.clone(),
                _ => String::new(),
            }
        }
        TextKey::Note => model
            .annotate()
            .and_then(|s| s.selected_annotation())
            .and_then(|a| a.note.clone())
            .unwrap_or_default(),
    }
}

impl Layout {
    /// What is under a point, or nothing.
    pub fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        self.elements
            .iter()
            .find(|e| e.rect.contains(x, y))
            .map(|e| e.hit)
    }

    pub fn trial(&self) -> Option<&TrialPage> {
        match &self.page {
            Page::Trial(page) => Some(page),
            _ => None,
        }
    }

    pub fn annotate(&self) -> Option<&AnnotatePage> {
        match &self.page {
            Page::Annotate(page) => Some(page),
            _ => None,
        }
    }
}

