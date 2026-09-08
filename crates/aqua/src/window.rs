//! The window, and the menu that gives it its keyboard shortcuts.
//!
//! A real titled `NSWindow` with its title bar made transparent and its three
//! standard buttons hidden, so the strip and the lights can be painted. Not a
//! borderless window: the system keeps drawing the shadow, the corner mask, the
//! resize edges, the Mission Control thumbnail and the window's accessibility
//! element, all of which a borderless window would have to reimplement badly.
//!
//! This is what the Electron version did too, so the look is known to come out
//! right.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSMenu, NSMenuItem,
    NSWindow, NSWindowButton, NSWindowDelegate, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, ns_string};

use crate::palette;

/// How a window is set up.
pub struct WindowDesc {
    pub title: String,
    pub size: (f64, f64),
    pub min_size: (f64, f64),
}

/// Make the window: titled, resizable, with the title bar transparent and the
/// standard buttons hidden.
pub fn make_window(mtm: MainThreadMarker, desc: &WindowDesc) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;

    let frame = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(desc.size.0, desc.size.1),
    );
    // SAFETY: the standard designated initialiser, with a style mask it accepts.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    window.setTitle(&NSString::from_str(&desc.title));
    // Hidden from the strip, but still set — VoiceOver and the Dock read it.
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    window.setMinSize(CGSize::new(desc.min_size.0, desc.min_size.1));

    // The metal the content view paints, so a slow first frame and a live
    // resize both show the same colour rather than a flash of white.
    let ground = palette::metal(palette::Focus::Active)[1].1;
    let ground = NSColor::colorWithSRGBRed_green_blue_alpha(ground.r, ground.g, ground.b, 1.0);
    window.setBackgroundColor(Some(&ground));

    // The system's buttons go, and ours are painted in their place. They are
    // hidden rather than removed, so `performClose:` and its siblings — which
    // the menu items send — still behave exactly as they always did.
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }

    // A 10.2 window does not go full screen; zoom is the whole vocabulary. This
    // also keeps the painted strip from being replaced by a system one halfway
    // through a transition.
    window.setCollectionBehavior(objc2_app_kit::NSWindowCollectionBehavior::FullScreenNone);
    // And it does not tab, which would put a system tab bar above the strip.
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);

    window.center();
    window
}

/// What the painted lights do when clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    Close,
    Minimise,
    /// A toggle, as the green button has always been.
    Zoom,
}

impl WindowCommand {
    pub fn perform(self, window: &NSWindow) {
        match self {
            // The `perform` family rather than the direct one, so a delegate's
            // `windowShouldClose:` still gets its say and the window still
            // flashes its button the way the system does.
            Self::Close => window.performClose(None),
            Self::Minimise => window.performMiniaturize(None),
            Self::Zoom => window.performZoom(None),
        }
    }
}

impl From<palette::Light> for WindowCommand {
    fn from(light: palette::Light) -> Self {
        match light {
            palette::Light::Close => Self::Close,
            palette::Light::Minimise => Self::Minimise,
            palette::Light::Zoom => Self::Zoom,
        }
    }
}

/// Build the menu bar.
///
/// The hidden buttons carry no shortcuts, so ⌘W, ⌘M and ⌘Q have to come from
/// here — as they would in any AppKit app. The Edit menu is what makes Cut,
/// Copy, Paste and Select All work in a text field, since those are sent up the
/// responder chain by the menu rather than handled by the field itself.
pub fn install_menu(mtm: MainThreadMarker, app_name: &str) {
    let app = NSApplication::sharedApplication(mtm);
    let main = NSMenu::new(mtm);

    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    add(
        mtm,
        &app_menu,
        &format!("About {app_name}"),
        "orderFrontStandardAboutPanel:",
        "",
    );
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &app_menu, &format!("Hide {app_name}"), "hide:", "h");
    add(mtm, &app_menu, "Hide Others", "hideOtherApplications:", "");
    add(mtm, &app_menu, "Show All", "unhideAllApplications:", "");
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(
        mtm,
        &app_menu,
        &format!("Quit {app_name}"),
        "terminate:",
        "q",
    );
    app_item.setSubmenu(Some(&app_menu));
    main.addItem(&app_item);

    let edit_item = NSMenuItem::new(mtm);
    let edit_menu = NSMenu::new(mtm);
    edit_menu.setTitle(ns_string!("Edit"));
    add(mtm, &edit_menu, "Undo", "undo:", "z");
    add(mtm, &edit_menu, "Redo", "redo:", "Z");
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &edit_menu, "Cut", "cut:", "x");
    add(mtm, &edit_menu, "Copy", "copy:", "c");
    add(mtm, &edit_menu, "Paste", "paste:", "v");
    add(mtm, &edit_menu, "Select All", "selectAll:", "a");
    edit_item.setSubmenu(Some(&edit_menu));
    main.addItem(&edit_item);

    let window_item = NSMenuItem::new(mtm);
    let window_menu = NSMenu::new(mtm);
    window_menu.setTitle(ns_string!("Window"));
    add(mtm, &window_menu, "Minimise", "performMiniaturize:", "m");
    add(mtm, &window_menu, "Zoom", "performZoom:", "");
    window_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &window_menu, "Close", "performClose:", "w");
    window_item.setSubmenu(Some(&window_menu));
    main.addItem(&window_item);

    app.setMainMenu(Some(&main));
    app.setWindowsMenu(Some(&window_menu));
}

fn add(mtm: MainThreadMarker, menu: &NSMenu, title: &str, selector: &str, key: &str) {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    // Registered by name rather than written as `sel!(...)`, because these are
    // data in a table. Every one is a documented AppKit action, and a menu item
    // sends it up the responder chain rather than to a particular object — so
    // the window and the text field each answer the ones that are theirs.
    let name = std::ffi::CString::new(selector).expect("a selector has no interior nul");
    unsafe {
        item.setAction(Some(objc2::runtime::Sel::register(&name)));
    }
    item.setKeyEquivalent(&NSString::from_str(key));
    menu.addItem(&item);
}

/// Bring the app up as a regular, dock-visible application and run it.
pub fn run(mtm: MainThreadMarker, delegate: &ProtocolObject<dyn NSWindowDelegate>) -> ! {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let _ = delegate;
    app.activate();
    app.run();
    unreachable!("the run loop does not return")
}
