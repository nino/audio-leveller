//! Sparkle, for "Check for Updates…" and the checks it makes on its own.
//!
//! The framework is loaded at run time from the bundle's `Frameworks` folder
//! rather than linked, so `cargo build`, `cargo test` and `cargo run` need no
//! copy of it. Only a bundle that `cargo xtask bundle` was given a key for has
//! the framework and the feed in it; anything else — a dev build, a bundle made
//! without the key — comes up with no updater and no menu item for one.

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{MainThreadMarker, msg_send};
use objc2_foundation::{NSBundle, NSString, ns_string};

/// A started `SPUStandardUpdaterController`. A menu item holds its target
/// weakly, so this has to outlive the menu — in practice, the run loop.
pub struct Updater {
    controller: Retained<AnyObject>,
}

impl Updater {
    /// What "Check for Updates…" targets with `checkForUpdates:`.
    pub fn controller(&self) -> &AnyObject {
        &self.controller
    }
}

/// Load Sparkle and start it, or `None` if this bundle was built without it.
///
/// Starting it is all an app has to do: Sparkle asks on the second launch
/// whether to check automatically, and from then on checks on its own.
pub fn start(_mtm: MainThreadMarker) -> Option<Updater> {
    let main = NSBundle::mainBundle();
    // A framework without a key would refuse to start and say so in an alert,
    // so the key is the test for whether this bundle is meant to update.
    main.objectForInfoDictionaryKey(ns_string!("SUPublicEDKey"))?;

    let frameworks = main.privateFrameworksPath()?;
    let path = NSString::from_str(&format!("{frameworks}/Sparkle.framework"));
    let sparkle = NSBundle::bundleWithPath(&path)?;
    // SAFETY: Sparkle's own framework, signed with the app's identity and
    // checked by the system's library validation as it loads.
    if !unsafe { sparkle.load() } {
        return None;
    }

    let class = AnyClass::get(c"SPUStandardUpdaterController")?;
    // SAFETY: the designated initialiser from `SPUStandardUpdaterController.h`.
    // Both delegates are optional, and passing none gives the standard
    // behaviour and interface.
    let controller: Option<Retained<AnyObject>> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![
            allocated,
            initWithStartingUpdater: Bool::YES,
            updaterDelegate: None::<&AnyObject>,
            userDriverDelegate: None::<&AnyObject>
        ]
    };
    controller.map(|controller| Updater { controller })
}
