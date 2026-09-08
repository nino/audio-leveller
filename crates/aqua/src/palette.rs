//! Every colour and gradient stop, taken from the stylesheet the Electron
//! renderer painted.
//!
//! Nothing here is invented. Where a value looks arbitrary it is because the
//! CSS said so, and the comment says which rule.
//!
//! Aqua flattens and mutes a window that is not in front, so almost everything
//! comes in two versions. The rule the original followed and this keeps: chrome
//! goes grey, *content* does not — text stays readable and an error stays red,
//! because an inactive window is still one you can read.

/// Straight sRGB, not premultiplied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Colour {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 1.0)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: f64) -> Self {
        Self {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a,
        }
    }

    pub const fn white(a: f64) -> Self {
        Self {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a,
        }
    }

    pub const fn black(a: f64) -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a,
        }
    }

    pub const fn with_alpha(self, a: f64) -> Self {
        Self { a, ..self }
    }

    /// The colour this becomes when laid over `under`.
    ///
    /// Used to flatten the semi-transparent panel fills when the accessibility
    /// setting asks for reduced transparency.
    pub fn over(self, under: Self) -> Self {
        let a = self.a + under.a * (1.0 - self.a);
        if a == 0.0 {
            return Self::white(0.0);
        }
        let mix = |top: f64, bottom: f64| (top * self.a + bottom * under.a * (1.0 - self.a)) / a;
        Self {
            r: mix(self.r, under.r),
            g: mix(self.g, under.g),
            b: mix(self.b, under.b),
            a,
        }
    }

    /// Toward grey, for the muted variants.
    pub fn desaturated(self, amount: f64) -> Self {
        let grey = 0.299 * self.r + 0.587 * self.g + 0.114 * self.b;
        let mix = |c: f64| c + (grey - c) * amount;
        Self {
            r: mix(self.r),
            g: mix(self.g),
            b: mix(self.b),
            a: self.a,
        }
    }

    pub fn components(self) -> [f64; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

/// A gradient stop: a colour and where it sits, from 0 to 1.
pub type Stop = (f64, Colour);

/// Whether the window is the front one.
///
/// Not a boolean at the call sites, because `paint(true)` reads as nothing at
/// all and there are a great many call sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Active,
    Inactive,
}

impl Focus {
    pub fn of(active: bool) -> Self {
        if active { Self::Active } else { Self::Inactive }
    }

    pub fn is_active(self) -> bool {
        self == Self::Active
    }

    /// Pick between an active and an inactive value.
    pub fn pick<T>(self, active: T, inactive: T) -> T {
        match self {
            Self::Active => active,
            Self::Inactive => inactive,
        }
    }
}

// ------------------------------------------------------------------- metal --

/// The brushed-metal ground. From `.window`.
pub fn metal(focus: Focus) -> Vec<Stop> {
    focus.pick(
        vec![
            (0.0, Colour::rgb(0xd6, 0xd6, 0xd6)),
            (0.5, Colour::rgb(0xc3, 0xc3, 0xc3)),
            (1.0, Colour::rgb(0xb9, 0xb9, 0xb9)),
        ],
        vec![
            (0.0, Colour::rgb(0xcd, 0xcd, 0xcd)),
            (1.0, Colour::rgb(0xc6, 0xc6, 0xc6)),
        ],
    )
}

/// The grain over the metal: a three-point tile of one light row, one blend and
/// one dark. From the `repeating-linear-gradient` in `.window`.
pub fn metal_grain(focus: Focus) -> [Colour; 3] {
    let (light, dark) = focus.pick((0.07, 0.035), (0.03, 0.015));
    [
        Colour::white(light),
        // The blended middle row the CSS ramp produces between the two.
        Colour::white(light / 2.0).over(Colour::black(dark / 2.0)),
        Colour::black(dark),
    ]
}

/// The 1pt highlight along the top of the metal.
pub fn metal_highlight(focus: Focus) -> Colour {
    Colour::white(focus.pick(0.55, 0.30))
}

// ---------------------------------------------------------------- titlebar --

pub const TITLEBAR_HEIGHT: f64 = 28.0;

/// From `.titlebar`.
pub fn titlebar(focus: Focus) -> Vec<Stop> {
    focus.pick(
        vec![
            (0.0, Colour::rgb(0xe6, 0xe6, 0xe6)),
            (0.45, Colour::rgb(0xcf, 0xcf, 0xcf)),
            (0.55, Colour::rgb(0xbd, 0xbd, 0xbd)),
            (1.0, Colour::rgb(0xc8, 0xc8, 0xc8)),
        ],
        vec![
            (0.0, Colour::rgb(0xde, 0xde, 0xde)),
            (1.0, Colour::rgb(0xd0, 0xd0, 0xd0)),
        ],
    )
}

pub fn titlebar_border(focus: Focus) -> Colour {
    focus.pick(Colour::rgb(0x8a, 0x8a, 0x8a), Colour::rgb(0xa8, 0xa8, 0xa8))
}

pub fn titlebar_highlight(focus: Focus) -> Colour {
    Colour::white(focus.pick(0.85, 0.45))
}

pub fn title_text(focus: Focus) -> Colour {
    focus.pick(Colour::rgb(0x2b, 0x2b, 0x2b), Colour::rgb(0x8b, 0x8b, 0x8b))
}

pub fn title_shadow(focus: Focus) -> Colour {
    Colour::white(focus.pick(0.7, 0.55))
}

// ---------------------------------------------------------- traffic lights --

pub const LIGHT_SIZE: f64 = 13.0;
pub const LIGHT_GAP: f64 = 8.0;
pub const LIGHT_LEFT: f64 = 9.0;

/// Which of the three, and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Close,
    Minimise,
    Zoom,
}

impl Light {
    pub const ALL: [Self; 3] = [Self::Close, Self::Minimise, Self::Zoom];

    /// The radial stops, from `.light.red` and its two siblings.
    pub fn stops(self) -> [Colour; 3] {
        match self {
            Self::Close => [
                Colour::rgb(0xff, 0xb2, 0xad),
                Colour::rgb(0xee, 0x5b, 0x52),
                Colour::rgb(0xd3, 0x37, 0x2f),
            ],
            Self::Minimise => [
                Colour::rgb(0xff, 0xf1, 0xa8),
                Colour::rgb(0xf5, 0xc3, 0x3a),
                Colour::rgb(0xd9, 0x9f, 0x1a),
            ],
            Self::Zoom => [
                Colour::rgb(0xc8, 0xf7, 0xb0),
                Colour::rgb(0x62, 0xc6, 0x55),
                Colour::rgb(0x3d, 0x9b, 0x33),
            ],
        }
    }

    /// The glyph Aqua shows while the pointer is over the group.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Close => "×",
            Self::Minimise => "−",
            Self::Zoom => "+",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::Minimise => "Minimise",
            Self::Zoom => "Zoom",
        }
    }
}

/// What an inactive light fades to. From `body.inactive .light`.
pub fn light_inactive() -> Vec<Stop> {
    vec![
        (0.0, Colour::rgb(0xd0, 0xd0, 0xd0)),
        (1.0, Colour::rgb(0xb8, 0xb8, 0xb8)),
    ]
}

pub fn light_border(focus: Focus) -> Colour {
    Colour::black(focus.pick(0.35, 0.25))
}

// ------------------------------------------------------------- gel buttons --

/// Which gel a control wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gel {
    /// The ordinary grey button. From `.aqua-button`.
    Grey,
    /// The default button, and a selected segment. From `.aqua-button.primary`.
    Blue,
}

/// The four-stop gel fill. The 45–50% step is the waistline; keeping those
/// positions exact is most of what makes it read as gel rather than as a ramp.
pub fn gel(kind: Gel, focus: Focus) -> Vec<Stop> {
    if focus == Focus::Inactive {
        // Both variants go to the same grey when the window is not in front,
        // which is what makes a default button stop shouting from the back.
        return vec![
            (0.0, Colour::rgb(0xf6, 0xf6, 0xf6)),
            (0.45, Colour::rgb(0xea, 0xea, 0xea)),
            (0.50, Colour::rgb(0xdf, 0xdf, 0xdf)),
            (1.0, Colour::rgb(0xea, 0xea, 0xea)),
        ];
    }
    match kind {
        Gel::Grey => vec![
            (0.0, Colour::rgb(0xfd, 0xfd, 0xfd)),
            (0.45, Colour::rgb(0xe6, 0xe6, 0xe6)),
            (0.50, Colour::rgb(0xd1, 0xd1, 0xd1)),
            (1.0, Colour::rgb(0xec, 0xec, 0xec)),
        ],
        Gel::Blue => vec![
            (0.0, Colour::rgb(0xb8, 0xd4, 0xff)),
            (0.45, Colour::rgb(0x6a, 0xa2, 0xf5)),
            (0.50, Colour::rgb(0x3d, 0x82, 0xea)),
            (1.0, Colour::rgb(0x77, 0xb3, 0xfb)),
        ],
    }
}

/// The pressed fill. From `.aqua-button:active`.
pub fn gel_pressed() -> Vec<Stop> {
    vec![
        (0.0, Colour::rgb(0xd0, 0xd0, 0xd0)),
        (1.0, Colour::rgb(0xbc, 0xbc, 0xbc)),
    ]
}

pub fn gel_border(kind: Gel, focus: Focus) -> Colour {
    match (kind, focus) {
        (_, Focus::Inactive) => Colour::rgb(0x9b, 0x9b, 0x9b),
        (Gel::Grey, _) => Colour::rgb(0x6a, 0x6a, 0x6a),
        (Gel::Blue, _) => Colour::rgb(0x3a, 0x5c, 0x9c),
    }
}

pub fn gel_text(kind: Gel, focus: Focus) -> Colour {
    match (kind, focus) {
        (_, Focus::Inactive) => Colour::rgb(0x5e, 0x5e, 0x5e),
        (Gel::Grey, _) => Colour::rgb(0x11, 0x11, 0x11),
        (Gel::Blue, _) => Colour::white(1.0),
    }
}

/// The text shadow, which is above the text on a grey button and below it on a
/// blue one — the trick that makes light text on a saturated fill read as
/// engraved rather than as a sticker.
pub fn gel_text_shadow(kind: Gel, focus: Focus) -> Option<(f64, Colour)> {
    match (kind, focus) {
        (_, Focus::Inactive) => None,
        (Gel::Grey, _) => Some((1.0, Colour::white(0.8))),
        (Gel::Blue, _) => Some((-1.0, Colour::black(0.35))),
    }
}

/// The highlight along a gel's top edge.
pub fn gel_highlight(kind: Gel, focus: Focus) -> Colour {
    match (kind, focus) {
        (_, Focus::Inactive) => Colour::white(0.7),
        (Gel::Grey, _) => Colour::white(0.95),
        (Gel::Blue, _) => Colour::white(0.85),
    }
}

// ----------------------------------------------------------------- panels --

/// The inset panel. From `.metal-inset`.
pub fn inset_fill(focus: Focus) -> Vec<Stop> {
    let (top, bottom) = focus.pick((0.35, 0.15), (0.22, 0.10));
    vec![(0.0, Colour::white(top)), (1.0, Colour::white(bottom))]
}

pub fn inset_border(focus: Focus) -> Colour {
    Colour::black(focus.pick(0.22, 0.16))
}

/// The glass panel. From `.glass`.
pub fn glass_fill(focus: Focus) -> Vec<Stop> {
    focus.pick(
        vec![
            (0.0, Colour::white(0.85)),
            (1.0, Colour::rgba(0xe6, 0xee, 0xfa, 0.75)),
        ],
        vec![
            (0.0, Colour::white(0.72)),
            (1.0, Colour::rgba(0xf0, 0xf0, 0xf0, 0.66)),
        ],
    )
}

pub fn glass_border(focus: Focus) -> Colour {
    focus.pick(Colour::rgba(0x3c, 0x50, 0x78, 0.35), Colour::black(0.22))
}

/// The blue glow inside the bottom of a glass panel, which is most of what
/// makes it read as glass rather than as white.
pub fn glass_inner_glow(focus: Focus) -> Option<Colour> {
    focus
        .is_active()
        .then(|| Colour::rgba(0xa0, 0xbe, 0xe6, 0.25))
}

// -------------------------------------------------------------- drop zone --

/// From `.dropzone`.
pub fn dropzone_fill(focus: Focus) -> Vec<Stop> {
    focus.pick(
        vec![
            (0.0, Colour::white(0.8)),
            (1.0, Colour::rgba(0xe2, 0xeb, 0xf9, 0.7)),
        ],
        vec![
            (0.0, Colour::white(0.7)),
            (1.0, Colour::rgba(0xef, 0xef, 0xef, 0.62)),
        ],
    )
}

pub fn dropzone_border(focus: Focus, hovered: bool) -> Colour {
    if hovered {
        return Colour::rgb(0x3d, 0x82, 0xea);
    }
    focus.pick(Colour::rgba(0x3c, 0x50, 0x78, 0.45), Colour::black(0.28))
}

// ------------------------------------------------------------------- text --

pub fn body_text() -> Colour {
    Colour::rgb(0x1c, 0x1c, 0x1c)
}

pub fn hint_text() -> Colour {
    Colour::rgb(0x55, 0x55, 0x55)
}

/// The white shadow under text on metal, which is what stops it looking pasted
/// on. From the `text-shadow` on `h2` and `.hint`.
pub fn text_emboss() -> Colour {
    Colour::white(0.7)
}

pub fn divider(focus: Focus) -> Colour {
    Colour::black(focus.pick(0.13, 0.09))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_becomes_the_fraction_it_should() {
        let c = Colour::rgb(0xff, 0x80, 0x00);
        assert_eq!(c.r, 1.0);
        assert!((c.g - 128.0 / 255.0).abs() < 1e-12);
        assert_eq!(c.b, 0.0);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn compositing_matches_what_the_browser_would_do() {
        // 50% white over black is mid grey, and the result is opaque.
        let mixed = Colour::white(0.5).over(Colour::black(1.0));
        assert!((mixed.r - 0.5).abs() < 1e-12);
        assert_eq!(mixed.a, 1.0);

        // An opaque top colour hides whatever is under it.
        let opaque = Colour::rgb(0x12, 0x34, 0x56).over(Colour::white(1.0));
        assert_eq!(opaque, Colour::rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn compositing_nothing_over_nothing_does_not_divide_by_zero() {
        let nothing = Colour::white(0.0).over(Colour::black(0.0));
        assert_eq!(nothing.a, 0.0);
        assert!(nothing.r.is_finite());
    }

    #[test]
    fn full_desaturation_leaves_a_grey_and_keeps_its_alpha() {
        let grey = Colour::rgb(0xee, 0x5b, 0x52)
            .with_alpha(0.5)
            .desaturated(1.0);
        assert!((grey.r - grey.g).abs() < 1e-12);
        assert!((grey.g - grey.b).abs() < 1e-12);
        assert_eq!(grey.a, 0.5);
        // And none at all changes nothing.
        let same = Colour::rgb(0xee, 0x5b, 0x52).desaturated(0.0);
        assert_eq!(same, Colour::rgb(0xee, 0x5b, 0x52));
    }

    #[test]
    fn every_gradient_runs_from_zero_to_one_in_order() {
        let gradients: Vec<Vec<Stop>> = [Focus::Active, Focus::Inactive]
            .into_iter()
            .flat_map(|focus| {
                vec![
                    metal(focus),
                    titlebar(focus),
                    gel(Gel::Grey, focus),
                    gel(Gel::Blue, focus),
                    inset_fill(focus),
                    glass_fill(focus),
                    dropzone_fill(focus),
                ]
            })
            .chain([gel_pressed(), light_inactive()])
            .collect();

        for stops in gradients {
            assert!(stops.len() >= 2, "a gradient needs two stops");
            assert_eq!(stops.first().unwrap().0, 0.0);
            assert_eq!(stops.last().unwrap().0, 1.0);
            assert!(
                stops.windows(2).all(|w| w[1].0 >= w[0].0),
                "stops out of order: {stops:?}"
            );
        }
    }

    #[test]
    fn the_gel_waistline_is_where_it_has_to_be() {
        // The 45-to-50% step is what makes it read as gel. A "tidy" even
        // spacing would flatten it into an ordinary ramp.
        for kind in [Gel::Grey, Gel::Blue] {
            let stops = gel(kind, Focus::Active);
            assert_eq!(stops.len(), 4);
            assert_eq!(stops[1].0, 0.45);
            assert_eq!(stops[2].0, 0.50);
        }
    }

    #[test]
    fn an_inactive_window_mutes_its_chrome_but_not_its_content() {
        // The rule the stylesheet followed: controls go grey, text does not.
        assert_ne!(
            gel(Gel::Blue, Focus::Active),
            gel(Gel::Blue, Focus::Inactive)
        );
        assert_eq!(
            gel(Gel::Grey, Focus::Inactive),
            gel(Gel::Blue, Focus::Inactive),
            "a default button should stop shouting from the back"
        );
        assert_eq!(body_text(), Colour::rgb(0x1c, 0x1c, 0x1c));
    }

    #[test]
    fn an_inactive_metal_is_flatter_than_an_active_one() {
        let active = metal(Focus::Active);
        let inactive = metal(Focus::Inactive);
        let span = |stops: &[Stop]| stops.first().unwrap().1.r - stops.last().unwrap().1.r;
        assert!(span(&active) > span(&inactive));
        assert!(metal_grain(Focus::Inactive)[0].a < metal_grain(Focus::Active)[0].a);
    }

    #[test]
    fn the_three_lights_are_three_different_colours() {
        let reds: Vec<f64> = Light::ALL.iter().map(|l| l.stops()[1].r).collect();
        assert!(reds[0] > reds[1] * 0.9, "close is the red one");
        assert!(Light::Zoom.stops()[1].g > Light::Zoom.stops()[1].r);
        for light in Light::ALL {
            assert!(!light.glyph().is_empty());
            assert!(!light.label().is_empty());
        }
    }

    #[test]
    fn focus_reads_as_what_it_is() {
        assert!(Focus::of(true).is_active());
        assert!(!Focus::of(false).is_active());
        assert_eq!(Focus::Active.pick(1, 2), 1);
        assert_eq!(Focus::Inactive.pick(1, 2), 2);
    }
}
