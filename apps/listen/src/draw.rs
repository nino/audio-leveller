//! Painting the window from the model and the layout.
//!
//! One entry point, `scene`, taking a graphics context and nothing else from
//! AppKit — so the live window and the offscreen renderer run the same code and
//! the app can be looked at without being run.

use aqua::chrome::{self, ButtonState, LightState, Panel};
use aqua::paint;
use aqua::palette::{self, Colour, Focus, Gel};
use aqua::text::{self, Align, Style};
use leveller_listen::{Answer, ClipQuestion, Confidence, Peaks, TrialQuestion};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::CGContext;

use crate::layout::{AnnotatePage, Hit, HomePage, Layout, Page, Rect, ResultsPage, TrialPage};
use crate::model::Model;

/// What the pointer is doing, which the model does not need to know about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    pub hovered: Option<Hit>,
    pub pressed: Option<Hit>,
}

impl Pointer {
    fn state(&self, hit: Hit, enabled: bool) -> ButtonState {
        ButtonState {
            hovered: self.hovered == Some(hit),
            pressed: self.pressed == Some(hit),
            enabled,
        }
    }

    fn over_lights(&self) -> bool {
        matches!(self.hovered, Some(Hit::Light(_)))
    }
}

/// Where the player is, which lives in the audio thread rather than the model.
#[derive(Clone, Copy, Default)]
pub struct Transport<'a> {
    pub playing: bool,
    /// Seconds from the start of the clip.
    pub position: f64,
    pub duration: f64,
    /// The waveform of whichever clip is selected.
    pub peaks: Option<&'a Peaks>,
    /// A region being dragged out on the annotation waveform, in seconds.
    pub dragging: Option<(f64, f64)>,
    /// Draw what the text fields hold, rather than leaving their wells empty.
    ///
    /// The live window puts a real `NSTextField` over each well and this stays
    /// off; the offscreen renderer has no views, so it draws them itself.
    pub fields: bool,
}

fn cg(rect: Rect) -> CGRect {
    paint::rect(rect.x, rect.y, rect.width, rect.height)
}

fn hint() -> Style {
    Style::default().size(11.0).colour(palette::hint_text())
}

fn body() -> Style {
    Style::default().size(12.0)
}

fn heading() -> Style {
    Style::default().size(16.0).bold().embossed()
}

/// Draw the whole window.
pub fn scene(
    ctx: &CGContext,
    model: &Model,
    layout: &Layout,
    pointer: Pointer,
    transport: Transport,
    bounds: CGRect,
) {
    let focus = Focus::of(model.window_active());
    chrome::metal(ctx, bounds, focus);

    if let (Some(rect), Some(message)) = (layout.error, model.error()) {
        error(ctx, rect, message, focus);
    }

    match &layout.page {
        Page::Home(page) => home(ctx, model, page, pointer, focus),
        Page::Trial(page) => trial(ctx, model, page, pointer, transport, focus),
        Page::Results(page) => results(ctx, model, page, pointer, focus),
        Page::Annotate(page) => annotate(ctx, model, page, pointer, transport, focus),
    }

    if transport.fields {
        for field in &layout.fields {
            let value = crate::layout::value_of(model, &field.key);
            let (text, style) = if value.is_empty() {
                (field.placeholder.to_string(), hint())
            } else {
                (value, body())
            };
            text::draw(&text, cg(field.rect.inset(6.0, 4.0)), &style);
        }
    }

    // Last, so it sits over anything scrolled up under it.
    chrome::titlebar(ctx, cg(layout.titlebar), "Listen", focus);
    chrome::traffic_lights(
        ctx,
        layout.titlebar.height,
        LightState {
            hovered: pointer.over_lights(),
            pressed: matches!(pointer.pressed, Some(Hit::Light(_))),
        },
        focus,
    );
}

fn error(ctx: &CGContext, rect: Rect, message: &str, focus: Focus) {
    chrome::panel(ctx, cg(rect), Panel::Glass, focus);
    text::draw(
        message,
        cg(rect.inset(12.0, 10.0)),
        // Red stays red on an inactive window: an error is content, not chrome.
        &body().colour(Colour::rgb(0xa3, 0x16, 0x0f)),
    );
}

// ------------------------------------------------------------------- home --

fn home(ctx: &CGContext, model: &Model, page: &HomePage, pointer: Pointer, focus: Focus) {
    text::draw("Listening tests", cg(page.heading), &heading().size(20.0));
    text::draw("Listener", cg(page.listener_label), &body().embossed());
    // The field itself is a real NSTextField the shell puts on top; all that
    // belongs here is the well it sits in.
    chrome::panel(ctx, cg(page.listener_field), Panel::Inset, focus);

    text::draw(
        "Sessions",
        cg(page.sessions_heading),
        &heading().size(13.0),
    );
    chrome::panel(ctx, cg(page.sessions_panel), Panel::Inset, focus);
    if page.sessions.is_empty() {
        text::draw(
            "Nothing in listening/sessions yet — make one with the command line.",
            cg(page.sessions_panel.inset(14.0, 18.0)),
            &hint(),
        );
    }
    for row in &page.sessions {
        if row.index > 0 {
            chrome::divider(ctx, row.rect.x, row.rect.y, row.rect.width, focus);
        }
        text::draw(
            &model.sessions()[row.index],
            cg(row.name),
            &body().size(13.0),
        );
        chrome::gel_button(
            ctx,
            cg(row.open),
            "Listen",
            Gel::Blue,
            pointer.state(Hit::OpenSession(row.index), true),
            focus,
        );
        chrome::gel_button(
            ctx,
            cg(row.results),
            "Results",
            Gel::Grey,
            pointer.state(Hit::OpenResults(row.index), true),
            focus,
        );
    }

    text::draw("Annotate", cg(page.files_heading), &heading().size(13.0));
    chrome::panel(ctx, cg(page.files_panel), Panel::Inset, focus);
    if page.files.is_empty() {
        text::draw(
            "Put a .wav in listening/annotate to mark up breaths and clicks.",
            cg(page.files_panel.inset(14.0, 18.0)),
            &hint(),
        );
    }
    for row in &page.files {
        if row.index > 0 {
            chrome::divider(ctx, row.rect.x, row.rect.y, row.rect.width, focus);
        }
        text::draw(
            &model.annotatable()[row.index],
            cg(row.name),
            &body().size(13.0).monospaced(),
        );
        chrome::gel_button(
            ctx,
            cg(row.open),
            "Annotate",
            Gel::Grey,
            pointer.state(Hit::OpenAnnotate(row.index), true),
            focus,
        );
    }
}

// ------------------------------------------------------------------ trial --

fn trial(
    ctx: &CGContext,
    model: &Model,
    page: &TrialPage,
    pointer: Pointer,
    transport: Transport,
    focus: Focus,
) {
    let Some(state) = model.trial() else { return };
    chrome::gel_button(
        ctx,
        cg(page.back),
        "Sessions",
        Gel::Grey,
        pointer.state(Hit::Home, true),
        focus,
    );

    let title = state.trial().map_or("", |t| t.title.as_str());
    text::draw(title, cg(page.heading), &heading().size(18.0));
    text::draw(
        &format!(
            "{} · trial {} of {}",
            state.session.name,
            state.index + 1,
            state.session.trials.len()
        ),
        cg(page.subheading),
        &hint(),
    );

    // The clip buttons. The selected one wears the blue gel, which is the only
    // thing on the page saying what is playing.
    for (i, rect) in page.clips.iter().enumerate() {
        let label = state
            .trial()
            .and_then(|t| t.clips.get(i))
            .map_or_else(|| format!("{i}"), |c| c.label.clone());
        chrome::gel_button(
            ctx,
            cg(*rect),
            &label,
            if i == state.clip { Gel::Blue } else { Gel::Grey },
            pointer.state(Hit::Clip(i), true),
            focus,
        );
    }
    chrome::gel_button(
        ctx,
        cg(page.play),
        if transport.playing { "Pause" } else { "Play" },
        Gel::Grey,
        pointer.state(Hit::PlayPause, true),
        focus,
    );

    waveform(ctx, page.waveform, transport, focus);
    text::draw(
        &format!(
            "{} / {}   ·   the clips are the same passage, and switching keeps its place",
            clock(transport.position),
            clock(transport.duration)
        ),
        cg(page.time),
        &hint(),
    );

    // Questions about the clip that is playing.
    chrome::panel(ctx, cg(page.clip_panel), Panel::Inset, focus);
    let label = state
        .trial()
        .and_then(|t| t.clips.get(state.clip))
        .map_or("", |c| c.label.as_str());
    text::draw(
        &format!("About clip {label}"),
        cg(page.clip_heading),
        &heading().size(13.0),
    );
    for row in &page.clip_questions {
        let Some(question) = state.clip_questions().get(row.index) else {
            continue;
        };
        let answer = state.clip_answer(label, question.id());
        match question {
            ClipQuestion::Scale {
                label: name,
                min,
                min_label,
                max_label,
                ..
            } => {
                text::draw(name, cg(row.label), &body().embossed());
                let chosen = match answer {
                    Some(Answer::Number(n)) => Some((n - min).round() as usize),
                    _ => None,
                };
                for (i, rect) in row.choices.iter().enumerate() {
                    let value = min + i as f64;
                    chrome::gel_button(
                        ctx,
                        cg(*rect),
                        &format!("{value:.0}"),
                        if chosen == Some(i) { Gel::Blue } else { Gel::Grey },
                        pointer.state(Hit::ClipScale(row.index, i), true),
                        focus,
                    );
                }
                // The ends of the scale, said in words, under the first and
                // last button: a number from 0 to 5 means nothing on its own.
                if let (Some(first), Some(last)) = (row.choices.first(), row.choices.last()) {
                    if let Some(text) = min_label {
                        text::draw(
                            text,
                            paint::rect(first.x, first.bottom(), 80.0, 12.0),
                            &hint().size(9.0),
                        );
                    }
                    if let Some(text) = max_label {
                        text::draw(
                            text,
                            paint::rect(last.right() - 80.0, last.bottom(), 80.0, 12.0),
                            &hint().size(9.0).align(Align::Right),
                        );
                    }
                }
            }

            ClipQuestion::Tags {
                label: name,
                options,
                ..
            } => {
                text::draw(name, cg(row.label), &body().embossed());
                let chosen: &[String] = match answer {
                    Some(Answer::Tags(tags)) => tags,
                    _ => &[],
                };
                for (i, rect) in row.choices.iter().enumerate() {
                    let Some(option) = options.get(i) else { continue };
                    chrome::gel_button(
                        ctx,
                        cg(*rect),
                        option,
                        if chosen.contains(option) {
                            Gel::Blue
                        } else {
                            Gel::Grey
                        },
                        pointer.state(Hit::ClipTag(row.index, i), true),
                        focus,
                    );
                }
            }

            ClipQuestion::Text { label: name, .. } => {
                text::draw(name, cg(row.label), &body().embossed());
                chrome::panel(
                    ctx,
                    paint::rect(
                        row.label.right() + 8.0,
                        row.label.y,
                        row.rect.right() - row.label.right() - 8.0,
                        row.label.height,
                    ),
                    Panel::Inset,
                    focus,
                );
            }
        }
    }

    // Questions about the set as a whole.
    chrome::panel(ctx, cg(page.trial_panel), Panel::Inset, focus);
    for row in &page.trial_questions {
        let Some(question) = state.trial_questions().get(row.index) else {
            continue;
        };
        let answer = state.trial_answer(question.id());
        match question {
            TrialQuestion::Pick { label: name, .. } => {
                text::draw(name, cg(row.label), &body().embossed());
                let chosen = match answer {
                    Some(Answer::Text(text)) => Some(text.as_str()),
                    _ => None,
                };
                for (i, rect) in row.choices.iter().enumerate() {
                    let clip = state
                        .trial()
                        .and_then(|t| t.clips.get(i))
                        .map_or_else(|| i.to_string(), |c| c.label.clone());
                    chrome::gel_button(
                        ctx,
                        cg(*rect),
                        &clip,
                        if chosen == Some(clip.as_str()) {
                            Gel::Blue
                        } else {
                            Gel::Grey
                        },
                        pointer.state(Hit::TrialPick(row.index, i), true),
                        focus,
                    );
                }
            }
            TrialQuestion::Text { label: name, .. } => {
                text::draw(name, cg(row.label), &body().embossed());
                chrome::panel(
                    ctx,
                    paint::rect(
                        row.label.right() + 8.0,
                        row.label.y,
                        row.rect.right() - row.label.right() - 8.0,
                        row.label.height,
                    ),
                    Panel::Inset,
                    focus,
                );
            }
        }
    }

    chrome::gel_button(
        ctx,
        cg(page.previous),
        "Previous",
        Gel::Grey,
        pointer.state(Hit::PreviousTrial, state.index > 0),
        focus,
    );
    chrome::gel_button(
        ctx,
        cg(page.next),
        if state.index + 1 >= state.session.trials.len() {
            "Finish"
        } else {
            "Next"
        },
        Gel::Blue,
        pointer.state(Hit::NextTrial, true),
        focus,
    );
    text::draw(
        &format!("{} answers on this trial", state.answered()),
        cg(page.progress),
        &hint().align(Align::Centre),
    );
}

/// The waveform well, with whatever is loaded drawn into it.
fn waveform(ctx: &CGContext, rect: Rect, transport: Transport, focus: Focus) {
    chrome::panel(ctx, cg(rect), Panel::Inset, focus);
    let inner = rect.inset(3.0, 3.0);

    if let Some(peaks) = transport.peaks {
        let width = inner.width.max(1.0) as usize;
        let spp = (peaks.len() as f64 / width as f64).max(1e-9);
        let columns = peaks.columns(0.0, spp, width);
        let mid = inner.y + inner.height / 2.0;
        let half = inner.height / 2.0 - 2.0;
        let ink = focus.pick(
            Colour::rgb(0x2b, 0x4e, 0x8a),
            Colour::rgb(0x86, 0x8e, 0x9c),
        );
        for (x, column) in columns.iter().enumerate() {
            let top = mid - f64::from(column.max) * half;
            let bottom = mid - f64::from(column.min) * half;
            paint::fill_rect(
                ctx,
                paint::rect(inner.x + x as f64, top, 1.0, (bottom - top).max(1.0)),
                ink,
            );
        }
    } else {
        text::draw(
            "no clip loaded",
            cg(inner.inset(0.0, inner.height / 2.0 - 8.0)),
            &hint().align(Align::Centre),
        );
    }

    // The playhead.
    if transport.duration > 0.0 {
        let x = inner.x + (transport.position / transport.duration).clamp(0.0, 1.0) * inner.width;
        paint::fill_rect(
            ctx,
            paint::rect(x - 0.5, inner.y, 1.0, inner.height),
            Colour::rgb(0xc0, 0x20, 0x20),
        );
    }
}

fn clock(secs: f64) -> String {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    format!("{}:{:04.1}", (secs / 60.0).floor() as u64, secs % 60.0)
}

// ---------------------------------------------------------------- results --

fn results(ctx: &CGContext, model: &Model, page: &ResultsPage, pointer: Pointer, focus: Focus) {
    let Some(state) = model.results() else { return };
    chrome::gel_button(
        ctx,
        cg(page.back),
        "Sessions",
        Gel::Grey,
        pointer.state(Hit::Home, true),
        focus,
    );
    text::draw(
        &format!("{} · {} listeners", state.session.name, state.results.len()),
        cg(page.heading),
        &heading().size(18.0),
    );

    chrome::panel(ctx, cg(page.panel), Panel::Inset, focus);

    // A column head per clip label, then the mean of the first scale question
    // per trial — enough to see a difference at a glance, with the numbers
    // themselves in the JSON for anything more careful.
    let scale = state.session.clip_questions.iter().find_map(|q| match q {
        ClipQuestion::Scale { id, label, .. } => Some((id.clone(), label.clone())),
        _ => None,
    });
    if let Some((rect, cells)) = page.rows.first() {
        let head = paint::rect(rect.x, rect.y - 24.0, rect.width, 18.0);
        if state.results.is_empty() {
            // The heads would promise numbers that are not there, so the row
            // they would have used says so instead.
            text::draw("Nobody has scored this session yet.", head, &hint());
        } else {
            if let Some((_, name)) = scale.as_ref() {
                text::draw(&format!("mean {}", name.to_lowercase()), head, &hint());
            }
            for (i, cell) in cells.iter().enumerate() {
                let label = state
                    .session
                    .trials
                    .first()
                    .and_then(|t| t.clips.get(i))
                    .map_or_else(|| i.to_string(), |c| c.label.clone());
                text::draw(
                    &label,
                    paint::rect(cell.x, cell.y - 24.0, cell.width, 18.0),
                    &hint().bold().align(Align::Centre),
                );
            }
        }
    }

    for ((rect, cells), trial) in page.rows.iter().zip(&state.session.trials) {
        text::draw(&trial.title, cg(*rect), &body());
        let Some((question, _)) = scale.as_ref() else {
            continue;
        };
        for (i, cell) in cells.iter().enumerate() {
            let Some(clip) = trial.clips.get(i) else {
                continue;
            };
            let scores: Vec<f64> = state
                .results
                .iter()
                .filter_map(|r| r.trials.get(&trial.id))
                .filter_map(|t| t.clips.get(&clip.label))
                .filter_map(|answers| match answers.get(question) {
                    Some(Answer::Number(n)) => Some(*n),
                    _ => None,
                })
                .collect();
            let text = if scores.is_empty() {
                "—".to_string()
            } else {
                format!("{:.2}", scores.iter().sum::<f64>() / scores.len() as f64)
            };
            text::draw(
                &text,
                cg(*cell),
                &body().monospaced().align(Align::Centre),
            );
        }
    }

    chrome::gel_button(
        ctx,
        cg(page.reveal),
        if state.key.is_some() {
            "Revealed"
        } else {
            "Reveal the key"
        },
        Gel::Grey,
        pointer.state(Hit::Reveal, state.key.is_none()),
        focus,
    );

    if let (Some(rect), Some(key)) = (page.key_panel, state.key.as_ref()) {
        chrome::panel(ctx, cg(rect), Panel::Glass, focus);
        text::draw(
            "Which clip was which",
            paint::rect(rect.x + 14.0, rect.y + 12.0, rect.width - 28.0, 18.0),
            &heading().size(13.0),
        );
        for (i, (name, variant)) in key.variants.iter().enumerate() {
            let clips: Vec<String> = key
                .clips
                .iter()
                .filter_map(|(trial, labels)| {
                    labels
                        .iter()
                        .find(|(_, v)| *v == name)
                        .map(|(label, _)| format!("{trial}:{label}"))
                })
                .collect();
            text::draw(
                &format!("{name} — {} · {}", variant.file, clips.join(" ")),
                paint::rect(
                    rect.x + 14.0,
                    rect.y + 38.0 + i as f64 * 20.0,
                    rect.width - 28.0,
                    18.0,
                ),
                &body().size(11.0),
            );
        }
    }
}

// --------------------------------------------------------------- annotate --

fn annotate(
    ctx: &CGContext,
    model: &Model,
    page: &AnnotatePage,
    pointer: Pointer,
    transport: Transport,
    focus: Focus,
) {
    let Some(state) = model.annotate() else { return };
    chrome::gel_button(
        ctx,
        cg(page.back),
        "Sessions",
        Gel::Grey,
        pointer.state(Hit::Home, true),
        focus,
    );
    text::draw(&state.file.file, cg(page.heading), &heading().size(18.0));
    chrome::gel_button(
        ctx,
        cg(page.zoom_out),
        "−",
        Gel::Grey,
        pointer.state(Hit::ZoomOut, true),
        focus,
    );
    chrome::gel_button(
        ctx,
        cg(page.zoom_in),
        "+",
        Gel::Grey,
        pointer.state(Hit::ZoomIn, true),
        focus,
    );

    let rect = page.waveform;
    chrome::panel(ctx, cg(rect), Panel::Inset, focus);
    let inner = rect.inset(3.0, 3.0);
    let secs = state.view_secs.max(1e-9);
    let x_of = |t: f64| inner.x + ((t - state.view_start) / secs).clamp(0.0, 1.0) * inner.width;

    // The regions go under the waveform, so the trace stays readable through
    // them — the tinting is the label, not a highlight over the audio.
    for (i, region) in page.regions.iter().enumerate() {
        let annotation = state.visible().get(i).copied();
        let selected = annotation.map(|a| a.id.as_str()) == state.selected.as_deref();
        let tint = label_colour(annotation.map_or("other", |a| a.label.as_str()));
        paint::fill_rect(
            ctx,
            cg(*region),
            tint.with_alpha(if selected { 0.42 } else { 0.22 }),
        );
        if selected {
            paint::fill_rect(ctx, paint::rect(region.x, region.y, 1.0, region.height), tint);
            paint::fill_rect(
                ctx,
                paint::rect(region.right() - 1.0, region.y, 1.0, region.height),
                tint,
            );
        }
        if let Some(annotation) = annotation
            && region.width > 26.0
        {
            text::draw(
                &annotation.label,
                paint::rect(region.x + 3.0, region.y + 3.0, region.width - 6.0, 12.0),
                &hint().size(9.0),
            );
        }
    }

    // A region being dragged out right now, before it exists.
    if let Some((from, to)) = transport.dragging {
        let (x0, x1) = (x_of(from.min(to)), x_of(from.max(to)));
        paint::fill_rect(
            ctx,
            paint::rect(x0, inner.y, (x1 - x0).max(1.0), inner.height),
            Colour::rgba(0x3d, 0x82, 0xea, 0.3),
        );
    }

    let width = inner.width.max(1.0) as usize;
    let rate = f64::from(state.peaks.sample_rate().max(1));
    let spp = (secs * rate / width as f64).max(1e-9);
    let columns = state.peaks.columns(state.view_start * rate, spp, width);
    let mid = inner.y + inner.height / 2.0;
    let half = inner.height / 2.0 - 2.0;
    let ink = focus.pick(
        Colour::rgb(0x2b, 0x4e, 0x8a),
        Colour::rgb(0x86, 0x8e, 0x9c),
    );
    for (x, column) in columns.iter().enumerate() {
        let top = mid - f64::from(column.max) * half;
        let bottom = mid - f64::from(column.min) * half;
        paint::fill_rect(
            ctx,
            paint::rect(inner.x + x as f64, top, 1.0, (bottom - top).max(1.0)),
            ink,
        );
    }
    if transport.playing || transport.position > 0.0 {
        let x = x_of(transport.position);
        paint::fill_rect(
            ctx,
            paint::rect(x - 0.5, inner.y, 1.0, inner.height),
            Colour::rgb(0xc0, 0x20, 0x20),
        );
    }

    // The ruler: where in the file this is, so zooming does not lose the place.
    text::draw(
        &format!("{} — {}", clock(state.view_start), clock(state.view_start + secs)),
        cg(page.ruler),
        &hint(),
    );
    text::draw(
        &format!("of {}", clock(state.file.duration)),
        cg(page.ruler),
        &hint().align(Align::Right),
    );

    for (i, rect) in page.labels.iter().enumerate() {
        let Some(name) = state.file.labels.get(i) else {
            continue;
        };
        // The first letter is the keyboard shortcut, which is why it is shown.
        let shortcut = name.chars().next().unwrap_or(' ').to_ascii_uppercase();
        chrome::gel_button(
            ctx,
            cg(*rect),
            &format!("{name}  {shortcut}"),
            if i == state.label { Gel::Blue } else { Gel::Grey },
            pointer.state(Hit::Label(i), true),
            focus,
        );
    }

    for (rect, which, name) in [
        (page.sure, Confidence::Sure, "Sure"),
        (page.maybe, Confidence::Maybe, "Maybe"),
    ] {
        chrome::gel_button(
            ctx,
            cg(rect),
            name,
            if state.confidence == which {
                Gel::Blue
            } else {
                Gel::Grey
            },
            pointer.state(Hit::Confidence(which), true),
            focus,
        );
    }
    chrome::gel_button(
        ctx,
        cg(page.delete),
        "Delete",
        Gel::Grey,
        pointer.state(Hit::DeleteRegion, state.selected.is_some()),
        focus,
    );

    text::draw("Note", cg(page.note_label), &body().embossed());
    chrome::panel(ctx, cg(page.note_field), Panel::Inset, focus);

    let total = state.file.annotations.len();
    let maybes = state
        .file
        .annotations
        .iter()
        .filter(|a| a.confidence == Confidence::Maybe)
        .count();
    text::draw(
        &format!(
            "{total} regions, {maybes} unsure · drag on the waveform to mark one · saved as you go"
        ),
        cg(page.counts),
        &hint(),
    );
}

/// A colour per label, so a page of regions reads at a glance.
fn label_colour(label: &str) -> Colour {
    match label {
        "breath" => Colour::rgb(0x3d, 0x82, 0xea),
        "click" => Colour::rgb(0xd0, 0x3a, 0x2a),
        "plosive" => Colour::rgb(0xe0, 0x9a, 0x18),
        "mouth-noise" => Colour::rgb(0x7a, 0x4c, 0xc8),
        _ => Colour::rgb(0x4a, 0x9a, 0x50),
    }
}
