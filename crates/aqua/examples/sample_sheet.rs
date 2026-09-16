//! Every control, in both focus states, on one sheet.
//!
//! `cargo run -p aqua --example sample_sheet -- /tmp/aqua.png`
//!
//! This is how the look is checked. Comparing it against `docs/screenshot.png`
//! is the whole review: if a gradient is wrong or an inner shadow is on the
//! wrong edge, it is visible here in one glance.

use aqua::chrome::{self, ButtonState, LightState, Panel};
use aqua::paint::rect;
use aqua::palette::{self, Focus, Gel};
use aqua::render;
use aqua::text::{self, Align, Style};

const WIDTH: f64 = 800.0;
const HEIGHT: f64 = 470.0;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "aqua.png".into());
    render::to_png(
        std::path::Path::new(&path),
        WIDTH,
        HEIGHT,
        2.0,
        |ctx, bounds| {
            chrome::metal(ctx, bounds, Focus::Active);

            // The title strip, with its lights, exactly as a window wears it.
            let bar = rect(0.0, 0.0, WIDTH, palette::TITLEBAR_HEIGHT);
            chrome::titlebar(ctx, bar, "Audio Leveller", Focus::Active);
            chrome::traffic_lights(
                ctx,
                palette::TITLEBAR_HEIGHT,
                LightState {
                    hovered: false,
                    pressed: false,
                },
                Focus::Active,
            );

            let mut y = palette::TITLEBAR_HEIGHT + 18.0;
            for focus in [Focus::Active, Focus::Inactive] {
                let label = focus.pick("Active", "Inactive");
                text::draw(
                    label,
                    rect(20.0, y, 200.0, 16.0),
                    &Style::default()
                        .size(11.0)
                        .bold()
                        .colour(palette::hint_text())
                        .embossed(),
                );
                y += 22.0;

                // Buttons, in every state the renderer can put them in.
                let states = [
                    ("Re-render", Gel::Grey, ButtonState::enabled()),
                    (
                        "Hovered",
                        Gel::Grey,
                        ButtonState {
                            hovered: true,
                            ..ButtonState::enabled()
                        },
                    ),
                    (
                        "Pressed",
                        Gel::Grey,
                        ButtonState {
                            pressed: true,
                            ..ButtonState::enabled()
                        },
                    ),
                    ("Disabled", Gel::Grey, ButtonState::default()),
                    ("Process", Gel::Blue, ButtonState::enabled()),
                ];
                let mut x = 20.0;
                for (title, kind, state) in states {
                    let width = 100.0;
                    chrome::gel_button(ctx, rect(x, y, width, 24.0), title, kind, state, focus);
                    x += width + 10.0;
                }

                // The segmented preset switch, and a checkbox row.
                chrome::segmented(ctx, rect(x, y, 124.0, 24.0), &["Nino", "ACX"], 0, focus);
                x += 136.0;
                chrome::checkbox(ctx, rect(x, y + 5.0, 14.0, 14.0), true, focus);
                chrome::checkbox(ctx, rect(x + 22.0, y + 5.0, 14.0, 14.0), false, focus);
                y += 38.0;

                // The two panels, side by side.
                chrome::panel(ctx, rect(20.0, y, 350.0, 62.0), Panel::Inset, focus);
                text::draw(
                    "Pipeline",
                    rect(34.0, y + 10.0, 200.0, 16.0),
                    &Style::default().size(14.0).bold().embossed(),
                );
                chrome::divider(ctx, 34.0, y + 32.0, 322.0, focus);
                text::draw(
                    "De-click · Denoise · De-reverb · EQ",
                    rect(34.0, y + 34.0, 322.0, 20.0),
                    &Style::default()
                        .size(11.0)
                        .colour(palette::hint_text())
                        .embossed(),
                );

                chrome::panel(ctx, rect(384.0, y, 356.0, 62.0), Panel::Glass, focus);
                text::draw(
                    "Levelled 3 segments to −18 LUFS",
                    rect(398.0, y + 8.0, 330.0, 18.0),
                    &Style::default().size(13.0),
                );
                chrome::progress(ctx, rect(398.0, y + 34.0, 328.0, 12.0), 0.62, focus);

                y += 78.0;
            }

            // The drop zone, which is the first thing anyone sees.
            let zone = rect(20.0, y, WIDTH - 40.0, HEIGHT - y - 20.0);
            chrome::dropzone(ctx, zone, false, Focus::Active);
            text::draw(
                "Audio Leveller",
                rect(zone.origin.x, zone.origin.y + 18.0, zone.size.width, 26.0),
                &Style::default()
                    .size(20.0)
                    .bold()
                    .align(Align::Centre)
                    .embossed(),
            );
            text::draw(
                "Drop a .wav file here",
                rect(zone.origin.x, zone.origin.y + 46.0, zone.size.width, 20.0),
                &Style::default().size(14.0).align(Align::Centre).embossed(),
            );
            text::draw(
                "Each speech segment is normalised to its target loudness",
                rect(zone.origin.x, zone.origin.y + 68.0, zone.size.width, 16.0),
                &Style::default()
                    .size(11.0)
                    .colour(palette::hint_text())
                    .align(Align::Centre)
                    .embossed(),
            );
        },
    )
    .expect("writing the sheet");
    println!("wrote {path}");
}
