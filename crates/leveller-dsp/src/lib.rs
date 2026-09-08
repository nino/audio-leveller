//! The signal processing behind Audio Leveller.
//!
//! No file IO, no platform, no UI — just the maths, so it compiles and runs
//! anywhere and can be tested without a disk or a window.

pub mod biquad;
pub mod fft;
pub mod signal;

pub use biquad::{Biquad, apply_cascade, cascade_magnitude_db, kweighting};
pub use fft::{FftPlan, RealFftPlan, hann_window, power_spectrum};
pub use signal::{Signal, from_db, to_db};
