//! The model as a denoise backend.
//!
//! Everything above this line is the model; this is the adapter that lets the
//! denoise stage choose it. The stage keeps the decisions that are the stage's
//! — how much reduction to ask for, whether the result cost too much programme
//! loudness to keep — and this only answers "can you run, and what do you make
//! of these samples".

use std::sync::{Arc, Mutex, OnceLock};

use leveller_dsp::FftPlan;
use leveller_stages::backend::{DenoiseBackend, DenoiseRequest, DenoiseResponse};
use serde_json::json;

use crate::deepfilternet::{
    ChunkOptions, Config, DFN3, analyse, apply_model, compute_features, erb_widths, frame_count,
    run_chunked, synthesise,
};
use crate::registry::{MODELS, ModelSpec, model_for, verify_model};
use crate::session::Session;

/// DeepFilterNet3, through tract.
pub struct Model {
    config: Config,
    chunk: ChunkOptions,
    /// Built on first use and kept: loading is several seconds of graph
    /// optimisation, and a render of a stereo file would otherwise pay it
    /// twice.
    session: OnceLock<Arc<Mutex<Session>>>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            config: DFN3,
            chunk: ChunkOptions::default(),
            session: OnceLock::new(),
        }
    }

    fn spec(&self, sample_rate: u32) -> Option<&'static ModelSpec> {
        model_for(sample_rate)
    }

    fn session(&self, spec: &ModelSpec) -> Result<Arc<Mutex<Session>>, String> {
        if let Some(session) = self.session.get() {
            return Ok(session.clone());
        }
        let session = Arc::new(Mutex::new(
            Session::load(spec, self.config).map_err(|e| e.to_string())?,
        ));
        // A race here means two sessions were built and one is dropped, which
        // costs time and nothing else.
        let _ = self.session.set(session.clone());
        Ok(self.session.get().cloned().unwrap_or(session))
    }
}

impl DenoiseBackend for Model {
    fn name(&self) -> &'static str {
        "onnx"
    }

    fn description(&self) -> &'static str {
        "DeepFilterNet3 through tract (needs weights: cargo xtask fetch-model)"
    }

    /// Higher than the stage's default, because this backend is measurably
    /// gentler on material that barely needs it.
    ///
    /// The 35 dB default was derived from the classical suppressor, which has
    /// no idea what speech is and reshapes it whether or not there was anything
    /// to remove. This model decides frame by frame: on a real recording
    /// measuring 38.5 dB it leaves 54% of frames untouched outright, and
    /// forcing it to run costs 0.00 dB of programme loudness and returns
    /// 42.8 dB SI-SDR against the original — where the classical backend on the
    /// same material returns 28.4. At 45 dB a recording like that gets 6.5 dB
    /// of reduction rather than none, and a genuinely pristine one still gets
    /// nothing.
    ///
    /// Raising it is only safe because the stage discards a backend's output
    /// when it costs too much programme loudness: on synthetic speech, which
    /// this model misreads, the taper no longer hides the problem and the guard
    /// is what catches it instead.
    fn clean_snr_db(&self) -> Option<f64> {
        Some(45.0)
    }

    fn unavailable_reason(&self, sample_rate: u32) -> Option<String> {
        let Some(spec) = self.spec(sample_rate) else {
            // The model is trained at one rate, and resampling into and out of
            // it inside the stage would spend two conversions to reach a
            // denoiser that is not obviously better than the classical one
            // needing neither. So a 44.1 kHz file gets the spectral backend and
            // says so.
            return Some(format!("no registered model runs at {sample_rate} Hz"));
        };
        verify_model(spec).err()
    }

    fn process(&self, request: &DenoiseRequest) -> DenoiseResponse {
        match self.enhance(request) {
            Ok(response) => response,
            // The trait cannot fail, and the stage has already asked whether
            // this backend was available — so anything reaching here is a
            // surprise. Returning the input unchanged with the reason in the
            // report is better than a panic in the middle of somebody's render.
            Err(reason) => DenoiseResponse {
                channels: request.channels.to_vec(),
                info: json!({ "model": "deepfilternet3", "failed": reason }),
            },
        }
    }
}

impl Model {
    fn enhance(&self, request: &DenoiseRequest) -> Result<DenoiseResponse, String> {
        let spec = self
            .spec(request.sample_rate)
            .ok_or_else(|| format!("no model for {} Hz", request.sample_rate))?;
        let session = self.session(spec)?;
        let session = session.lock().map_err(|_| "the session is poisoned")?;

        let config = self.config;
        let widths = erb_widths(&config);
        let mut plan = FftPlan::new(config.fft_size);
        let length = request.channels.first().map_or(0, Vec::len);

        // Frames covering the signal, plus `lookahead` more so the last frames
        // get a model output at all: the estimate for frame t is emitted at
        // t + 2.
        let covering = frame_count(length, config.hop_size);
        let total = covering + config.lookahead;

        let mut channels = Vec::with_capacity(request.channels.len());
        let mut applied = crate::deepfilternet::Applied::default();
        let mut lsnr_sum = 0.0f64;
        let mut lsnr_count = 0usize;

        for channel in request.channels {
            let spectrogram = analyse(channel, total, &config, &mut plan);
            let features = compute_features(&spectrogram, &config, &widths);
            let model = run_chunked(
                &features,
                &config,
                &self.chunk,
                |erb, spec, frames| session.run_span(erb, spec, frames),
                |_| {},
            )
            .map_err(|e| e.to_string())?;

            let (enhanced, counts) = apply_model(
                &spectrogram[..covering].to_vec(),
                &model,
                &config,
                &widths,
                request.reduction_db,
            );
            channels.push(synthesise(&enhanced, length, &config, &mut plan));

            applied.frames_gained += counts.frames_gained;
            applied.frames_deep_filtered += counts.frames_deep_filtered;
            applied.frames_left_alone += counts.frames_left_alone;
            lsnr_sum += model.lsnr.iter().map(|v| f64::from(*v)).sum::<f64>();
            lsnr_count += model.lsnr.len();
        }

        Ok(DenoiseResponse {
            channels,
            info: json!({
                "model": spec.id,
                "frames": covering,
                "framesGained": applied.frames_gained,
                "framesDeepFiltered": applied.frames_deep_filtered,
                "framesLeftAlone": applied.frames_left_alone,
                "attenuationLimitDb": request.reduction_db,
                "meanLsnrDb": if lsnr_count > 0 {
                    Some(lsnr_sum / lsnr_count as f64)
                } else {
                    None
                },
            }),
        })
    }
}

/// The backends a build with this crate has, in preference order.
///
/// The model wins where its weights are present and verify, and otherwise the
/// classical suppressor runs and the report says why.
pub fn backends() -> leveller_stages::backend::Backends {
    let mut backends = leveller_stages::backend::Backends::new();
    backends.register(Arc::new(Model::new()));
    backends.register(Arc::new(leveller_stages::Spectral));
    backends
}

/// A registry whose denoise stage can reach the model.
pub fn registry() -> leveller_pipeline::Registry {
    leveller_stages::registry(backends())
}

/// Whether the weights are installed and verify, for a caller that wants to
/// say so before starting a render.
pub fn weights_installed() -> bool {
    MODELS.iter().any(|spec| verify_model(spec).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_the_model_was_not_trained_for_is_declined_with_a_reason() {
        // Not an error: the stage falls back to the classical suppressor, and
        // the report says which one ran and why.
        let model = Model::new();
        let reason = model.unavailable_reason(44_100).expect("44.1 kHz declined");
        assert!(reason.contains("44100 Hz"), "{reason}");
    }

    #[test]
    fn the_backend_names_itself_the_way_the_preference_order_expects() {
        // `default_preference` in the stage crate asks for "onnx" by name, and
        // a rename here would silently mean the model is never chosen.
        assert_eq!(Model::new().name(), "onnx");
        assert!(
            leveller_stages::backend::default_preference()
                .iter()
                .any(|name| name == "onnx"),
            "the stage no longer prefers this backend by that name"
        );
    }

    #[test]
    fn the_model_is_preferred_over_the_classical_suppressor() {
        let backends = backends();
        assert_eq!(backends.names(), vec!["onnx", "spectral"]);
    }

    #[test]
    fn the_clean_source_threshold_is_the_backends_rather_than_the_stages() {
        // The stage's 35 dB was derived from the classical suppressor. This
        // model is gentler on material that barely needs it, and says so.
        assert_eq!(Model::new().clean_snr_db(), Some(45.0));
    }

    #[test]
    fn missing_weights_are_reported_rather_than_thrown() {
        // Pointed at an empty directory, the backend must decline politely —
        // this is the normal state of a fresh checkout.
        let scratch = std::env::temp_dir().join(format!("leveller-model-empty-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        // SAFETY: single-threaded test; the variable is read and restored here.
        let previous = std::env::var("AUDIO_LEVELLER_MODELS").ok();
        unsafe { std::env::set_var("AUDIO_LEVELLER_MODELS", &scratch) };

        let reason = Model::new().unavailable_reason(48_000);

        match previous {
            Some(value) => unsafe { std::env::set_var("AUDIO_LEVELLER_MODELS", value) },
            None => unsafe { std::env::remove_var("AUDIO_LEVELLER_MODELS") },
        }
        let _ = std::fs::remove_dir_all(&scratch);

        let reason = reason.expect("no weights means unavailable");
        assert!(reason.contains("not found at"), "{reason}");
    }
}
