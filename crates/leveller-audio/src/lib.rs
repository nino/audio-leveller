//! Gapless playback across a set of clips, with one shared playhead.
//!
//! The mixer is where the behaviour is, and it has no audio device in it — a
//! real-time callback that can only be exercised by playing sound is a callback
//! nobody checks. The device wraps it.

pub mod mixer;
pub mod player;

pub use mixer::{Clip, Mixer, Shared};
pub use player::{Player, PlayerError};
