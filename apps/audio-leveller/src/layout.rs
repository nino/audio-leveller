//! Where everything goes, computed from the model and the window's size.
//!
//! Separate from the drawing because it is also what hit-testing and
//! accessibility need: a click has to find the same rectangle the paint went
//! into, and VoiceOver has to be told where each element is. Three copies of
//! the arithmetic would be three chances to disagree.
//!
//! Platform-independent, and tested as such.

use leveller_ui::{Model, Status};

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
}

/// Everything that can be clicked, and what clicking it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Light(aqua::palette::Light),
    /// Anywhere else in the title strip, which drags the window.
    TitleBar,
    DropZone,
    Preset(usize),
    Revert,
    Rerender,
    StageCheckbox(usize),
    StageDisclosure(usize),
    /// A parameter's slider: which stage, and which of its parameters.
    Slider(usize, usize),
}

/// One placed thing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Element {
    pub rect: Rect,
    pub hit: Hit,
}

/// A stage's row, and the rows of its parameters when it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct StageRow {
    pub index: usize,
    pub rect: Rect,
    pub checkbox: Rect,
    pub disclosure: Rect,
    pub label: Rect,
    pub description: Rect,
    /// One per parameter, only when the stage is open.
    pub params: Vec<ParamRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamRow {
    pub index: usize,
    pub label: Rect,
    pub control: Rect,
    pub value: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub titlebar: Rect,
    pub dropzone: Rect,
    pub status: Option<Rect>,
    pub preset_panel: Rect,
    pub preset_label: Rect,
    pub preset_segments: Rect,
    pub revert: Rect,
    pub rerender: Rect,
    pub preset_note: Rect,
    pub chain_panel: Rect,
    pub chain_heading: Rect,
    pub rows: Vec<StageRow>,
    /// Everything clickable, in front-to-back order — so hit-testing takes the
    /// first match and a control on top of a panel wins.
    pub elements: Vec<Element>,
    /// How tall the content is, which is what the scroll view needs.
    pub content_height: f64,
}

pub const TITLEBAR: f64 = aqua::palette::TITLEBAR_HEIGHT;
const MARGIN: f64 = 20.0;
const PANEL_PADDING: f64 = 14.0;
const ROW_HEIGHT: f64 = 30.0;
const PARAM_HEIGHT: f64 = 26.0;
const DROPZONE_HEIGHT: f64 = 150.0;
const BUTTON_HEIGHT: f64 = 24.0;

/// Work out where everything goes for a window of this size.
pub fn compute(model: &Model, width: f64, height: f64) -> Layout {
    let mut elements = Vec::new();
    let inner = width - MARGIN * 2.0;

    // Pushed in back-to-front order throughout, and reversed at the end — so
    // the strip goes down before the lights that sit on it, and a click on a
    // light closes the window rather than starting a drag.
    let titlebar = Rect::new(0.0, 0.0, width, TITLEBAR);
    elements.push(Element {
        rect: titlebar,
        hit: Hit::TitleBar,
    });
    for (frame, light) in aqua::chrome::light_frames(TITLEBAR)
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
            hit: Hit::Light(light),
        });
    }

    let mut y = TITLEBAR + 16.0;

    let dropzone = Rect::new(MARGIN, y, inner, DROPZONE_HEIGHT);
    elements.push(Element {
        rect: dropzone,
        hit: Hit::DropZone,
    });
    y = dropzone.bottom() + 14.0;

    // The status panel is only there once something has happened.
    let status = match model.status() {
        Status::Idle => None,
        Status::Working { .. } => Some(Rect::new(MARGIN, y, inner, 62.0)),
        Status::Failed(_) => Some(Rect::new(MARGIN, y, inner, 54.0)),
        Status::Done(summary) => Some(Rect::new(
            MARGIN,
            y,
            inner,
            // Two lines of numbers, then one per stage.
            56.0 + summary.stages.len() as f64 * 17.0,
        )),
    };
    if let Some(status) = status {
        y = status.bottom() + 14.0;
    }

    // The preset row.
    let preset_panel = Rect::new(MARGIN, y, inner, 66.0);
    let row_y = y + PANEL_PADDING;
    let preset_label = Rect::new(MARGIN + PANEL_PADDING, row_y, 56.0, BUTTON_HEIGHT);
    let segment_width = 62.0 * model.preset_names().len() as f64;
    let preset_segments = Rect::new(
        preset_label.x + preset_label.width + 8.0,
        row_y,
        segment_width,
        BUTTON_HEIGHT,
    );
    let revert = Rect::new(
        preset_segments.x + preset_segments.width + 10.0,
        row_y,
        72.0,
        BUTTON_HEIGHT,
    );
    let rerender = Rect::new(
        MARGIN + inner - PANEL_PADDING - 96.0,
        row_y,
        96.0,
        BUTTON_HEIGHT,
    );
    let preset_note = Rect::new(
        MARGIN + PANEL_PADDING,
        row_y + BUTTON_HEIGHT + 6.0,
        inner - PANEL_PADDING * 2.0,
        16.0,
    );

    for (i, _) in model.preset_names().iter().enumerate() {
        let segment_width = preset_segments.width / model.preset_names().len() as f64;
        elements.push(Element {
            rect: Rect::new(
                preset_segments.x + i as f64 * segment_width,
                preset_segments.y,
                segment_width,
                preset_segments.height,
            ),
            hit: Hit::Preset(i),
        });
    }
    elements.push(Element {
        rect: revert,
        hit: Hit::Revert,
    });
    elements.push(Element {
        rect: rerender,
        hit: Hit::Rerender,
    });
    y = preset_panel.bottom() + 14.0;

    // The chain.
    let chain_top = y;
    let chain_heading = Rect::new(MARGIN + PANEL_PADDING, y + PANEL_PADDING, 200.0, 18.0);
    let mut row_y = chain_heading.bottom() + 8.0;

    let mut rows = Vec::new();
    for (index, stage) in model.stages().iter().enumerate() {
        let rect = Rect::new(
            MARGIN + PANEL_PADDING,
            row_y,
            inner - PANEL_PADDING * 2.0,
            ROW_HEIGHT,
        );
        let disclosure = Rect::new(rect.x, rect.y + 8.0, 14.0, 14.0);
        let checkbox = Rect::new(rect.x + 22.0, rect.y + 8.0, 14.0, 14.0);
        let label = Rect::new(rect.x + 44.0, rect.y, 130.0, ROW_HEIGHT);
        let description = Rect::new(
            label.x + label.width + 8.0,
            rect.y,
            rect.width - label.width - 52.0,
            ROW_HEIGHT,
        );

        elements.push(Element {
            rect: disclosure,
            hit: Hit::StageDisclosure(index),
        });
        elements.push(Element {
            rect: checkbox,
            hit: Hit::StageCheckbox(index),
        });

        let mut params = Vec::new();
        let mut param_y = rect.bottom();
        if stage.expanded {
            for (p, _) in stage.params.iter().enumerate() {
                let label = Rect::new(rect.x + 44.0, param_y, 150.0, PARAM_HEIGHT);
                let control = Rect::new(
                    label.x + label.width + 8.0,
                    param_y + 8.0,
                    rect.width - label.width - 150.0,
                    10.0,
                );
                let value = Rect::new(
                    control.x + control.width + 10.0,
                    param_y,
                    84.0,
                    PARAM_HEIGHT,
                );
                elements.push(Element {
                    rect: Rect::new(control.x, param_y, control.width, PARAM_HEIGHT),
                    hit: Hit::Slider(index, p),
                });
                params.push(ParamRow {
                    index: p,
                    label,
                    control,
                    value,
                });
                param_y += PARAM_HEIGHT;
            }
        }

        rows.push(StageRow {
            index,
            rect,
            checkbox,
            disclosure,
            label,
            description,
            params,
        });
        row_y = param_y;
    }

    let chain_panel = Rect::new(
        MARGIN,
        chain_top,
        inner,
        (row_y - chain_top + PANEL_PADDING).max(60.0),
    );

    // Hit-testing walks this front to back.
    elements.reverse();

    Layout {
        titlebar,
        dropzone,
        status,
        preset_panel,
        preset_label,
        preset_segments,
        revert,
        rerender,
        preset_note,
        chain_panel,
        chain_heading,
        rows,
        elements,
        content_height: (chain_panel.bottom() + MARGIN).max(height),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_ui::Msg;

    fn layout(model: &Model) -> Layout {
        compute(model, 760.0, 900.0)
    }

    #[test]
    fn the_title_strip_spans_the_window_and_holds_three_lights() {
        let model = Model::new();
        let layout = layout(&model);
        assert_eq!(layout.titlebar, Rect::new(0.0, 0.0, 760.0, TITLEBAR));

        let lights: Vec<Hit> = layout
            .elements
            .iter()
            .filter(|e| matches!(e.hit, Hit::Light(_)))
            .map(|e| e.hit)
            .collect();
        assert_eq!(lights.len(), 3);
    }

    #[test]
    fn a_click_on_a_light_is_a_light_and_not_a_drag() {
        // The lights sit inside the strip, so the order they are tested in is
        // the whole difference between closing a window and moving it.
        let layout = layout(&Model::new());
        let frames = aqua::chrome::light_frames(TITLEBAR);
        let centre = |r: objc2_core_foundation::CGRect| {
            (
                r.origin.x + r.size.width / 2.0,
                r.origin.y + r.size.height / 2.0,
            )
        };

        for (frame, light) in frames.into_iter().zip(aqua::palette::Light::ALL) {
            let (x, y) = centre(frame);
            assert_eq!(layout.hit(x, y), Some(Hit::Light(light)));
        }
        // And the rest of the strip drags.
        assert_eq!(layout.hit(400.0, 14.0), Some(Hit::TitleBar));
    }

    #[test]
    fn every_stage_gets_a_row_with_a_checkbox_and_a_disclosure() {
        let model = Model::new();
        let layout = layout(&model);
        assert_eq!(layout.rows.len(), model.stages().len());

        for row in &layout.rows {
            assert_eq!(
                layout.hit(
                    row.checkbox.x + row.checkbox.width / 2.0,
                    row.checkbox.y + row.checkbox.height / 2.0
                ),
                Some(Hit::StageCheckbox(row.index))
            );
            assert_eq!(
                layout.hit(
                    row.disclosure.x + row.disclosure.width / 2.0,
                    row.disclosure.y + row.disclosure.height / 2.0
                ),
                Some(Hit::StageDisclosure(row.index))
            );
        }
    }

    #[test]
    fn rows_do_not_overlap() {
        let layout = layout(&Model::new());
        for pair in layout.rows.windows(2) {
            assert!(
                pair[0].rect.bottom() <= pair[1].rect.y,
                "row {} runs into row {}",
                pair[0].index,
                pair[1].index
            );
        }
    }

    #[test]
    fn opening_a_stage_makes_room_for_its_parameters() {
        let mut model = Model::new();
        let closed = layout(&model);
        assert!(closed.rows[0].params.is_empty());

        model.update(Msg::ToggleExpanded(0));
        let open = layout(&model);
        assert_eq!(open.rows[0].params.len(), model.stages()[0].params.len());
        assert!(
            open.content_height >= closed.content_height,
            "the panel should have grown"
        );
        // And the rows below moved down rather than being overdrawn.
        assert!(open.rows[1].rect.y > closed.rows[1].rect.y);
    }

    #[test]
    fn a_parameter_slider_can_be_hit() {
        let mut model = Model::new();
        model.update(Msg::ToggleExpanded(0));
        let layout = layout(&model);

        let param = &layout.rows[0].params[0];
        assert_eq!(
            layout.hit(param.control.x + param.control.width / 2.0, param.control.y),
            Some(Hit::Slider(0, 0))
        );
    }

    #[test]
    fn the_status_panel_appears_only_once_something_has_happened() {
        let mut model = Model::new();
        assert!(layout(&model).status.is_none());

        model.update(Msg::Dropped(std::path::PathBuf::from("/tmp/talk.wav")));
        assert!(layout(&model).status.is_some());

        model.update(Msg::Finished(Err("no".into())));
        assert!(layout(&model).status.is_some());
    }

    #[test]
    fn the_status_panel_pushes_everything_below_it_down() {
        let mut model = Model::new();
        let before = layout(&model);
        model.update(Msg::Dropped(std::path::PathBuf::from("/tmp/talk.wav")));
        let after = layout(&model);

        assert!(after.preset_panel.y > before.preset_panel.y);
        assert!(after.chain_panel.y > before.chain_panel.y);
        assert!(after.rows[0].rect.y > before.rows[0].rect.y);
    }

    #[test]
    fn there_is_one_segment_per_preset_and_each_can_be_hit() {
        let model = Model::new();
        let layout = layout(&model);
        for (i, _) in model.preset_names().iter().enumerate() {
            let width = layout.preset_segments.width / model.preset_names().len() as f64;
            let x = layout.preset_segments.x + (i as f64 + 0.5) * width;
            let y = layout.preset_segments.y + layout.preset_segments.height / 2.0;
            assert_eq!(layout.hit(x, y), Some(Hit::Preset(i)));
        }
    }

    #[test]
    fn the_buttons_do_not_overlap_each_other() {
        let layout = layout(&Model::new());
        assert!(layout.revert.x + layout.revert.width <= layout.rerender.x);
        assert!(layout.preset_segments.x + layout.preset_segments.width <= layout.revert.x);
    }

    #[test]
    fn nothing_lands_outside_the_window() {
        let model = Model::new();
        let layout = compute(&model, 760.0, 900.0);
        for element in &layout.elements {
            assert!(element.rect.x >= 0.0, "{element:?}");
            assert!(
                element.rect.x + element.rect.width <= 760.0 + 0.01,
                "{element:?} runs past the right edge"
            );
        }
    }

    #[test]
    fn a_narrow_window_still_places_everything_left_to_right() {
        let layout = compute(&Model::new(), 560.0, 560.0);
        assert!(layout.dropzone.width > 0.0);
        assert!(layout.rerender.x > layout.revert.x);
        for element in &layout.elements {
            assert!(element.rect.width >= 0.0, "{element:?}");
        }
    }

    #[test]
    fn empty_space_hits_nothing() {
        let layout = layout(&Model::new());
        assert_eq!(layout.hit(5.0, layout.content_height - 5.0), None);
        assert_eq!(layout.hit(-10.0, 100.0), None);
    }

    #[test]
    fn the_content_is_at_least_as_tall_as_the_window() {
        // Otherwise a short chain would leave the metal ending halfway down.
        let layout = compute(&Model::new(), 760.0, 1200.0);
        assert!(layout.content_height >= 1200.0);
    }
}
