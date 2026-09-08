//! The classical backend: spectral suppression, always available.
//!
//! No weights, no native dependency, no download — so this is what runs by
//! default and what every evaluation baseline is measured against.

use leveller_dsp::denoise::{DenoiseOptions, denoise};
use serde_json::json;

use crate::backend::{DenoiseBackend, DenoiseRequest, DenoiseResponse};

pub struct Spectral;

impl DenoiseBackend for Spectral {
    fn name(&self) -> &'static str {
        "spectral"
    }

    fn description(&self) -> &'static str {
        "Wiener suppression with a decision-directed SNR estimate (no model, no download)"
    }

    fn unavailable_reason(&self, _sample_rate: u32) -> Option<String> {
        None
    }

    fn process(&self, request: &DenoiseRequest) -> DenoiseResponse {
        let result = denoise(
            request.channels,
            request.pauses,
            &DenoiseOptions {
                reduction_db: request.reduction_db,
                ..DenoiseOptions::default()
            },
        );

        DenoiseResponse {
            channels: result.channels,
            info: json!({
                "noiseProfileFrames": result.profile.frames,
                // Worth surfacing: a profile from detected pauses is a direct
                // measurement, one from minimum statistics is an inference.
                "noiseProfileFromPauses": result.profile.from_pauses,
                "meanNoiseGainDb": result.mean_noise_gain_db,
                "meanSpeechGainDb": result.mean_speech_gain_db,
            }),
        }
    }
}
