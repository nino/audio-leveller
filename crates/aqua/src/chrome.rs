//! The window's own chrome, drawn rather than asked for: the brushed metal,
//! the title strip, and the three gel lights.
//!
//! These are functions over a graphics context rather than views, so the same
//! code paints a live window and an offscreen bitmap — which is what makes the
//! look testable at all. The views in [`crate::views`] are thin wrappers that
//! call them.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;

use crate::paint::{self, Shape};
use crate::palette::{self, Colour, Focus, Gel, Light};
use crate::text::{self, Align, Style};

/// The brushed-metal ground: a vertical gradient, a fine horizontal grain, and
/// a highlight along the top.
///
/// The grain is a three-row repeat — light, blend, dark — which is what the
/// CSS `repeating-linear-gradient` produces over its four-pixel period. At a
/// glance it is texture; up close it is the same one-point stripes the original
/// drew.
pub fn metal(ctx: &CGContext, rect: CGRect, focus: Focus) {
    let path = Shape::Rect.path(rect);
    paint::fill_gradient(ctx, &path, rect, &palette::metal(focus));

    let grain = palette::metal_grain(focus);
    paint::clipped(ctx, &path, |ctx| {
        let mut y = rect.origin.y;
        while y < rect.origin.y + rect.size.height {
            for (row, colour) in grain.iter().enumerate() {
                paint::fill_rect(
                    ctx,
                    paint::rect(rect.origin.x, y + row as f64, rect.size.width, 1.0),
                    *colour,
                );
            }
            y += grain.len() as f64;
        }
    });

    paint::fill_rect(
        ctx,
        paint::rect(rect.origin.x, rect.origin.y, rect.size.width, 1.0),
        palette::metal_highlight(focus),
    );
}

/// The title strip, with the window's name centred in it.
///
/// The lights are drawn separately, by [`traffic_lights`], because they are
/// real buttons and this is only paint.
pub fn titlebar(ctx: &CGContext, rect: CGRect, title: &str, focus: Focus) {
    let path = Shape::Rect.path(rect);
    paint::fill_gradient(ctx, &path, rect, &palette::titlebar(focus));

    paint::fill_rect(
        ctx,
        paint::rect(rect.origin.x, rect.origin.y, rect.size.width, 1.0),
        palette::titlebar_highlight(focus),
    );
    paint::fill_rect(
        ctx,
        paint::rect(
            rect.origin.x,
            rect.origin.y + rect.size.height - 1.0,
            rect.size.width,
            1.0,
        ),
        palette::titlebar_border(focus),
    );

    text::draw(
        title,
        rect,
        &Style::default()
            .size(13.0)
            .colour(palette::title_text(focus))
            .shadow(1.0, palette::title_shadow(focus))
            .align(Align::Centre),
    );
}

/// Where each light sits inside a title strip of `height`.
pub fn light_frames(height: f64) -> [CGRect; 3] {
    let y = (height - palette::LIGHT_SIZE) / 2.0;
    std::array::from_fn(|i| {
        paint::rect(
            palette::LIGHT_LEFT + i as f64 * (palette::LIGHT_SIZE + palette::LIGHT_GAP),
            y,
            palette::LIGHT_SIZE,
            palette::LIGHT_SIZE,
        )
    })
}

/// How a light is being interacted with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightState {
    /// True while the pointer is anywhere over the group — Aqua shows the
    /// glyphs on all three at once, not one at a time.
    pub hovered: bool,
    pub pressed: bool,
}

/// One gel traffic light.
///
/// The order is the order the CSS lists its shadows in, and it matters: the
/// dark inner shadow at the bottom is what gives the dome its underside, the
/// white one at the top is the highlight sitting on it, and the border goes
/// over both.
pub fn light(ctx: &CGContext, rect: CGRect, which: Light, state: LightState, focus: Focus) {
    let path = Shape::Circle.path(rect);

    // The outer `0 1px 1px rgba(255,255,255,.5)`, under everything else.
    paint::drop_shadow(ctx, &path, (0.0, 1.0), 1.0, Colour::white(0.5));

    if focus.is_active() {
        let [near, mid, far] = which.stops();
        paint::fill_radial(
            ctx,
            &path,
            rect,
            (0.5, 0.3),
            &[(0.0, near), (0.45, mid), (1.0, far)],
        );
    } else {
        paint::fill_gradient(ctx, &path, rect, &palette::light_inactive());
    }

    if state.pressed {
        // Pressed inverts the dome: the shadow moves to the top.
        paint::inner_shadow(ctx, &path, (0.0, 2.0), 4.0, Colour::black(0.4));
        paint::inner_shadow(ctx, &path, (0.0, -1.0), 2.0, Colour::white(0.4));
    } else if focus.is_active() {
        paint::inner_shadow(ctx, &path, (0.0, -2.0), 3.0, Colour::black(0.25));
        paint::inner_shadow(ctx, &path, (0.0, 2.0), 2.0, Colour::white(0.85));
    } else {
        // Inactive keeps only the top highlight, so the row reads as three
        // discs rather than three domes.
        paint::inner_highlight_top(ctx, &path, Colour::white(0.5));
    }

    paint::stroke_inside(ctx, &path, 1.0, palette::light_border(focus));

    // The glyph appears only while the pointer is over the group, and never on
    // an inactive window — which is what the original did, and what makes the
    // row look like an ornament until you reach for it.
    if state.hovered && focus.is_active() {
        text::draw(
            which.glyph(),
            rect,
            &Style::default()
                .size(9.0)
                .bold()
                .colour(Colour::black(0.55))
                .shadow(1.0, Colour::white(0.45))
                .align(Align::Centre),
        );
    }
}

/// All three lights, at the left of a title strip.
pub fn traffic_lights(ctx: &CGContext, height: f64, state: LightState, focus: Focus) {
    for (frame, which) in light_frames(height).into_iter().zip(Light::ALL) {
        light(ctx, frame, which, state, focus);
    }
}

/// What a gel control is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonState {
    pub hovered: bool,
    pub pressed: bool,
    pub enabled: bool,
}

impl ButtonState {
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// A gel button: drop shadow, gradient, top highlight, border, title.
///
/// The order is the CSS's, and each step is one declaration from
/// `.aqua-button`.
pub fn gel_button(
    ctx: &CGContext,
    rect: CGRect,
    title: &str,
    kind: Gel,
    state: ButtonState,
    focus: Focus,
) {
    let draw = |ctx: &CGContext| {
        let path = Shape::Capsule.path(rect);

        if focus.is_active() && !state.pressed {
            let blur = if kind == Gel::Blue { 2.0 } else { 1.0 };
            paint::drop_shadow(ctx, &path, (0.0, 1.0), blur, Colour::black(0.35));
        }

        if state.pressed {
            paint::fill_gradient(ctx, &path, rect, &palette::gel_pressed());
            paint::inner_shadow(ctx, &path, (0.0, 1.0), 3.0, Colour::black(0.35));
        } else {
            paint::fill_gradient(ctx, &path, rect, &palette::gel(kind, focus));
            paint::inner_highlight_top(ctx, &path, palette::gel_highlight(kind, focus));
        }

        paint::stroke_inside(ctx, &path, 1.0, palette::gel_border(kind, focus));

        let mut style = Style::default()
            .size(if rect.size.height < 22.0 { 11.0 } else { 13.0 })
            .colour(palette::gel_text(kind, focus))
            .align(Align::Centre);
        if let Some((offset, colour)) = palette::gel_text_shadow(kind, focus) {
            style = style.shadow(offset, colour);
        }
        text::draw(title, rect, &style);
    };

    // `opacity: 0.5` on a disabled control, which dims the whole thing at once
    // rather than needing a second palette.
    if state.enabled {
        draw(ctx);
    } else {
        paint::with_alpha(ctx, 0.5, draw);
    }
}

/// The panel kinds the stylesheet has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// `.metal-inset`: a shallow well pressed into the metal.
    Inset,
    /// `.glass`: the raised, faintly blue slab the status panel uses.
    Glass,
}

/// A panel.
pub fn panel(ctx: &CGContext, rect: CGRect, kind: Panel, focus: Focus) {
    let path = Shape::Rounded(6.0).path(rect);

    match kind {
        Panel::Inset => {
            paint::fill_gradient(ctx, &path, rect, &palette::inset_fill(focus));
            let (offset, blur, alpha) = focus.pick((1.0, 3.0, 0.18), (1.0, 2.0, 0.10));
            paint::inner_shadow(ctx, &path, (0.0, offset), blur, Colour::black(alpha));
            paint::stroke_inside(ctx, &path, 1.0, palette::inset_border(focus));
            // The `0 1px 0 rgba(255,255,255,.6)` under the panel, which is what
            // makes it look pressed in rather than drawn on.
            paint::fill_rect(
                ctx,
                paint::rect(
                    rect.origin.x + 6.0,
                    rect.origin.y + rect.size.height,
                    rect.size.width - 12.0,
                    1.0,
                ),
                Colour::white(focus.pick(0.6, 0.4)),
            );
        }
        Panel::Glass => {
            paint::drop_shadow(
                ctx,
                &path,
                (0.0, 1.0),
                focus.pick(2.0, 1.0),
                Colour::black(focus.pick(0.25, 0.14)),
            );
            paint::fill_gradient(ctx, &path, rect, &palette::glass_fill(focus));
            paint::inner_highlight_top(ctx, &path, Colour::white(focus.pick(1.0, 0.7)));
            if let Some(glow) = palette::glass_inner_glow(focus) {
                paint::inner_shadow(ctx, &path, (0.0, -14.0), 24.0, glow);
            }
            paint::stroke_inside(ctx, &path, 1.0, palette::glass_border(focus));
        }
    }
}

/// The drop target: a glassy well with a dashed border.
pub fn dropzone(ctx: &CGContext, rect: CGRect, hovered: bool, focus: Focus) {
    let path = Shape::Rounded(8.0).path(rect);

    paint::fill_gradient(ctx, &path, rect, &palette::dropzone_fill(focus));
    paint::inner_highlight_top(ctx, &path, Colour::white(focus.pick(1.0, 0.7)));
    if focus.is_active() {
        paint::inner_shadow(
            ctx,
            &path,
            (0.0, -12.0),
            22.0,
            Colour::rgba(0xa0, 0xbe, 0xe6, 0.22),
        );
    }
    if hovered {
        // The glow the original animates on drag-over.
        paint::drop_shadow(
            ctx,
            &path,
            (0.0, 0.0),
            12.0,
            Colour::rgba(0x50, 0x8c, 0xf0, 0.75),
        );
    }
    paint::stroke_dashed(
        ctx,
        &path,
        2.0,
        &[6.0, 6.0],
        palette::dropzone_border(focus, hovered),
    );
}

/// A horizontal rule between rows.
pub fn divider(ctx: &CGContext, x: f64, y: f64, width: f64, focus: Focus) {
    paint::fill_rect(ctx, paint::rect(x, y, width, 1.0), palette::divider(focus));
}

/// A segmented control: one capsule cut into pieces, with the selected one
/// wearing the blue gel.
pub fn segmented(
    ctx: &CGContext,
    rect: CGRect,
    titles: &[&str],
    selected: usize,
    focus: Focus,
) -> Vec<CGRect> {
    let path = Shape::Capsule.path(rect);
    if focus.is_active() {
        paint::drop_shadow(ctx, &path, (0.0, 1.0), 1.0, Colour::black(0.3));
    }
    paint::fill_gradient(ctx, &path, rect, &palette::gel(Gel::Grey, focus));

    let width = if titles.is_empty() {
        rect.size.width
    } else {
        rect.size.width / titles.len() as f64
    };
    let frames: Vec<CGRect> = (0..titles.len())
        .map(|i| {
            CGRect::new(
                CGPoint::new(rect.origin.x + i as f64 * width, rect.origin.y),
                CGSize::new(width, rect.size.height),
            )
        })
        .collect();

    paint::clipped(ctx, &path, |ctx| {
        for (i, frame) in frames.iter().enumerate() {
            if i == selected {
                let segment = Shape::Rect.path(*frame);
                paint::fill_gradient(ctx, &segment, *frame, &palette::gel(Gel::Blue, focus));
                paint::inner_shadow(
                    ctx,
                    &segment,
                    (0.0, 1.0),
                    3.0,
                    Colour::black(focus.pick(0.25, 0.18)),
                );
            } else {
                paint::inner_highlight_top(
                    ctx,
                    &Shape::Rect.path(*frame),
                    palette::gel_highlight(Gel::Grey, focus),
                );
            }
            if i + 1 < titles.len() {
                paint::fill_rect(
                    ctx,
                    paint::rect(
                        frame.origin.x + frame.size.width - 1.0,
                        frame.origin.y,
                        1.0,
                        frame.size.height,
                    ),
                    focus.pick(Colour::rgb(0x8a, 0x8a, 0x8a), Colour::rgb(0xb6, 0xb6, 0xb6)),
                );
            }
        }
    });

    paint::stroke_inside(ctx, &path, 1.0, palette::gel_border(Gel::Grey, focus));

    for (i, (frame, title)) in frames.iter().zip(titles).enumerate() {
        let kind = if i == selected { Gel::Blue } else { Gel::Grey };
        let mut style = Style::default()
            .size(13.0)
            .colour(palette::gel_text(kind, focus))
            .align(Align::Centre);
        if let Some((offset, colour)) = palette::gel_text_shadow(kind, focus) {
            style = style.shadow(offset, colour);
        }
        text::draw(title, *frame, &style);
    }

    frames
}

/// A checkbox, drawn rather than borrowed: the system's is a rounded square
/// with the accent colour, and this is the 10.2 one.
pub fn checkbox(ctx: &CGContext, rect: CGRect, checked: bool, focus: Focus) {
    let path = Shape::Rounded(3.0).path(rect);

    if checked {
        let blue = focus.pick(
            vec![
                (0.0, Colour::rgb(0x6a, 0xa2, 0xf5)),
                (1.0, Colour::rgb(0x3d, 0x82, 0xea)),
            ],
            vec![
                (0.0, Colour::rgb(0xc4, 0xc4, 0xc4)),
                (1.0, Colour::rgb(0xb0, 0xb0, 0xb0)),
            ],
        );
        paint::fill_gradient(ctx, &path, rect, &blue);
        paint::inner_highlight_top(ctx, &path, Colour::white(0.5));
        paint::stroke_inside(
            ctx,
            &path,
            1.0,
            focus.pick(Colour::rgb(0x2b, 0x4f, 0x93), Colour::rgb(0x9b, 0x9b, 0x9b)),
        );
        text::draw(
            "✓",
            rect,
            &Style::default()
                .size(rect.size.height * 0.75)
                .bold()
                .colour(Colour::white(1.0))
                .align(Align::Centre),
        );
    } else {
        paint::fill_gradient(
            ctx,
            &path,
            rect,
            &[
                (0.0, Colour::rgb(0xfa, 0xfa, 0xfa)),
                (1.0, Colour::rgb(0xe4, 0xe4, 0xe4)),
            ],
        );
        paint::inner_shadow(ctx, &path, (0.0, 1.0), 2.0, Colour::black(0.2));
        paint::stroke_inside(ctx, &path, 1.0, Colour::rgb(0x8b, 0x8b, 0x8b));
    }
}

/// A progress bar: a sunken track with a blue gel fill and diagonal stripes.
pub fn progress(ctx: &CGContext, rect: CGRect, fraction: f64, focus: Focus) {
    let track = Shape::Capsule.path(rect);
    paint::fill_gradient(
        ctx,
        &track,
        rect,
        &[
            (0.0, Colour::rgb(0xd8, 0xd8, 0xd8)),
            (1.0, Colour::rgb(0xf0, 0xf0, 0xf0)),
        ],
    );
    paint::inner_shadow(ctx, &track, (0.0, 1.0), 2.0, Colour::black(0.3));

    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let filled = CGRect::new(
            rect.origin,
            CGSize::new(rect.size.width * fraction, rect.size.height),
        );
        paint::clipped(ctx, &track, |ctx| {
            let path = Shape::Rect.path(filled);
            paint::fill_gradient(ctx, &path, rect, &palette::gel(Gel::Blue, focus));
            // The barber's-pole stripes, at 45 degrees. Drawn as parallelograms
            // rather than as a rotated pattern, which is fewer moving parts for
            // the same result.
            paint::clipped(ctx, &path, |ctx| {
                let step = 12.0;
                let height = rect.size.height;
                let mut x = rect.origin.x - height;
                while x < rect.origin.x + filled.size.width + height {
                    let stripe = [
                        CGPoint::new(x, rect.origin.y + height),
                        CGPoint::new(x + height, rect.origin.y),
                        CGPoint::new(x + height + step / 2.0, rect.origin.y),
                        CGPoint::new(x + step / 2.0, rect.origin.y + height),
                    ];
                    paint::fill_polygon(ctx, &stripe, Colour::white(focus.pick(0.35, 0.28)));
                    x += step;
                }
            });
        });
    }

    paint::stroke_inside(
        ctx,
        &track,
        1.0,
        focus.pick(Colour::rgb(0x7d, 0x8f, 0xae), Colour::rgb(0xa4, 0xa4, 0xa4)),
    );
}
