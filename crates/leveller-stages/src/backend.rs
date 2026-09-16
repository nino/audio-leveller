//! Pluggable denoise backends.
//!
//! There are two kinds of noise reduction worth having and they are not
//! interchangeable. A classical spectral suppressor knows nothing about speech;
//! it removes what is *stationary*, which covers hiss, hum, fan and traffic,
//! and it cannot invent detail because it only ever attenuates. A trained model
//! knows what speech looks like and removes non-stationary noise the classical
//! method cannot touch — at the cost of being able to hallucinate plausible
//! speech detail that was never there.
//!
//! So the interface is deliberately narrow and both live behind it: the
//! classical backend is always present and always works, the model backend is
//! better where it is available, and the stage records which one ran. That
//! matters for a local tool — someone without the weights should still get a
//! working denoiser rather than a stage that silently does nothing.

use std::sync::Arc;

use leveller_dsp::denoise::SampleRange;
use serde_json::Value;

pub struct DenoiseRequest<'a> {
    pub channels: &'a [Vec<f32>],
    pub sample_rate: u32,
    /// Ranges believed to hold no speech, from the silence analysis.
    pub pauses: &'a [SampleRange],
    /// How far to push the noise floor down, in dB.
    pub reduction_db: f64,
}

pub struct DenoiseResponse {
    pub channels: Vec<Vec<f32>>,
    /// Backend-specific detail for the report.
    pub info: Value,
}

pub trait DenoiseBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;

    /// Programme-to-floor distance, in dB, above which *this* backend has
    /// nothing left to offer and should be tapered to nothing. `None` falls
    /// back to the stage's own parameter.
    ///
    /// It belongs to the backend rather than the stage because it is a
    /// statement about that backend's transparency, and the two are not equally
    /// transparent. The threshold is the point past which the cure costs more
    /// than the disease, and that point is different for a suppressor with no
    /// idea what speech is than for a model deciding frame by frame whether a
    /// frame is worth touching.
    fn clean_snr_db(&self) -> Option<f64> {
        None
    }

    /// Whether this backend can run right now, and why not when it cannot.
    ///
    /// A backend needing weights is unavailable until they are on disk, and the
    /// reason goes in the report so the fallback is visible rather than silent.
    fn unavailable_reason(&self, sample_rate: u32) -> Option<String>;

    fn process(&self, request: &DenoiseRequest) -> DenoiseResponse;
}

/// The backends a build has, in preference order.
#[derive(Clone, Default)]
pub struct Backends {
    backends: Vec<Arc<dyn DenoiseBackend>>,
}

/// A backend that was preferred but could not run, and why.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

impl Backends {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, backend: Arc<dyn DenoiseBackend>) -> &mut Self {
        self.backends.push(backend);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn DenoiseBackend>> {
        self.backends.iter().find(|b| b.name() == name)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    /// The first backend in `preference` order that can actually run, and what
    /// was skipped on the way.
    pub fn resolve(
        &self,
        preference: &[String],
        sample_rate: u32,
    ) -> (Option<Arc<dyn DenoiseBackend>>, Vec<Skipped>) {
        let mut skipped = Vec::new();

        for name in preference {
            let Some(backend) = self.get(name) else {
                skipped.push(Skipped {
                    name: name.clone(),
                    reason: "not registered in this build".into(),
                });
                continue;
            };
            if let Some(reason) = backend.unavailable_reason(sample_rate) {
                skipped.push(Skipped {
                    name: name.clone(),
                    reason,
                });
                continue;
            }
            return (Some(backend.clone()), skipped);
        }

        (None, skipped)
    }
}

/// Preference order: the model backend wins where its weights are present and
/// verify, and otherwise the classical one runs and the report says why.
pub fn default_preference() -> Vec<String> {
    vec!["onnx".into(), "spectral".into()]
}
