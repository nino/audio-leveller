//! The signal processing behind Audio Leveller.
//!
//! No file IO, no platform, no UI — just the maths, so it compiles and runs
//! anywhere and can be tested without a disk or a window.

pub mod biquad;
pub mod convolve;
pub mod declick;
pub mod denoise;
pub mod dereverb;
pub mod dynamics;
pub mod dyneq;
pub mod eq;
pub mod fft;
pub mod leveller;
pub mod loudness;
pub mod lpc;
pub mod ltas;
pub mod resample;
pub mod reverbtime;
pub mod roomtone;
pub mod signal;
pub mod silence;
pub mod stft;
pub mod truepeak;

pub use biquad::{Biquad, apply_cascade, cascade_magnitude_db, kweighting};
pub use convolve::convolve;
pub use declick::{DeclickOptions, declick};
pub use denoise::{DenoiseOptions, NoiseProfile, denoise};
pub use dereverb::{DereverbOptions, dereverb};
pub use dynamics::{Compressor, Expander, Timing};
pub use dyneq::{DynEqOptions, dynamic_eq};
pub use eq::{EqBand, EqFit, EqFitOptions, build_cascade, decide_rumble, fit_corrective};
pub use fft::{FftPlan, RealFftPlan, hann_window, power_spectrum};
pub use leveller::{LevellerOptions, LevellerResult, level};
pub use loudness::Weighted;
pub use lpc::{ArModel, interpolate_gap};
pub use ltas::{Ltas, LtasOptions};
pub use resample::{resample, resampled_length};
pub use reverbtime::reverb_decay;
pub use roomtone::{RoomTone, RoomToneOptions};
pub use signal::{Signal, from_db, to_db};
pub use silence::{SilenceAnalysis, SilenceOptions, SilenceRegion};
pub use stft::{Stft, apply_gains, magnitudes};
pub use truepeak::{true_peak_dbfs, true_peak_envelope, true_peak_linear};
