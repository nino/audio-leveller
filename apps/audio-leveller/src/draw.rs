//! Painting the window from the model and the layout.
//!
//! One function, `scene`, which draws the whole thing. It takes a graphics
//! context and nothing else from AppKit, so the live window and the offscreen
//! renderer run exactly the same code — which is what makes it possible to look
//! at the app without running it.

use aqua::chrome::{self, ButtonState, LightState, Panel};
use aqua::paint;
use aqua::palette::{self, Colour, Focus, Gel};
use aqua::text::{self, Align, Style};
use leveller_stages::params::ParamSpec;
use leveller_ui::{Model, Status};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::CGContext;

use crate::layout::{Hit, Layout, Rect};

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

fn cg(rect: Rect) -> CGRect {
    paint::rect(rect.x, rect.y, rect.width, rect.height)
}

/// Draw the whole window.
pub fn scene(ctx: &CGContext, model: &Model, layout: &Layout, pointer: Pointer, bounds: CGRect) {
    let focus = Focus::of(model.window_active());

    chrome::metal(ctx, bounds, focus);
    dropzone(ctx, model, layout, focus);
    if let Some(rect) = layout.status {
        status(ctx, model, rect, focus);
    }
    presets(ctx, model, layout, pointer, focus);
    chain(ctx, model, layout, pointer, focus);

    // Last, so it sits over anything scrolled up under it.
    chrome::titlebar(ctx, cg(layout.titlebar), "Audio Leveller", focus);
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

fn dropzone(ctx: &CGContext, model: &Model, layout: &Layout, focus: Focus) {
    let rect = layout.dropzone;
    chrome::dropzone(ctx, cg(rect), model.drag_over(), focus);

    let centred = |y: f64, height: f64| paint::rect(rect.x, rect.y + y, rect.width, height);
    text::draw(
        "🎚️",
        centred(18.0, 40.0),
        &Style::default().size(34.0).align(Align::Centre),
    );
    text::draw(
        "Audio Leveller",
        centred(62.0, 26.0),
        &Style::default()
            .size(20.0)
            .bold()
            .align(Align::Centre)
            .embossed(),
    );
    text::draw(
        "Drop a .wav file here",
        centred(90.0, 20.0),
        &Style::default().size(14.0).align(Align::Centre).embossed(),
    );
    text::draw(
        "Each speech segment is normalised to its target loudness",
        centred(112.0, 16.0),
        &Style::default()
            .size(11.0)
            .colour(palette::hint_text())
            .align(Align::Centre)
            .embossed(),
    );
}

fn status(ctx: &CGContext, model: &Model, rect: Rect, focus: Focus) {
    chrome::panel(ctx, cg(rect), Panel::Glass, focus);
    let x = rect.x + 14.0;
    let width = rect.width - 28.0;
    let line = |y: f64| paint::rect(x, rect.y + y, width, 17.0);

    match model.status() {
        Status::Idle => {}

        Status::Working {
            path,
            stage,
            index,
            total,
            overall,
        } => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            text::draw(
                &format!("Processing {name}"),
                line(10.0),
                &Style::default().size(13.0).bold(),
            );
            let of = if *total > 0 {
                format!("  ({} of {total})", index + 1)
            } else {
                String::new()
            };
            text::draw(
                &format!("{stage}{of}"),
                line(28.0),
                &Style::default().size(11.0).colour(palette::hint_text()),
            );
            chrome::progress(
                ctx,
                paint::rect(x, rect.y + 46.0, width, 10.0),
                *overall,
                focus,
            );
        }

        Status::Failed(message) => {
            text::draw(
                "Could not process that file",
                line(10.0),
                &Style::default()
                    .size(13.0)
                    .bold()
                    // Red stays red on an inactive window: an error is content,
                    // not chrome.
                    .colour(Colour::rgb(0xa3, 0x16, 0x0f)),
            );
            text::draw(
                message,
                line(30.0),
                &Style::default().size(11.0).colour(palette::hint_text()),
            );
        }

        Status::Done(summary) => {
            let name = summary
                .output_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            text::draw(
                &format!("Wrote {name}"),
                line(10.0),
                &Style::default().size(13.0).bold(),
            );
            text::draw(
                &format!(
                    "{:.1} LUFS in, {:.1} out · peak {:.1} dBFS · {} segments, {} pauses · bed {:.0}s",
                    summary.input_lufs,
                    summary.output_lufs,
                    summary.output_peak_dbfs,
                    summary.segments,
                    summary.silences,
                    summary.room_tone_sec
                ),
                line(28.0),
                &Style::default().size(11.0).colour(palette::hint_text()),
            );

            // One line per stage, which is the whole point of the panel: what
            // each of them decided, and how long it took.
            for (i, stage) in summary.stages.iter().enumerate() {
                let y = 50.0 + i as f64 * 17.0;
                text::draw(
                    &stage.name,
                    paint::rect(x, rect.y + y, 76.0, 17.0),
                    &Style::default().size(11.0).bold().colour(if stage.enabled {
                        palette::body_text()
                    } else {
                        palette::hint_text()
                    }),
                );
                text::draw(
                    &stage.decision,
                    paint::rect(x + 80.0, rect.y + y, width - 150.0, 17.0),
                    &Style::default().size(11.0).colour(palette::hint_text()),
                );
                if stage.enabled {
                    text::draw(
                        &format!("{:.0} ms", stage.elapsed_ms),
                        paint::rect(x + width - 66.0, rect.y + y, 66.0, 17.0),
                        &Style::default()
                            .size(11.0)
                            .monospaced()
                            .colour(palette::hint_text())
                            .align(Align::Right),
                    );
                }
            }
        }
    }
}

fn presets(ctx: &CGContext, model: &Model, layout: &Layout, pointer: Pointer, focus: Focus) {
    chrome::panel(ctx, cg(layout.preset_panel), Panel::Inset, focus);

    text::draw(
        "Preset",
        cg(layout.preset_label),
        &Style::default().size(14.0).bold().embossed(),
    );

    let names = model.preset_names();
    let selected = names.iter().position(|n| *n == model.preset()).unwrap_or(0);
    chrome::segmented(ctx, cg(layout.preset_segments), &names, selected, focus);

    chrome::gel_button(
        ctx,
        cg(layout.revert),
        "Revert",
        Gel::Grey,
        pointer.state(Hit::Revert, model.is_edited()),
        focus,
    );
    chrome::gel_button(
        ctx,
        cg(layout.rerender),
        "Re-render",
        // The default button, once there is something to re-render.
        if model.can_rerender() {
            Gel::Blue
        } else {
            Gel::Grey
        },
        pointer.state(Hit::Rerender, model.can_rerender()),
        focus,
    );

    let note = if model.is_edited() {
        format!("{} — edited", model.preset_description())
    } else {
        model.preset_description().to_string()
    };
    text::draw(
        &note,
        cg(layout.preset_note),
        &Style::default()
            .size(11.0)
            .colour(palette::hint_text())
            .embossed(),
    );
}

fn chain(ctx: &CGContext, model: &Model, layout: &Layout, pointer: Pointer, focus: Focus) {
    chrome::panel(ctx, cg(layout.chain_panel), Panel::Inset, focus);
    text::draw(
        "Pipeline",
        cg(layout.chain_heading),
        &Style::default().size(14.0).bold().embossed(),
    );

    for row in &layout.rows {
        let stage = &model.stages()[row.index];

        if row.index > 0 {
            chrome::divider(ctx, row.rect.x, row.rect.y, row.rect.width, focus);
        }

        // The disclosure triangle, pointing right when closed and down when
        // open — which is the only affordance saying a row has more in it.
        text::draw(
            if stage.expanded { "▼" } else { "▶" },
            cg(row.disclosure),
            &Style::default()
                .size(9.0)
                .colour(palette::hint_text())
                .align(Align::Centre),
        );

        chrome::checkbox(ctx, cg(row.checkbox), stage.enabled, focus);

        // A stage that is off keeps its name readable but goes grey, so the
        // chain still reads top to bottom.
        let dimmed = if stage.enabled {
            palette::body_text()
        } else {
            palette::hint_text()
        };
        text::draw(
            stage.label,
            cg(row.label),
            &Style::default().size(13.0).bold().colour(dimmed).embossed(),
        );
        text::draw(
            &stage.description,
            cg(row.description),
            &Style::default()
                .size(11.0)
                .colour(palette::hint_text())
                .align(Align::Right)
                .embossed(),
        );

        for param in &row.params {
            let Some(spec) = stage.params.get(param.index) else {
                continue;
            };
            parameter(
                ctx, model, stage.name, spec, param, pointer, row.index, focus,
            );
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one row, drawn once, from what it needs"
)]
fn parameter(
    ctx: &CGContext,
    model: &Model,
    stage: &str,
    spec: &ParamSpec,
    row: &crate::layout::ParamRow,
    pointer: Pointer,
    stage_index: usize,
    focus: Focus,
) {
    let (key, label) = match spec {
        ParamSpec::Number { key, label, .. }
        | ParamSpec::Boolean { key, label, .. }
        | ParamSpec::Choice { key, label, .. } => (*key, *label),
    };
    let edited = model.is_param_edited(stage, key);

    text::draw(
        label,
        cg(row.label),
        &Style::default()
            .size(11.0)
            .colour(if edited {
                // The amber the stylesheet uses for a moved parameter.
                Colour::rgb(0x8a, 0x5a, 0x00)
            } else {
                palette::hint_text()
            })
            .embossed(),
    );

    let value = model.param(stage, key);
    match spec {
        ParamSpec::Number { min, max, unit, .. } => {
            let current = value.and_then(serde_json::Value::as_f64).unwrap_or(*min);
            let fraction = if max > min {
                ((current - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            slider(
                ctx,
                cg(row.control),
                fraction,
                pointer.pressed == Some(Hit::Slider(stage_index, row.index)),
                focus,
            );
            let shown = if unit.is_empty() {
                format!("{current:.4}")
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string()
            } else {
                format!("{current:.2} {unit}")
            };
            text::draw(
                &shown,
                cg(row.value),
                &Style::default()
                    .size(11.0)
                    .monospaced()
                    .align(Align::Right)
                    .embossed(),
            );
        }

        ParamSpec::Boolean { .. } => {
            let on = value.and_then(serde_json::Value::as_bool).unwrap_or(false);
            chrome::checkbox(
                ctx,
                paint::rect(row.control.x, row.control.y - 2.0, 14.0, 14.0),
                on,
                focus,
            );
        }

        ParamSpec::Choice { options, .. } => {
            let current = value.and_then(|v| v.as_str()).unwrap_or("");
            let titles: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
            let selected = options.iter().position(|(v, _)| *v == current).unwrap_or(0);
            chrome::segmented(
                ctx,
                paint::rect(row.control.x, row.control.y - 7.0, 130.0, 20.0),
                &titles,
                selected,
                focus,
            );
        }
    }
}

/// A slider: a sunken track with a gel knob on it.
fn slider(ctx: &CGContext, rect: CGRect, fraction: f64, dragging: bool, focus: Focus) {
    let track = paint::rect(
        rect.origin.x,
        rect.origin.y + rect.size.height / 2.0 - 3.0,
        rect.size.width,
        6.0,
    );
    let path = aqua::paint::Shape::Capsule.path(track);
    paint::fill_gradient(
        ctx,
        &path,
        track,
        &[
            (0.0, Colour::rgb(0xc9, 0xc9, 0xc9)),
            (1.0, Colour::rgb(0xea, 0xea, 0xea)),
        ],
    );
    paint::inner_shadow(ctx, &path, (0.0, 1.0), 2.0, Colour::black(0.35));
    paint::stroke_inside(
        ctx,
        &path,
        1.0,
        focus.pick(Colour::rgb(0x8b, 0x8b, 0x8b), Colour::rgb(0xa4, 0xa4, 0xa4)),
    );

    let knob_size = 14.0;
    let travel = (rect.size.width - knob_size).max(0.0);
    let knob = paint::rect(
        rect.origin.x + travel * fraction,
        rect.origin.y + rect.size.height / 2.0 - knob_size / 2.0,
        knob_size,
        knob_size,
    );
    let knob_path = aqua::paint::Shape::Circle.path(knob);

    paint::drop_shadow(ctx, &knob_path, (0.0, 1.0), 2.0, Colour::black(0.4));
    if focus.is_active() {
        let stops = if dragging {
            [
                Colour::rgb(0xf2, 0xf7, 0xff),
                Colour::rgb(0xa9, 0xcb, 0xf7),
                Colour::rgb(0x6a, 0xa2, 0xf5),
                Colour::rgb(0x3d, 0x82, 0xea),
            ]
        } else {
            [
                Colour::white(1.0),
                Colour::rgb(0xd3, 0xe4, 0xfb),
                Colour::rgb(0x8f, 0xb6, 0xee),
                Colour::rgb(0x5f, 0x92, 0xe2),
            ]
        };
        paint::fill_radial(
            ctx,
            &knob_path,
            knob,
            (0.5, 0.28),
            &[
                (0.0, stops[0]),
                (0.4, stops[1]),
                (0.7, stops[2]),
                (1.0, stops[3]),
            ],
        );
    } else {
        paint::fill_gradient(
            ctx,
            &knob_path,
            knob,
            &[
                (0.0, Colour::rgb(0xf4, 0xf4, 0xf4)),
                (1.0, Colour::rgb(0xda, 0xda, 0xda)),
            ],
        );
    }
    paint::inner_highlight_top(ctx, &knob_path, Colour::white(0.9));
    paint::stroke_inside(
        ctx,
        &knob_path,
        1.0,
        focus.pick(Colour::rgb(0x5c, 0x6b, 0x86), Colour::rgb(0x9b, 0x9b, 0x9b)),
    );
}
