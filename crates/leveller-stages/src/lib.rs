//! The eight stages, and the order they run in.
//!
//! [`DEFAULT_CHAIN`] is that order, and the list encodes the decisions that
//! matter: de-click before the denoiser, because impulses are
//! out-of-distribution for any denoiser and get smeared rather than removed;
//! EQ after it, because denoising changes the spectrum you would otherwise be
//! fitting a curve to; the expander before the compressor, because both set
//! their thresholds from the programme loudness of whatever they are handed and
//! only the expander leaves that number where it found it; and levelling last,
//! so the loudness target is exact and the true-peak limiter sees what the
//! compressor actually produced.

pub mod backend;
pub mod params;
pub mod spectral;
pub mod stages;
pub mod support;

use std::sync::Arc;

use leveller_pipeline::{Registry, StageSpec};

pub use backend::{Backends, DenoiseBackend, DenoiseRequest, DenoiseResponse, Skipped};
pub use params::{ParamSpec, Preset, StageParams, presets, schema};
pub use spectral::Spectral;
pub use stages::{CompressParams, DenoiseParams, DereverbParams, EqParams, ExpandParams, Voicing};

/// The chain, in order.
pub const DEFAULT_CHAIN: [&str; 8] = [
    "declick", "denoise", "dereverb", "eq", "dyneq", "expand", "compress", "level",
];

/// A registry holding the eight stages, with the denoise backends given.
///
/// The backends are a parameter rather than a global because a build without
/// ONNX simply does not have that one, and the difference should be visible at
/// the call site rather than discovered at run time.
pub fn registry(backends: Backends) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(stages::Declick))
        .register(Arc::new(stages::Denoise::new(backends)))
        .register(Arc::new(stages::Dereverb))
        .register(Arc::new(stages::Eq))
        .register(Arc::new(stages::DynEq))
        .register(Arc::new(stages::Expand))
        .register(Arc::new(stages::Compress))
        .register(Arc::new(stages::Level));
    registry
}

/// The backends a plain build has: the classical one, which always works.
pub fn default_backends() -> Backends {
    let mut backends = Backends::new();
    backends.register(Arc::new(Spectral));
    backends
}

/// A registry with the default chain and the default backends.
pub fn default_registry() -> Registry {
    registry(default_backends())
}

#[derive(Clone, Debug, Default)]
pub struct ChainOptions {
    /// Restrict the chain to these stages, keeping the default order.
    pub only: Vec<String>,
    /// Run the chain but bypass these. They still appear in the report.
    pub bypass: Vec<String>,
    /// Parameter overrides, keyed by stage name.
    pub params: Vec<(String, serde_json::Map<String, serde_json::Value>)>,
}

#[derive(Debug, thiserror::Error)]
#[error("unknown stage \"{name}\" (available: {available})")]
pub struct UnknownStage {
    pub name: String,
    pub available: String,
}

/// Build a chain from the default order.
///
/// Unknown stage names are refused here rather than silently ignored, so a typo
/// in `--bypass` is an error instead of a stage that quietly stayed on.
pub fn build_chain(options: &ChainOptions) -> Result<Vec<StageSpec>, UnknownStage> {
    let named = options
        .only
        .iter()
        .chain(&options.bypass)
        .chain(options.params.iter().map(|(name, _)| name));
    for name in named {
        if !DEFAULT_CHAIN.contains(&name.as_str()) {
            return Err(UnknownStage {
                name: name.clone(),
                available: DEFAULT_CHAIN.join(", "),
            });
        }
    }

    Ok(DEFAULT_CHAIN
        .iter()
        .filter(|name| options.only.is_empty() || options.only.iter().any(|o| o == *name))
        .map(|name| {
            let spec = StageSpec::new(*name);
            let spec = if options.bypass.iter().any(|b| b == name) {
                spec.bypassed()
            } else {
                spec
            };
            match options.params.iter().find(|(stage, _)| stage == name) {
                Some((_, params)) => spec.with_params(params.clone()),
                None => spec,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_corpus::{ClickOptions, SpeechOptions, Spurt, add_clicks, add_noise};
    use leveller_dsp::Signal;
    use leveller_pipeline::{Progress, run};
    use serde_json::{Value, json};
    use std::sync::OnceLock;

    const SR: u32 = 48_000;

    fn speech() -> &'static leveller_corpus::Speech {
        static SPEECH: OnceLock<leveller_corpus::Speech> = OnceLock::new();
        SPEECH.get_or_init(|| {
            leveller_corpus::synthetic_speech(&SpeechOptions {
                sample_rate: SR,
                spurts: vec![Spurt::new(4.0, -30.0), Spurt::new(4.0, -18.0)],
                pause_sec: 1.5,
                floor_dbfs: -55.0,
                seed: 4242,
                channels: 1,
            })
        })
    }

    fn signal() -> Arc<Signal> {
        Arc::new(Signal::new(SR, speech().signal.channels().to_vec()))
    }

    fn chain() -> Vec<StageSpec> {
        build_chain(&ChainOptions::default()).unwrap()
    }

    fn stage_report<'a>(report: &'a leveller_pipeline::PipelineReport, name: &str) -> &'a Value {
        &report
            .stages
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no {name} stage in the report"))
            .report
    }

    #[test]
    fn the_default_chain_is_the_documented_order() {
        assert_eq!(
            chain().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            DEFAULT_CHAIN.to_vec()
        );
        assert!(chain().iter().all(|s| s.enabled));
        assert_eq!(default_registry().names(), DEFAULT_CHAIN.to_vec());
    }

    #[test]
    fn only_restricts_the_chain_but_keeps_its_order() {
        let specs = build_chain(&ChainOptions {
            only: vec!["level".into(), "declick".into()],
            ..ChainOptions::default()
        })
        .unwrap();
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["declick", "level"],
            "the order is the chain's, not the caller's"
        );
    }

    #[test]
    fn bypass_disables_a_stage_without_removing_it() {
        let specs = build_chain(&ChainOptions {
            bypass: vec!["denoise".into()],
            ..ChainOptions::default()
        })
        .unwrap();
        assert_eq!(specs.len(), DEFAULT_CHAIN.len());
        let denoise = specs.iter().find(|s| s.name == "denoise").unwrap();
        assert!(!denoise.enabled);
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_stage_that_quietly_stayed_on() {
        for options in [
            ChainOptions {
                only: vec!["delcick".into()],
                ..ChainOptions::default()
            },
            ChainOptions {
                bypass: vec!["delcick".into()],
                ..ChainOptions::default()
            },
            ChainOptions {
                params: vec![("delcick".into(), serde_json::Map::new())],
                ..ChainOptions::default()
            },
        ] {
            let error = build_chain(&options).unwrap_err();
            assert_eq!(error.name, "delcick");
            assert!(error.available.contains("declick"));
        }
    }

    #[test]
    fn the_whole_chain_runs_and_levels_the_file() {
        let input = signal();
        let result = run(&input, &chain(), &default_registry(), |_| {}).unwrap();

        // The point of all eight stages: both segments land on target.
        for segment in &speech().segments {
            let measured = leveller_dsp::loudness::Weighted::new(
                result.signal.channels(),
                result.signal.sample_rate(),
            )
            .integrated_over(segment.start, segment.end);
            assert!(
                (measured - -18.0).abs() < 1.5,
                "segment at {} came out at {measured} LUFS",
                segment.start
            );
        }
        assert_eq!(result.report.stages.len(), 8);
        assert!(result.report.stages.iter().all(|s| s.enabled));
    }

    #[test]
    fn the_room_tone_bed_arrives_as_an_extra() {
        let result = run(&signal(), &chain(), &default_registry(), |_| {}).unwrap();
        assert_eq!(result.report.extras, vec!["roomtone".to_string()]);
        assert!(!result.extras["roomtone"].is_empty());
    }

    #[test]
    fn a_true_peak_ceiling_is_honoured_end_to_end() {
        let result = run(&signal(), &chain(), &default_registry(), |_| {}).unwrap();
        let peak = leveller_dsp::truepeak::true_peak_dbfs(result.signal.channels());
        assert!(peak <= -1.0 + 0.05, "{peak} dBTP");
    }

    #[test]
    fn every_stage_reports_whether_it_acted() {
        let result = run(&signal(), &chain(), &default_registry(), |_| {}).unwrap();
        for stage in &result.report.stages {
            assert!(
                stage.report.is_object(),
                "{} reported {:?}",
                stage.name,
                stage.report
            );
            assert!(stage.elapsed_ms >= 0.0);
            assert!(stage.params.is_object(), "{} has no params", stage.name);
        }
    }

    #[test]
    fn the_denoiser_names_the_backend_that_ran() {
        let noisy = Arc::new(Signal::new(
            SR,
            add_noise(&speech().signal, 12.0, 5).channels().to_vec(),
        ));
        let result = run(&noisy, &chain(), &default_registry(), |_| {}).unwrap();
        let report = stage_report(&result.report, "denoise");

        assert_eq!(report["backend"], json!("spectral"));
        assert!(!report["skippedEntirely"].as_bool().unwrap());
        // The trained backend is not in a default build, and the report says so
        // rather than falling back silently.
        let skipped = report["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["name"], json!("onnx"));
        // And it measured what it achieved rather than assuming it.
        assert!(
            report["reductionAchievedDb"].as_f64().unwrap() > 2.0,
            "{report}"
        );
    }

    #[test]
    fn the_denoiser_declines_on_a_clean_recording() {
        // The taper: a recording whose floor is already 35 dB down gets none of
        // the requested reduction, because the processing would be all it got.
        let clean = leveller_corpus::synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, -18.0)],
            pause_sec: 1.5,
            floor_dbfs: -90.0,
            ..SpeechOptions::default()
        });
        let input = Arc::new(Signal::new(SR, clean.signal.channels().to_vec()));
        let result = run(&input, &chain(), &default_registry(), |_| {}).unwrap();
        let report = stage_report(&result.report, "denoise");

        assert!(report["skippedEntirely"].as_bool().unwrap(), "{report}");
        assert_eq!(report["reductionAppliedDb"], json!(0.0));
    }

    #[test]
    fn the_dereverberator_engages_only_past_its_threshold() {
        // Worth being explicit about what the corpus voice measures. Its
        // syllable envelope falls at roughly 4 Hz, and the blind decay
        // measurement reads that as well as the articulation, so it comes out
        // around 105 ms — above the 90 ms default, where real dry speech reads
        // about 35. So the corpus is *not* the fixture for "dry material is
        // left alone"; the DSP tests cover that against a bare reference. What
        // this checks is that the threshold is the thing deciding.
        let registry = default_registry();

        let mut lenient = serde_json::Map::new();
        lenient.insert("minDecayMs".into(), json!(500.0));
        let specs = build_chain(&ChainOptions {
            params: vec![("dereverb".into(), lenient)],
            ..ChainOptions::default()
        })
        .unwrap();
        let skipped = run(&signal(), &specs, &registry, |_| {}).unwrap();
        let report = stage_report(&skipped.report, "dereverb");
        assert!(report["skipped"].as_bool().unwrap(), "{report}");
        assert_eq!(report["binsProcessed"], json!(0));

        let engaged = run(&signal(), &chain(), &registry, |_| {}).unwrap();
        let report = stage_report(&engaged.report, "dereverb");
        assert!(!report["skipped"].as_bool().unwrap(), "{report}");
        assert!(report["binsProcessed"].as_u64().unwrap() > 0);
        // And it reports the decay it measured on both sides, so the decision
        // is auditable rather than a boolean.
        assert!(report["decayBeforeMs"].as_f64().unwrap() > 90.0);
        assert!(report["decayAfterMs"].is_number());
    }

    #[test]
    fn the_de_clicker_finds_injected_clicks_and_says_so() {
        let clicked = add_clicks(&speech().signal, &ClickOptions::default());
        let input = Arc::new(Signal::new(SR, clicked.signal.channels().to_vec()));
        let result = run(&input, &chain(), &default_registry(), |_| {}).unwrap();
        let report = stage_report(&result.report, "declick");

        assert!(!report["aborted"].as_bool().unwrap());
        assert!(report["repaired"].as_u64().unwrap() > 20, "{report}");
    }

    #[test]
    fn the_eq_reports_its_bands_and_its_voicing_separately() {
        let result = run(&signal(), &chain(), &default_registry(), |_| {}).unwrap();
        let report = stage_report(&result.report, "eq");

        assert!(!report["skipped"].as_bool().unwrap(), "{report}");
        assert_eq!(report["voicing"], json!("warm"));
        assert_eq!(report["voicingBands"].as_array().unwrap().len(), 2);
        assert!(report["speechFrames"].as_u64().unwrap() > 4);
    }

    #[test]
    fn a_neutral_voicing_places_no_tone_bands() {
        let mut params = serde_json::Map::new();
        params.insert("voicing".into(), json!("neutral"));
        let specs = build_chain(&ChainOptions {
            params: vec![("eq".into(), params)],
            ..ChainOptions::default()
        })
        .unwrap();

        let result = run(&signal(), &specs, &default_registry(), |_| {}).unwrap();
        let report = stage_report(&result.report, "eq");
        assert_eq!(report["voicing"], json!("neutral"));
        assert!(report["voicingBands"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_parameter_override_reaches_the_stage_that_uses_it() {
        let mut params = serde_json::Map::new();
        params.insert("targetLufs".into(), json!(-23.0));
        let specs = build_chain(&ChainOptions {
            params: vec![("level".into(), params)],
            ..ChainOptions::default()
        })
        .unwrap();

        let result = run(&signal(), &specs, &default_registry(), |_| {}).unwrap();
        let segment = speech().segments[0];
        let measured = leveller_dsp::loudness::Weighted::new(
            result.signal.channels(),
            result.signal.sample_rate(),
        )
        .integrated_over(segment.start, segment.end);
        assert!((measured - -23.0).abs() < 1.5, "{measured} LUFS");
    }

    #[test]
    fn a_fully_bypassed_chain_returns_the_file_untouched() {
        let input = signal();
        let specs = build_chain(&ChainOptions {
            bypass: DEFAULT_CHAIN.iter().map(|s| (*s).to_string()).collect(),
            ..ChainOptions::default()
        })
        .unwrap();

        let result = run(&input, &specs, &default_registry(), |_| {}).unwrap();
        assert!(Arc::ptr_eq(&result.signal, &input));
        assert!(result.report.stages.iter().all(|s| !s.enabled));
        assert!(result.report.extras.is_empty());
    }

    #[test]
    fn progress_reaches_every_stage_in_order() {
        let mut seen: Vec<String> = Vec::new();
        run(&signal(), &chain(), &default_registry(), |p: Progress| {
            if seen.last().map(String::as_str) != Some(p.stage) {
                seen.push(p.stage.to_string());
            }
        })
        .unwrap();
        assert_eq!(seen, DEFAULT_CHAIN.to_vec());
    }

    #[test]
    fn a_stereo_file_stays_stereo() {
        let stereo = leveller_corpus::synthetic_speech(&SpeechOptions {
            sample_rate: SR,
            spurts: vec![Spurt::new(4.0, -24.0)],
            pause_sec: 1.5,
            channels: 2,
            ..SpeechOptions::default()
        });
        let input = Arc::new(Signal::new(SR, stereo.signal.channels().to_vec()));
        let result = run(&input, &chain(), &default_registry(), |_| {}).unwrap();
        assert_eq!(result.signal.channel_count(), 2);
        assert_eq!(result.signal.len(), input.len());
    }

    #[test]
    fn a_forty_four_one_file_comes_back_at_forty_four_one() {
        let input = Arc::new(Signal::new(44_100, speech().signal.channels().to_vec()));
        let result = run(&input, &chain(), &default_registry(), |_| {}).unwrap();
        assert_eq!(result.signal.sample_rate(), 44_100);
        assert!(
            !result.report.resampled,
            "no stage in the default chain demands a rate"
        );
    }
}
