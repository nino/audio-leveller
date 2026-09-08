//! The Aqua look, drawn with Core Graphics.
//!
//! Everything here is a translation of the stylesheet the Electron renderer
//! painted, so the two apps in this project keep reading as one piece of
//! software and neither drifts.
//!
//! The palette is platform-independent and always compiled, so the colours and
//! their relationships can be tested anywhere. The drawing and the views are
//! macOS only.

pub mod palette;

#[cfg(target_os = "macos")]
pub mod chrome;
#[cfg(target_os = "macos")]
pub mod paint;
#[cfg(target_os = "macos")]
pub mod render;
#[cfg(target_os = "macos")]
pub mod text;

pub use palette::{Colour, Focus, Gel, Light, Stop};
