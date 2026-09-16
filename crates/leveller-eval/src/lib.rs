//! The evaluation harness: a synthetic corpus, objective metrics, and bounds
//! with reasons.
//!
//! What it is for: run it, note the numbers, build a stage, run it again
//! against the saved baseline and see exactly what moved. "Sounds better" is
//! not evidence; a click residual that fell 45 dB while SI-SDR held steady is.

pub mod cases;
pub mod metrics;
pub mod runner;

pub use cases::{CaseInput, EvalCase, Expectation};
pub use metrics::{Inputs, Metrics, References};
pub use runner::{Baseline, CaseResult, Check, EvalError, fixture_cases, run_all};
