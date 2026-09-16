//! Drawing into a bitmap instead of onto the screen, and writing it out as a
//! PNG.
//!
//! This exists because a screenshot of a running app needs the screen-recording
//! permission, and a development machine that has not granted it — a CI runner,
//! or a shell — then cannot see what it built. Rendering the same drawing code
//! into a bitmap needs no permission at all, needs no window, and produces the
//! same pixels, because it *is* the same code.
//!
//! It has turned out to be the more useful direction anyway: the whole
//! interface can be laid out and looked at in one image, at any size, in any
//! state, without clicking through to reach it.

use std::path::Path;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSGraphicsContext};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSDictionary, NSString};

/// Draw into a bitmap of `width` by `height` points at `scale` pixels per
/// point, and write it to `path` as a PNG.
///
/// The drawing runs with the bitmap installed as the current context and the
/// coordinate system flipped, so it sees exactly what a flipped view sees.
///
/// # Panics
/// If the bitmap cannot be created, which on a machine that has AppKit at all
/// means the size was absurd.
pub fn to_png(
    path: &Path,
    width: f64,
    height: f64,
    scale: f64,
    draw: impl FnOnce(&CGContext, CGRect),
) -> Result<(), std::io::Error> {
    let pixels_wide = (width * scale).round() as isize;
    let pixels_high = (height * scale).round() as isize;

    // SAFETY: a null plane pointer asks the class to allocate its own storage,
    // which is what the header documents for this initialiser.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pixels_wide,
            pixels_high,
            8,
            4,
            true,
            false,
            objc2_app_kit::NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .expect("a bitmap of this size");

    // The representation counts in pixels; the drawing works in points. Telling
    // it its own size in points is what makes `scale` mean "draw at Retina" and
    // not "draw the same thing bigger".
    rep.setSize(CGSize::new(width, height));

    let Some(context) = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep) else {
        return Err(std::io::Error::other(
            "could not make a drawing context for the bitmap",
        ));
    };

    // AppKit's bitmap contexts have their origin at the bottom left. Flipping
    // here means every drawing function can take y as growing downward, exactly
    // as the CSS did.
    let cg = context.CGContext();
    CGContext::translate_ctm(Some(&cg), 0.0, height);
    CGContext::scale_ctm(Some(&cg), 1.0, -1.0);

    // And the *context* has to be told it is flipped, not only its transform.
    // Core Graphics drawing does not care either way, but AppKit's text layout
    // reads `isFlipped` to decide which way up to render a glyph — so flipping
    // the transform alone produces a perfectly correct interface with every
    // word upside down.
    let flipped = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);

    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&flipped));

    draw(
        &cg,
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height)),
    );

    flipped.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    write_png(&rep, path)
}

fn write_png(rep: &Retained<NSBitmapImageRep>, path: &Path) -> Result<(), std::io::Error> {
    let properties: Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> =
        NSDictionary::new();
    let Some(data) = (unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }) else {
        return Err(std::io::Error::other(
            "could not encode the bitmap as a PNG",
        ));
    };

    std::fs::write(path, data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome;
    use crate::palette::Focus;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aqua-render-{name}-{}.png", std::process::id()))
    }

    #[test]
    fn a_drawing_becomes_a_png_on_disk() {
        let path = scratch("basic");
        to_png(&path, 120.0, 40.0, 2.0, |ctx, rect| {
            chrome::metal(ctx, rect, Focus::Active);
        })
        .expect("writing the png");

        let bytes = std::fs::read(&path).expect("reading it back");
        assert!(
            bytes.len() > 100,
            "suspiciously small: {} bytes",
            bytes.len()
        );
        assert_eq!(&bytes[1..4], b"PNG", "not a PNG");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_scale_reaches_the_pixels_and_not_the_layout() {
        // Twice the scale is four times the pixels of the same drawing, which
        // is what "draw at Retina" has to mean.
        let one = scratch("scale-1");
        let two = scratch("scale-2");
        for (path, scale) in [(&one, 1.0), (&two, 2.0)] {
            to_png(path, 100.0, 50.0, scale, |ctx, rect| {
                chrome::metal(ctx, rect, Focus::Active);
            })
            .expect("writing");
        }
        let size = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();
        assert!(size(&two) > size(&one));
        let _ = std::fs::remove_file(&one);
        let _ = std::fs::remove_file(&two);
    }

    #[test]
    fn a_path_that_cannot_be_written_is_an_error_rather_than_a_panic() {
        let error = to_png(
            std::path::Path::new("/nowhere/at/all/shot.png"),
            10.0,
            10.0,
            1.0,
            |_, _| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
