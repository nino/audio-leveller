//! The signal processing behind Audio Leveller.
//!
//! No file IO, no platform, no UI — just the maths, so it compiles and runs
//! anywhere and can be tested without a disk or a window.

pub mod biquad;
pub mod fft;
pub mod loudness;
pub mod resample;
pub mod signal;
pub mod silence;
pub mod stft;
pub mod truepeak;

pub use biquad::{Biquad, apply_cascade, cascade_magnitude_db, kweighting};
pub use fft::{FftPlan, RealFftPlan, hann_window, power_spectrum};
pub use loudness::Weighted;
pub use resample::{resample, resampled_length};
pub use signal::{Signal, from_db, to_db};
pub use silence::{SilenceAnalysis, SilenceOptions, SilenceRegion};
pub use stft::{Stft, apply_gains, magnitudes};
pub use truepeak::{true_peak_dbfs, true_peak_envelope, true_peak_linear};
