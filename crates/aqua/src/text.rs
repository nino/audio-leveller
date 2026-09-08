//! Text, drawn the way the stylesheet drew it.
//!
//! The font is Lucida Grande, which is what the CSS asks for and what a 10.2
//! interface used. It still ships with macOS. Where it is missing the system
//! font stands in, which is the same fallback the CSS `font-family` list gives.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSShadow, NSShadowAttributeName, NSTextAlignment,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};

use crate::palette::Colour;

/// Where text sits inside the rectangle it is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
    Right,
}

impl Align {
    fn ns(self) -> NSTextAlignment {
        match self {
            Self::Left => NSTextAlignment::Left,
            Self::Centre => NSTextAlignment::Center,
            Self::Right => NSTextAlignment::Right,
        }
    }
}

/// How a run of text looks.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub size: f64,
    pub bold: bool,
    /// A fixed-pitch face, for numbers that should not jump about as they
    /// change.
    pub monospaced: bool,
    pub colour: Colour,
    /// A `text-shadow`, as a vertical offset and a colour. Positive is below.
    ///
    /// This is what stops text on metal looking pasted on, and it is the same
    /// trick in reverse — offset upward — that makes white text on a blue gel
    /// read as engraved.
    pub shadow: Option<(f64, Colour)>,
    pub align: Align,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            size: 13.0,
            bold: false,
            monospaced: false,
            colour: crate::palette::body_text(),
            shadow: None,
            align: Align::Left,
        }
    }
}

impl Style {
    pub fn size(mut self, size: f64) -> Self {
        self.size = size;
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn monospaced(mut self) -> Self {
        self.monospaced = true;
        self
    }

    pub fn colour(mut self, colour: Colour) -> Self {
        self.colour = colour;
        self
    }

    pub fn shadow(mut self, offset: f64, colour: Colour) -> Self {
        self.shadow = Some((offset, colour));
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// The white emboss under text on metal, which almost everything wants.
    pub fn embossed(self) -> Self {
        self.shadow(1.0, crate::palette::text_emboss())
    }
}

fn font(style: &Style) -> Retained<NSFont> {
    if style.monospaced {
        // Monaco is what the stylesheet asks for; the system's fixed-pitch face
        // is the fallback where it is missing.
        return NSString::from_str("Monaco")
            .pipe(|name| NSFont::fontWithName_size(&name, style.size))
            .unwrap_or_else(|| NSFont::userFixedPitchFontOfSize(style.size).unwrap());
    }

    let name = if style.bold {
        "LucidaGrande-Bold"
    } else {
        "LucidaGrande"
    };
    NSString::from_str(name)
        .pipe(|name| NSFont::fontWithName_size(&name, style.size))
        .unwrap_or_else(|| {
            if style.bold {
                NSFont::boldSystemFontOfSize(style.size)
            } else {
                NSFont::systemFontOfSize(style.size)
            }
        })
}

/// A tiny helper so the font lookups above read as one expression each.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}

fn ns_colour(colour: Colour) -> Retained<NSColor> {
    { NSColor::colorWithSRGBRed_green_blue_alpha(colour.r, colour.g, colour.b, colour.a) }
}

fn attributed(text: &str, style: &Style) -> Retained<NSAttributedString> {
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(style.align.ns());
    // One line, clipped rather than wrapped: every label here is a label.
    paragraph.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);

    let mut keys: Vec<&objc2_foundation::NSString> = Vec::with_capacity(4);
    let mut values: Vec<&objc2::runtime::AnyObject> = Vec::with_capacity(4);

    let font = font(style);
    let text_colour = ns_colour(style.colour);
    // SAFETY: these globals are the documented attribute keys.
    unsafe {
        keys.push(NSFontAttributeName);
        values.push(&font);
        keys.push(NSForegroundColorAttributeName);
        values.push(&text_colour);
        keys.push(NSParagraphStyleAttributeName);
        values.push(&paragraph);
    }

    let shadow = style.shadow.map(|(offset, tint)| {
        let shadow = NSShadow::new();
        {
            // The view is flipped, so a positive CSS offset — downward — is a
            // negative offset here.
            shadow.setShadowOffset(CGSize::new(0.0, -offset));
            shadow.setShadowBlurRadius(0.0);
            shadow.setShadowColor(Some(&ns_colour(tint)));
        }
        shadow
    });
    if let Some(shadow) = &shadow {
        // SAFETY: the documented attribute key.
        unsafe {
            keys.push(NSShadowAttributeName);
            values.push(shadow);
        }
    }

    let attributes = NSDictionary::from_slices(&keys, &values);
    // SAFETY: every value is of the class its key documents.
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), &attributes) }
}

/// Draw one line of text inside `rect`, vertically centred.
pub fn draw(text: &str, rect: CGRect, style: &Style) {
    if text.is_empty() {
        return;
    }
    let string = attributed(text, style);
    let size = { string.size() };
    let centred = CGRect::new(
        CGPoint::new(
            rect.origin.x,
            rect.origin.y + (rect.size.height - size.height) / 2.0,
        ),
        CGSize::new(rect.size.width, size.height),
    );
    string.drawInRect(centred);
}

/// Draw text at a point, with no vertical centring — for stacked lines the
/// caller is positioning itself.
pub fn draw_at(text: &str, at: CGPoint, width: f64, style: &Style) {
    if text.is_empty() {
        return;
    }
    let string = attributed(text, style);
    let size = { string.size() };
    string.drawInRect(CGRect::new(at, CGSize::new(width, size.height)));
}

/// How wide and tall a run of text will be.
pub fn measure(text: &str, style: &Style) -> CGSize {
    if text.is_empty() {
        return CGSize::new(0.0, 0.0);
    }
    attributed(text, style).size()
}
