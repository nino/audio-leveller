//! The macOS window frame.
//!
//! The window is a real titled `NSWindow`, so the system still draws the
//! shadow, the rounded corners and the resize edges, with its title bar made
//! transparent by `titleBarStyle: "Overlay"`. What the configuration cannot do
//! is hide the system's traffic lights, and the page paints gel ones of its own
//! in the same place. This hides the system's, as the AppKit shell did.

use objc2_app_kit::{NSWindow, NSWindowButton};

pub fn hide_system_buttons(window: &tauri::WebviewWindow) {
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    // SAFETY: Tauri hands back the `NSWindow *` it owns, which lives as long as
    // the window does, and setup runs on the main thread.
    let ns_window: &NSWindow = unsafe { &*ns_window.cast::<NSWindow>() };
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = ns_window.standardWindowButton(kind) {
            button.setHidden(true);
        }
    }
}
