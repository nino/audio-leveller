//! The chain: what a stage is, how they are wired together, and what they
//! report.
//!
//! No stage lives here — those are in `leveller-stages`, which registers them.
//! Keeping the runner ignorant of its stages is what lets the trained denoise
//! backend be present in one build and absent in another without the runner
//! learning about it.

pub mod analysis;
pub mod registry;
pub mod runner;
pub mod stage;

pub use analysis::{Analyzer, Measurement};
pub use registry::Registry;
pub use runner::{
    PipelineError, PipelineReport, PipelineResult, Progress, StageReport, StageSpec, run,
};
pub use stage::{Stage, StageContext, StageError, StageOutput, params_of, report_of};

#[cfg(test)]
mod tests {
    use super::*;
    use leveller_dsp::Signal;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stage that multiplies by a gain and says what it did.
    struct Gain {
        name: &'static str,
        rate: Option<u32>,
        runs: Arc<AtomicUsize>,
    }

    impl Gain {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                rate: None,
                runs: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn at_rate(mut self, rate: u32) -> Self {
            self.rate = Some(rate);
            self
        }
    }

    #[derive(serde::Deserialize)]
    struct GainParams {
        gain: f64,
    }

    impl Stage for Gain {
        fn name(&self) -> &'static str {
            self.name
        }
        fn description(&self) -> &'static str {
            "multiply by a gain"
        }
        fn default_params(&self) -> Value {
            json!({ "gain": 1.0 })
        }
        fn required_sample_rate(&self) -> Option<u32> {
            self.rate
        }
        fn render(
            &self,
            signal: Arc<Signal>,
            params: &Value,
            ctx: &mut StageContext,
        ) -> Result<StageOutput, StageError> {
            self.runs.fetch_add(1, Ordering::Relaxed);
            let params: GainParams = params_of(self.name, params)?;
            ctx.progress(0.5);

            // A gain of one is a decline, and hands back the very same signal.
            if params.gain == 1.0 {
                return Ok(StageOutput::unchanged(
                    signal,
                    json!({ "applied": false, "gain": 1.0 }),
                ));
            }

            let mut out = (*signal).clone();
            out.scale(params.gain as f32);
            ctx.progress(1.0);
            Ok(StageOutput::new(
                out,
                json!({ "applied": true, "gain": params.gain }),
            ))
        }
    }

    /// A stage that emits an extra signal alongside its output.
    struct Emitter(&'static str);

    impl Stage for Emitter {
        fn name(&self) -> &'static str {
            "emit"
        }
        fn description(&self) -> &'static str {
            "produce an extra signal"
        }
        fn default_params(&self) -> Value {
            json!({})
        }
        fn render(
            &self,
            signal: Arc<Signal>,
            _params: &Value,
            _ctx: &mut StageContext,
        ) -> Result<StageOutput, StageError> {
            let extra = Signal::mono(signal.sample_rate(), vec![0.25; 100]);
            Ok(StageOutput::unchanged(signal, json!({})).with_extra(self.0, extra))
        }
    }

    fn registry() -> Registry {
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Gain::new("first")))
            .register(Arc::new(Gain::new("second")));
        registry
    }

    fn signal() -> Arc<Signal> {
        Arc::new(Signal::mono(
            48_000,
            (0..48_000)
                .map(|i| (0.4 * (i as f64 * 0.05).sin()) as f32)
                .collect(),
        ))
    }

    fn specs(gains: &[(&str, f64)]) -> Vec<StageSpec> {
        gains
            .iter()
            .map(|(name, gain)| {
                let mut params = serde_json::Map::new();
                params.insert("gain".into(), json!(gain));
                StageSpec::new(*name).with_params(params)
            })
            .collect()
    }

    #[test]
    fn stages_run_in_the_order_they_are_given() {
        let input = signal();
        let result = run(
            &input,
            &specs(&[("first", 2.0), ("second", 0.5)]),
            &registry(),
            |_| {},
        )
        .unwrap();

        // 2 x 0.5 = 1, so the audio comes back where it started.
        for (a, b) in result.signal.channel(0).iter().zip(input.channel(0)) {
            assert!((a - b).abs() < 1e-6);
        }
        assert_eq!(result.report.stages.len(), 2);
        assert_eq!(result.report.stages[0].name, "first");
        assert_eq!(result.report.stages[1].name, "second");
    }

    #[test]
    fn a_fully_bypassed_chain_is_bit_identical() {
        // Not "very close": the same buffer, passed through.
        let input = signal();
        let result = run(
            &input,
            &[
                StageSpec::new("first").bypassed(),
                StageSpec::new("second").bypassed(),
            ],
            &registry(),
            |_| {},
        )
        .unwrap();
        assert!(Arc::ptr_eq(&result.signal, &input));
    }

    #[test]
    fn a_chain_of_stages_that_all_decline_is_bit_identical_too() {
        // Every stage returns the signal it was handed, so nothing is copied
        // and nothing is measured twice.
        let input = signal();
        let result = run(
            &input,
            &specs(&[("first", 1.0), ("second", 1.0)]),
            &registry(),
            |_| {},
        )
        .unwrap();
        assert!(Arc::ptr_eq(&result.signal, &input));
        assert_eq!(result.report.input, result.report.output);
    }

    #[test]
    fn a_bypassed_stage_still_appears_in_the_report() {
        let result = run(
            &signal(),
            &[
                StageSpec::new("first").bypassed(),
                specs(&[("second", 2.0)])[0].clone(),
            ],
            &registry(),
            |_| {},
        )
        .unwrap();

        assert_eq!(result.report.stages.len(), 2);
        assert!(!result.report.stages[0].enabled);
        assert_eq!(result.report.stages[0].report, Value::Null);
        assert!(result.report.stages[1].enabled);
        assert_eq!(result.report.stages[1].report["applied"], json!(true));
    }

    #[test]
    fn parameters_are_reported_fully_resolved() {
        // Defaults merged with overrides, so a report says what actually ran
        // rather than what was asked for.
        let result = run(&signal(), &specs(&[("first", 3.0)]), &registry(), |_| {}).unwrap();
        assert_eq!(result.report.stages[0].params, json!({ "gain": 3.0 }));

        let untouched = run(&signal(), &[StageSpec::new("first")], &registry(), |_| {}).unwrap();
        assert_eq!(untouched.report.stages[0].params, json!({ "gain": 1.0 }));
    }

    #[test]
    fn progress_runs_from_nothing_to_everything() {
        let mut seen: Vec<f64> = Vec::new();
        run(
            &signal(),
            &specs(&[("first", 2.0), ("second", 2.0)]),
            &registry(),
            |p| seen.push(p.overall),
        )
        .unwrap();

        assert_eq!(seen.first(), Some(&0.0));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "progress went backwards: {seen:?}"
        );
    }

    #[test]
    fn progress_names_the_stage_and_counts_only_the_enabled_ones() {
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        run(
            &signal(),
            &[
                StageSpec::new("first").bypassed(),
                specs(&[("second", 2.0)])[0].clone(),
            ],
            &registry(),
            |p| seen.push((p.stage.to_string(), p.index, p.total)),
        )
        .unwrap();

        assert!(seen.iter().all(|(name, _, _)| name == "second"));
        assert!(seen.iter().all(|(_, _, total)| *total == 1));
        assert!(seen.iter().all(|(_, index, _)| *index == 0));
    }

    #[test]
    fn a_stage_that_names_a_rate_gets_it_and_the_output_comes_back() {
        let mut registry = Registry::new();
        registry.register(Arc::new(Gain::new("first").at_rate(44_100)));

        let input = signal(); // 48 kHz
        let result = run(&input, &specs(&[("first", 2.0)]), &registry, |_| {}).unwrap();

        assert_eq!(result.report.source_sample_rate, 48_000);
        assert_eq!(result.report.working_sample_rate, 44_100);
        assert!(result.report.resampled);
        assert_eq!(result.signal.sample_rate(), 48_000, "and back again");
        assert_eq!(result.signal.len(), input.len());
    }

    #[test]
    fn a_bypassed_rate_demand_does_not_trigger_a_conversion() {
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Gain::new("first").at_rate(44_100)))
            .register(Arc::new(Gain::new("second")));

        let result = run(
            &signal(),
            &[
                StageSpec::new("first").bypassed(),
                specs(&[("second", 2.0)])[0].clone(),
            ],
            &registry,
            |_| {},
        )
        .unwrap();
        assert!(!result.report.resampled);
        assert_eq!(result.report.working_sample_rate, 48_000);
    }

    #[test]
    fn stages_that_disagree_on_a_rate_are_refused() {
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Gain::new("first").at_rate(44_100)))
            .register(Arc::new(Gain::new("second").at_rate(48_000)));

        let error = run(
            &signal(),
            &specs(&[("first", 2.0), ("second", 2.0)]),
            &registry,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(error, PipelineError::MixedRates { .. }), "{error}");
    }

    #[test]
    fn an_unknown_stage_is_refused_before_anything_runs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        let counted = Gain {
            name: "first",
            rate: None,
            runs: runs.clone(),
        };
        registry.register(Arc::new(counted));

        let error = run(
            &signal(),
            &[
                specs(&[("first", 2.0)])[0].clone(),
                StageSpec::new("nonsense"),
            ],
            &registry,
            |_| {},
        )
        .unwrap_err();

        assert!(
            matches!(error, PipelineError::UnknownStage { .. }),
            "{error}"
        );
        assert_eq!(
            runs.load(Ordering::Relaxed),
            0,
            "nothing should have run before the chain was rejected"
        );
    }

    #[test]
    fn extras_are_collected_and_named() {
        let mut registry = Registry::new();
        registry.register(Arc::new(Emitter("roomtone")));

        let result = run(&signal(), &[StageSpec::new("emit")], &registry, |_| {}).unwrap();
        assert_eq!(result.report.extras, vec!["roomtone".to_string()]);
        assert_eq!(result.extras["roomtone"].len(), 100);
    }

    #[test]
    fn two_stages_claiming_one_extra_name_is_an_error() {
        struct Second(Emitter);
        impl Stage for Second {
            fn name(&self) -> &'static str {
                "emit2"
            }
            fn description(&self) -> &'static str {
                self.0.description()
            }
            fn default_params(&self) -> Value {
                self.0.default_params()
            }
            fn render(
                &self,
                signal: Arc<Signal>,
                params: &Value,
                ctx: &mut StageContext,
            ) -> Result<StageOutput, StageError> {
                self.0.render(signal, params, ctx)
            }
        }

        let mut registry = Registry::new();
        registry
            .register(Arc::new(Emitter("roomtone")))
            .register(Arc::new(Second(Emitter("roomtone"))));

        let error = run(
            &signal(),
            &[StageSpec::new("emit"), StageSpec::new("emit2")],
            &registry,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(error, PipelineError::DuplicateExtra { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_stage_given_nonsense_parameters_says_which_stage() {
        let mut params = serde_json::Map::new();
        params.insert("gain".into(), json!("loud"));
        let error = run(
            &signal(),
            &[StageSpec::new("first").with_params(params)],
            &registry(),
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("first"), "{error}");
    }

    #[test]
    fn the_report_describes_the_file_it_ran_on() {
        let input = Arc::new(Signal::new(
            44_100,
            vec![vec![0.1; 44_100], vec![0.1; 44_100]],
        ));
        let result = run(&input, &specs(&[("first", 2.0)]), &registry(), |_| {}).unwrap();

        assert_eq!(result.report.channels, 2);
        assert_eq!(result.report.source_sample_rate, 44_100);
        assert!((result.report.duration_sec - 1.0).abs() < 1e-9);
        assert!(result.report.stages[0].elapsed_ms >= 0.0);
    }

    #[test]
    fn a_registry_refuses_a_duplicate_name() {
        let mut registry = Registry::new();
        registry.register(Arc::new(Gain::new("first")));
        assert!(registry.contains("first"));
        assert_eq!(registry.names(), vec!["first"]);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn registering_one_name_twice_is_a_wiring_mistake() {
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Gain::new("first")))
            .register(Arc::new(Gain::new("first")));
    }

    #[test]
    fn an_empty_chain_runs_and_measures() {
        let input = signal();
        let result = run(&input, &[], &registry(), |_| {}).unwrap();
        assert!(Arc::ptr_eq(&result.signal, &input));
        assert!(result.report.stages.is_empty());
        assert!(result.report.input.integrated_lufs.is_finite());
    }
}
