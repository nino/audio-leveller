//! The drawing primitives, one per CSS idea.
//!
//! Every function here exists because the stylesheet used the corresponding
//! declaration: `linear-gradient` is [`fill_gradient`], `box-shadow: inset` is
//! [`inner_shadow`], a `border` on a `border-box` element is
//! [`stroke_inside`], and so on. Translating rule by rule is what keeps the
//! result recognisably the same interface rather than a fresh interpretation of
//! it.
//!
//! Coordinates are points and the origin is at the top left, because every
//! view here is flipped — which lets the CSS offsets be copied across without
//! anyone having to remember to negate them.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGColor, CGColorSpace, CGContext, CGGradient, CGGradientDrawingOptions, CGMutablePath, CGPath,
};

use crate::palette::{Colour, Stop};

/// The shape a control is cut to.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// A pill: fully rounded at both ends, as every Aqua button is.
    Capsule,
    /// Rounded to a given radius.
    Rounded(f64),
    Circle,
    Rect,
}

impl Shape {
    /// The path this shape traces around `rect`.
    pub fn path(self, rect: CGRect) -> objc2_core_foundation::CFRetained<CGPath> {
        let radius = match self {
            Self::Capsule | Self::Circle => rect.size.height.min(rect.size.width) / 2.0,
            Self::Rounded(r) => r.min(rect.size.height / 2.0).min(rect.size.width / 2.0),
            Self::Rect => 0.0,
        };
        if radius <= 0.0 {
            // SAFETY: a null transform means the identity, which is what the
            // header documents.
            return unsafe { CGPath::with_rect(rect, std::ptr::null()) };
        }
        // SAFETY: as above; the radii are clamped to half the shorter side, as
        // the function requires.
        unsafe { CGPath::with_rounded_rect(rect, radius, radius, std::ptr::null()) }
    }
}

fn colour_space() -> objc2_core_foundation::CFRetained<CGColorSpace> {
    // Falling back to nothing is not an option a caller could act on, and this
    // has never returned null on any Mac that runs the app.
    CGColorSpace::new_device_rgb().expect("device RGB")
}

fn cg_colour(colour: Colour) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_srgb(colour.r, colour.g, colour.b, colour.a)
}

/// Save the context, run `draw`, and restore it — so a clip or a shadow set
/// inside cannot leak out.
pub fn saving(ctx: &CGContext, draw: impl FnOnce(&CGContext)) {
    CGContext::save_g_state(Some(ctx));
    draw(ctx);
    CGContext::restore_g_state(Some(ctx));
}

/// Clip to a shape and draw inside it.
pub fn clipped(ctx: &CGContext, path: &CGPath, draw: impl FnOnce(&CGContext)) {
    saving(ctx, |ctx| {
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(path));
        CGContext::clip(Some(ctx));
        draw(ctx);
    });
}

/// Fill a shape with a flat colour.
pub fn fill(ctx: &CGContext, path: &CGPath, colour: Colour) {
    if colour.a <= 0.0 {
        return;
    }
    saving(ctx, |ctx| {
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_colour(colour)));
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(path));
        CGContext::fill_path(Some(ctx));
    });
}

/// Fill a rectangle with a flat colour. The 1pt strips and dividers, mostly.
pub fn fill_rect(ctx: &CGContext, rect: CGRect, colour: Colour) {
    if colour.a <= 0.0 {
        return;
    }
    saving(ctx, |ctx| {
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_colour(colour)));
        CGContext::fill_rect(Some(ctx), rect);
    });
}

fn gradient(stops: &[Stop]) -> Option<objc2_core_foundation::CFRetained<CGGradient>> {
    let components: Vec<f64> = stops.iter().flat_map(|(_, c)| c.components()).collect();
    let locations: Vec<f64> = stops.iter().map(|(at, _)| *at).collect();
    // SAFETY: both slices are alive for the call and hold exactly the counts
    // the function is told about — four components per stop, one location each.
    unsafe {
        CGGradient::with_color_components(
            Some(&colour_space()),
            components.as_ptr(),
            locations.as_ptr(),
            stops.len(),
        )
    }
}

/// A `linear-gradient(to bottom, …)`, clipped to a shape.
pub fn fill_gradient(ctx: &CGContext, path: &CGPath, rect: CGRect, stops: &[Stop]) {
    let Some(gradient) = gradient(stops) else {
        return;
    };
    clipped(ctx, path, |ctx| {
        CGContext::draw_linear_gradient(
            Some(ctx),
            Some(&gradient),
            CGPoint::new(rect.origin.x, rect.origin.y),
            CGPoint::new(rect.origin.x, rect.origin.y + rect.size.height),
            // Extend past both ends, so a gradient whose stops do not span the
            // whole shape still fills it rather than leaving a gap.
            CGGradientDrawingOptions::DrawsBeforeStartLocation
                | CGGradientDrawingOptions::DrawsAfterEndLocation,
        );
    });
}

/// A `radial-gradient(circle at x y, …)`, clipped to a shape.
///
/// `centre` is a fraction of the rect, as the CSS gives it. The end radius is
/// the distance to the farthest corner, which is what `farthest-corner`, the
/// CSS default, means.
pub fn fill_radial(
    ctx: &CGContext,
    path: &CGPath,
    rect: CGRect,
    centre: (f64, f64),
    stops: &[Stop],
) {
    let Some(gradient) = gradient(stops) else {
        return;
    };
    let point = CGPoint::new(
        rect.origin.x + rect.size.width * centre.0,
        rect.origin.y + rect.size.height * centre.1,
    );
    let corner = |x: f64, y: f64| ((point.x - x).powi(2) + (point.y - y).powi(2)).sqrt();
    let radius = corner(rect.origin.x, rect.origin.y)
        .max(corner(rect.origin.x + rect.size.width, rect.origin.y))
        .max(corner(rect.origin.x, rect.origin.y + rect.size.height))
        .max(corner(
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ));

    clipped(ctx, path, |ctx| {
        CGContext::draw_radial_gradient(
            Some(ctx),
            Some(&gradient),
            point,
            0.0,
            point,
            radius,
            CGGradientDrawingOptions::DrawsAfterEndLocation,
        );
    });
}

/// A CSS `border` on a `box-sizing: border-box` element: a stroke that sits
/// *inside* the shape rather than straddling its edge.
///
/// Done by clipping to the path and stroking at twice the width, so exactly
/// half of it survives. Stroking at the nominal width instead would put half a
/// point outside the shape, which on a control sitting against a panel edge is
/// visible as a soft double line.
pub fn stroke_inside(ctx: &CGContext, path: &CGPath, width: f64, colour: Colour) {
    if colour.a <= 0.0 || width <= 0.0 {
        return;
    }
    clipped(ctx, path, |ctx| {
        CGContext::set_stroke_color_with_color(Some(ctx), Some(&cg_colour(colour)));
        CGContext::set_line_width(Some(ctx), width * 2.0);
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(path));
        CGContext::stroke_path(Some(ctx));
    });
}

/// A dashed border, for the drop zone.
pub fn stroke_dashed(ctx: &CGContext, path: &CGPath, width: f64, dash: &[f64], colour: Colour) {
    if colour.a <= 0.0 || width <= 0.0 {
        return;
    }
    clipped(ctx, path, |ctx| {
        CGContext::set_stroke_color_with_color(Some(ctx), Some(&cg_colour(colour)));
        CGContext::set_line_width(Some(ctx), width * 2.0);
        // SAFETY: the slice outlives the call and its length is passed with it.
        unsafe {
            CGContext::set_line_dash(Some(ctx), 0.0, dash.as_ptr(), dash.len());
        }
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(path));
        CGContext::stroke_path(Some(ctx));
    });
}

/// A `box-shadow: inset`.
///
/// The trick: clip to the shape, then fill *everything outside* it with an
/// opaque colour that casts a shadow. The fill itself lands outside the clip
/// and is never seen; only its shadow, which falls inward, survives. That is
/// the standard way to get an inner shadow out of a compositor that only knows
/// how to cast outer ones.
pub fn inner_shadow(ctx: &CGContext, path: &CGPath, offset: (f64, f64), blur: f64, colour: Colour) {
    if colour.a <= 0.0 {
        return;
    }
    let bounds = CGPath::bounding_box(Some(path));
    let margin = blur.max(offset.0.abs()).max(offset.1.abs()) * 4.0 + 8.0;
    let outer = CGRect::new(
        CGPoint::new(bounds.origin.x - margin, bounds.origin.y - margin),
        CGSize::new(
            bounds.size.width + margin * 2.0,
            bounds.size.height + margin * 2.0,
        ),
    );

    clipped(ctx, path, |ctx| {
        // SAFETY: `set_shadow_with_color` takes an optional colour and a
        // context; both are valid here.
        {
            CGContext::set_shadow_with_color(
                Some(ctx),
                CGSize::new(offset.0, offset.1),
                blur,
                Some(&cg_colour(colour)),
            );
        }
        // The even-odd rule turns "the outer rectangle" and "the shape" into
        // "everything between them", which is the region whose shadow falls
        // inward.
        let ring = CGMutablePath::new();
        // SAFETY: null transform is the identity.
        unsafe {
            CGMutablePath::add_rect(Some(&ring), std::ptr::null(), outer);
            CGMutablePath::add_path(Some(&ring), std::ptr::null(), Some(path));
        }
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_colour(Colour::black(1.0))));
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(&ring));
        CGContext::eo_fill_path(Some(ctx));
    });
}

/// The cheap case of `box-shadow: inset 0 1px 0 white`: a 1pt strip along the
/// top of the shape.
///
/// Every gel control has one, and running the full inner-shadow machinery for a
/// hard-edged single-pixel line would be wasteful — this is a clipped fill.
pub fn inner_highlight_top(ctx: &CGContext, path: &CGPath, colour: Colour) {
    if colour.a <= 0.0 {
        return;
    }
    let bounds = CGPath::bounding_box(Some(path));
    clipped(ctx, path, |ctx| {
        fill_rect(
            ctx,
            CGRect::new(bounds.origin, CGSize::new(bounds.size.width, 1.0)),
            colour,
        );
    });
}

/// An outer `box-shadow`, drawn behind whatever is filled next.
pub fn drop_shadow(ctx: &CGContext, path: &CGPath, offset: (f64, f64), blur: f64, colour: Colour) {
    if colour.a <= 0.0 {
        return;
    }
    saving(ctx, |ctx| {
        // SAFETY: both arguments are valid for the call.
        {
            CGContext::set_shadow_with_color(
                Some(ctx),
                CGSize::new(offset.0, offset.1),
                blur,
                Some(&cg_colour(colour)),
            );
        }
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_colour(Colour::black(1.0))));
        CGContext::begin_path(Some(ctx));
        CGContext::add_path(Some(ctx), Some(path));
        CGContext::fill_path(Some(ctx));
    });
}

/// Draw at reduced opacity — CSS `opacity`, which is how a disabled control is
/// dimmed.
pub fn with_alpha(ctx: &CGContext, alpha: f64, draw: impl FnOnce(&CGContext)) {
    if alpha >= 1.0 {
        return draw(ctx);
    }
    saving(ctx, |ctx| {
        CGContext::set_alpha(Some(ctx), alpha);
        draw(ctx);
    });
}

/// Fill an arbitrary polygon, for the progress bar's diagonal stripes.
pub fn fill_polygon(ctx: &CGContext, points: &[CGPoint], colour: Colour) {
    if colour.a <= 0.0 || points.len() < 3 {
        return;
    }
    saving(ctx, |ctx| {
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_colour(colour)));
        CGContext::begin_path(Some(ctx));
        CGContext::move_to_point(Some(ctx), points[0].x, points[0].y);
        for point in &points[1..] {
            CGContext::add_line_to_point(Some(ctx), point.x, point.y);
        }
        CGContext::close_path(Some(ctx));
        CGContext::fill_path(Some(ctx));
    });
}

/// A rectangle from a position and a size, since every call site would
/// otherwise be four nested constructors.
pub fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}

/// Shrink a rectangle by `by` on every side.
pub fn inset(rect: CGRect, by: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(rect.origin.x + by, rect.origin.y + by),
        CGSize::new(
            (rect.size.width - by * 2.0).max(0.0),
            (rect.size.height - by * 2.0).max(0.0),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capsule_is_rounded_to_half_its_height() {
        let bounds = |shape: Shape, r: CGRect| CGPath::bounding_box(Some(&shape.path(r)));
        let r = rect(0.0, 0.0, 100.0, 24.0);
        // The bounding box is the rectangle whatever the corners do; what is
        // being checked is that a path comes back at all and covers the rect.
        let box_of = bounds(Shape::Capsule, r);
        assert!((box_of.size.width - 100.0).abs() < 0.01);
        assert!((box_of.size.height - 24.0).abs() < 0.01);
    }

    #[test]
    fn a_radius_larger_than_the_shape_is_clamped_rather_than_refused() {
        // CGPath rejects a radius past half the shorter side outright, so the
        // clamp is what stops a 12pt radius on an 8pt-tall control crashing.
        let path = Shape::Rounded(40.0).path(rect(0.0, 0.0, 20.0, 8.0));
        let bounds = CGPath::bounding_box(Some(&path));
        assert!((bounds.size.height - 8.0).abs() < 0.01);
    }

    #[test]
    fn a_zero_radius_gives_a_plain_rectangle() {
        let path = Shape::Rect.path(rect(5.0, 6.0, 20.0, 8.0));
        let bounds = CGPath::bounding_box(Some(&path));
        assert!((bounds.origin.x - 5.0).abs() < 0.01);
        assert!((bounds.size.width - 20.0).abs() < 0.01);
    }

    #[test]
    fn insetting_never_produces_a_negative_size() {
        let squashed = inset(rect(0.0, 0.0, 4.0, 4.0), 10.0);
        assert_eq!(squashed.size.width, 0.0);
        assert_eq!(squashed.size.height, 0.0);
        // And an ordinary inset does what it says.
        let ordinary = inset(rect(0.0, 0.0, 20.0, 10.0), 2.0);
        assert_eq!(ordinary.origin.x, 2.0);
        assert_eq!(ordinary.size.width, 16.0);
    }

    #[test]
    fn a_gradient_is_built_from_its_stops() {
        assert!(gradient(&[(0.0, Colour::white(1.0)), (1.0, Colour::black(1.0))]).is_some());
        assert!(
            gradient(&[]).is_none(),
            "an empty gradient should be nothing, not a crash"
        );
    }
}
