//! The parameters worth putting in front of someone, and the presets that set
//! them.
//!
//! Every stage has more parameters than are worth exposing — transform sizes,
//! iteration counts, detector time constants that only make sense next to the
//! code that reads them. What is listed here is the subset that changes how the
//! result *sounds*, with the range each one is sane over. The defaults are not
//! repeated: they are read back out of the stage registry, so this file cannot
//! drift away from the code it describes.
//!
//! Presets are plain parameter overrides on top of those defaults, which keeps
//! "what a preset is" honest — there is no hidden second path through the
//! chain, only a different set of numbers.

use leveller_pipeline::Registry;
use serde_json::{Map, Value, json};

/// One exposed parameter.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ParamSpec {
    #[serde(rename_all = "camelCase")]
    Number {
        key: &'static str,
        label: &'static str,
        min: f64,
        max: f64,
        step: f64,
        /// Shown after the value: dB, ms, LUFS…
        unit: &'static str,
        help: &'static str,
    },
    #[serde(rename_all = "camelCase")]
    Boolean {
        key: &'static str,
        label: &'static str,
        help: &'static str,
    },
    #[serde(rename_all = "camelCase")]
    Choice {
        key: &'static str,
        label: &'static str,
        options: Vec<(&'static str, &'static str)>,
        help: &'static str,
    },
}

impl ParamSpec {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Number { key, .. } | Self::Boolean { key, .. } | Self::Choice { key, .. } => key,
        }
    }
}

/// A stage's exposed parameters, with its own one-liner attached.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageParams {
    /// Stage name, as registered.
    pub stage: &'static str,
    /// Title case, for the interface.
    pub label: &'static str,
    /// Filled in from the registry, so it cannot drift.
    pub description: String,
    pub params: Vec<ParamSpec>,
}

const fn num(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    step: f64,
    unit: &'static str,
    help: &'static str,
) -> ParamSpec {
    ParamSpec::Number {
        key,
        label,
        min,
        max,
        step,
        unit,
        help,
    }
}

/// The exposed parameters, in chain order.
///
/// Ranges are the *useful* range rather than the representable one: the
/// leveller will happily target −40 LUFS, but nobody wants that, and a slider
/// spending most of its travel in useless territory is worse than no slider.
fn groups() -> Vec<(&'static str, &'static str, Vec<ParamSpec>)> {
    vec![
        (
            "declick",
            "De-click",
            vec![
                num(
                    "thresholdSigma",
                    "Sensitivity",
                    3.0,
                    12.0,
                    0.5,
                    "σ",
                    "How far above the local noise a sample must sit to count as a click. Lower finds more, and eventually finds consonants.",
                ),
                num(
                    "maxRepairFraction",
                    "Repair limit",
                    0.002,
                    0.1,
                    0.002,
                    "of file",
                    "If more than this fraction of the file looks like clicks, the stage declines rather than rebuilding the recording.",
                ),
                num(
                    "pulseVetoRatio",
                    "Pulse veto",
                    0.0,
                    1.0,
                    0.05,
                    "×",
                    "How large a neighbouring impulse must be, against a candidate's own, for the candidate to be read as the voice's own excitation and left alone.",
                ),
            ],
        ),
        (
            "denoise",
            "Denoise",
            vec![
                num(
                    "reductionDb",
                    "Reduction",
                    0.0,
                    24.0,
                    1.0,
                    "dB",
                    "How much noise to ask the backend to remove.",
                ),
                num(
                    "cleanSnrDb",
                    "Clean threshold",
                    15.0,
                    60.0,
                    1.0,
                    "dB",
                    "Programme-to-floor distance above which the recording counts as already clean and is left alone.",
                ),
                num(
                    "maxProgrammeLossDb",
                    "Programme loss cap",
                    0.0,
                    6.0,
                    0.5,
                    "dB",
                    "Reject the denoised result if it cost more than this much of the speech itself.",
                ),
            ],
        ),
        (
            "dereverb",
            "De-reverb",
            vec![
                num(
                    "minDecayMs",
                    "Dryness threshold",
                    40.0,
                    300.0,
                    5.0,
                    "ms",
                    "Recordings whose decay is shorter than this are dry enough to leave untouched.",
                ),
                num(
                    "taps",
                    "Prediction taps",
                    5.0,
                    40.0,
                    1.0,
                    "",
                    "Length of the late-reverberation model. More taps reach further back, at a cost in time.",
                ),
                num(
                    "iterations",
                    "Iterations",
                    1.0,
                    6.0,
                    1.0,
                    "",
                    "How many times the estimate is refined.",
                ),
            ],
        ),
        (
            "eq",
            "EQ",
            vec![
                ParamSpec::Choice {
                    key: "voicing",
                    label: "Voicing",
                    options: vec![("warm", "Warm"), ("neutral", "Neutral")],
                    help: "A fixed tonal tilt applied after the corrective fit. Warm is +1 dB under 130 Hz and −1 dB across 2.5–5 kHz; neutral is off.",
                },
                num(
                    "maxBands",
                    "Max bands",
                    0.0,
                    8.0,
                    1.0,
                    "",
                    "How many corrective bands the fitter may place.",
                ),
                num(
                    "correction",
                    "Correction strength",
                    0.0,
                    1.0,
                    0.05,
                    "×",
                    "Fraction of each measured deviation actually corrected. 1 flattens the response; less leaves the recording its character.",
                ),
                num(
                    "maxCutDb",
                    "Max cut",
                    0.0,
                    12.0,
                    0.5,
                    "dB",
                    "Deepest single corrective cut.",
                ),
                num(
                    "maxBoostDb",
                    "Max boost",
                    0.0,
                    12.0,
                    0.5,
                    "dB",
                    "Largest single corrective boost. Kept smaller than the cut, because boosting lifts the noise with the speech.",
                ),
                ParamSpec::Boolean {
                    key: "rumbleEnabled",
                    label: "Rumble filter",
                    help: "Fit a high-pass when the sub-bass comes too close to the voice's fundamentals.",
                },
                num(
                    "rumbleFreq",
                    "Rumble corner",
                    40.0,
                    160.0,
                    5.0,
                    "Hz",
                    "Corner frequency used when the rumble filter engages.",
                ),
            ],
        ),
        (
            "dyneq",
            "Dynamic EQ",
            vec![
                num(
                    "thresholdDb",
                    "Threshold",
                    4.0,
                    24.0,
                    1.0,
                    "dB",
                    "How far a band must rise above its own neighbours before it is treated as a resonance.",
                ),
                num(
                    "maxReductionDb",
                    "Max reduction",
                    0.0,
                    18.0,
                    0.5,
                    "dB",
                    "Cap on how hard any one band is pushed down.",
                ),
                num(
                    "sibilanceExtraDb",
                    "De-esser",
                    0.0,
                    9.0,
                    0.5,
                    "dB",
                    "Extra sensitivity across 5–9 kHz, where sibilance lives.",
                ),
            ],
        ),
        (
            "expand",
            "Expander",
            vec![
                num(
                    "thresholdBelowProgrammeDb",
                    "Threshold",
                    12.0,
                    40.0,
                    1.0,
                    "dB below",
                    "How far under the programme loudness the expander starts working.",
                ),
                num(
                    "ratio",
                    "Ratio",
                    1.0,
                    6.0,
                    0.1,
                    ":1",
                    "Slope below the threshold.",
                ),
                num(
                    "rangeDb",
                    "Range",
                    0.0,
                    40.0,
                    1.0,
                    "dB",
                    "Cap on attenuation. This is what keeps it an expander rather than a gate.",
                ),
                num(
                    "cleanFloorDb",
                    "Clean floor",
                    30.0,
                    80.0,
                    1.0,
                    "dB",
                    "Programme-to-floor distance above which the floor is already deep enough to leave alone.",
                ),
            ],
        ),
        (
            "compress",
            "Compressor",
            vec![
                num(
                    "thresholdRelativeDb",
                    "Threshold",
                    -12.0,
                    6.0,
                    0.5,
                    "dB rel.",
                    "Where the knee sits relative to the programme loudness.",
                ),
                num(
                    "ratio",
                    "Ratio",
                    1.0,
                    6.0,
                    0.1,
                    ":1",
                    "Slope above the threshold.",
                ),
                num(
                    "kneeDb",
                    "Knee",
                    0.0,
                    20.0,
                    1.0,
                    "dB",
                    "Width of the soft knee, centred on the threshold.",
                ),
                num(
                    "attackMs",
                    "Attack",
                    1.0,
                    200.0,
                    1.0,
                    "ms",
                    "Time constant while the gain falls.",
                ),
                num(
                    "releaseMs",
                    "Release",
                    20.0,
                    800.0,
                    5.0,
                    "ms",
                    "Time constant while the gain recovers.",
                ),
                num(
                    "maxReductionDb",
                    "Max reduction",
                    0.0,
                    24.0,
                    1.0,
                    "dB",
                    "Hard cap on attenuation.",
                ),
                num(
                    "minLoudnessRangeLu",
                    "Skip below",
                    0.0,
                    10.0,
                    0.5,
                    "LU",
                    "Loudness range under which the material is already even and the stage declines.",
                ),
            ],
        ),
        (
            "level",
            "Leveller",
            vec![
                num(
                    "targetLufs",
                    "Target",
                    -31.0,
                    -12.0,
                    0.5,
                    "LUFS",
                    "Loudness every speech segment is normalised to.",
                ),
                num(
                    "ceilingDb",
                    "Ceiling",
                    -9.0,
                    0.0,
                    0.5,
                    "dBTP",
                    "True-peak ceiling for the output limiter.",
                ),
                num(
                    "maxGainDb",
                    "Max gain",
                    0.0,
                    40.0,
                    1.0,
                    "dB",
                    "Clamp on per-segment gain, so a near-silent segment is not boosted into its own noise.",
                ),
            ],
        ),
    ]
}

/// A preset: a name, a reason, and some numbers.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub name: &'static str,
    /// One line on who this preset is for.
    pub description: &'static str,
    /// Stages to bypass entirely.
    pub bypass: Vec<&'static str>,
    /// Overrides, keyed by stage name.
    pub params: Vec<(&'static str, Map<String, Value>)>,
}

/// The presets.
///
/// `Nino` is the chain as tuned — an empty override set, deliberately, so that
/// "the default preset" and "the defaults" cannot come apart.
///
/// `ACX` targets Audible's submission requirements: RMS between −23 and −18 dB,
/// peaks no higher than −3 dBFS, and a noise floor at or below −60 dB RMS. The
/// numbers aim at the middle of the loudness window rather than its edge, leave
/// half a dB of headroom under the peak limit, and lean harder on the two
/// stages that set the floor. Voicing goes neutral: the warm tilt is a taste,
/// and a submission rejected for tone is not a taste anyone wanted.
pub fn presets() -> Vec<Preset> {
    let object = |pairs: &[(&str, Value)]| -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    };

    vec![
        Preset {
            name: "Nino",
            description: "The chain as tuned — spoken word, warm, −18 LUFS.",
            bypass: Vec::new(),
            params: Vec::new(),
        },
        Preset {
            name: "ACX",
            description: "Audible/ACX submission: −20 LUFS, −3.5 dBTP ceiling, deep noise floor, no tonal tilt.",
            bypass: Vec::new(),
            params: vec![
                (
                    "level",
                    object(&[("targetLufs", json!(-20.0)), ("ceilingDb", json!(-3.5))]),
                ),
                ("eq", object(&[("voicing", json!("neutral"))])),
                // ACX fails a submission on its noise floor, so both floor
                // stages are asked to work on material the defaults would call
                // clean enough.
                (
                    "denoise",
                    object(&[("reductionDb", json!(16.0)), ("cleanSnrDb", json!(45.0))]),
                ),
                (
                    "expand",
                    object(&[("cleanFloorDb", json!(65.0)), ("rangeDb", json!(16.0))]),
                ),
                // A narrower dynamic range keeps the whole book inside the RMS
                // window, chapter to chapter, without riding the fader.
                (
                    "compress",
                    object(&[("ratio", json!(2.2)), ("minLoudnessRangeLu", json!(2.0))]),
                ),
            ],
        },
    ]
}

pub const DEFAULT_PRESET: &str = "Nino";

/// Find a preset by name, case-insensitively.
pub fn find_preset(name: &str) -> Option<Preset> {
    presets()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Everything the interface needs to draw the parameter panel, in one payload.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// Chain order, including the stages with no exposed parameters.
    pub chain: Vec<&'static str>,
    pub groups: Vec<StageParams>,
    /// Registry defaults, narrowed to the exposed keys.
    pub defaults: Vec<(&'static str, Map<String, Value>)>,
    pub presets: Vec<Preset>,
    pub default_preset: &'static str,
}

impl Schema {
    /// Fully resolved parameters for a preset: the exposed defaults with its
    /// overrides merged in.
    pub fn settings_for(&self, preset: &str) -> Option<Vec<(&'static str, Map<String, Value>)>> {
        let preset = self
            .presets
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(preset))?;
        let mut resolved = self.defaults.clone();
        for (stage, overrides) in &preset.params {
            if let Some((_, target)) = resolved.iter_mut().find(|(name, _)| name == stage) {
                for (key, value) in overrides {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        Some(resolved)
    }
}

/// The schema, with each stage's description and defaults read from the
/// registry it will actually run against.
pub fn schema(registry: &Registry) -> Schema {
    let groups = groups();

    let described: Vec<StageParams> = groups
        .iter()
        .filter_map(|(stage, label, params)| {
            let registered = registry.get(stage)?;
            Some(StageParams {
                stage,
                label,
                description: registered.description().to_string(),
                params: params.clone(),
            })
        })
        .collect();

    let defaults = groups
        .iter()
        .filter_map(|(stage, _, params)| {
            let registered = registry.get(stage)?;
            let all = registered.default_params();
            let picked: Map<String, Value> = params
                .iter()
                .filter_map(|p| Some((p.key().to_string(), all.get(p.key())?.clone())))
                .collect();
            Some((*stage, picked))
        })
        .collect();

    Schema {
        chain: registry.names(),
        groups: described,
        defaults,
        presets: presets(),
        default_preset: DEFAULT_PRESET,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_CHAIN, default_registry};

    #[test]
    fn every_exposed_parameter_exists_on_the_stage_it_names() {
        // The whole reason defaults are read from the registry rather than
        // repeated here: this test is what stops the two drifting.
        let registry = default_registry();
        let schema = schema(&registry);

        for group in &schema.groups {
            let defaults = registry
                .get(group.stage)
                .unwrap_or_else(|| panic!("no {} stage", group.stage))
                .default_params();
            for param in &group.params {
                assert!(
                    defaults.get(param.key()).is_some(),
                    "{} exposes \"{}\", which its stage does not have",
                    group.stage,
                    param.key()
                );
            }
        }
    }

    #[test]
    fn the_defaults_carry_a_value_for_every_exposed_key() {
        let schema = schema(&default_registry());
        for (stage, defaults) in &schema.defaults {
            let group = schema.groups.iter().find(|g| g.stage == *stage).unwrap();
            assert_eq!(defaults.len(), group.params.len(), "{stage}");
            for param in &group.params {
                assert!(
                    defaults.contains_key(param.key()),
                    "{stage}.{}",
                    param.key()
                );
            }
        }
    }

    #[test]
    fn every_stage_description_comes_from_the_registry() {
        let registry = default_registry();
        for group in &schema(&registry).groups {
            assert_eq!(
                group.description,
                registry.get(group.stage).unwrap().description()
            );
        }
    }

    #[test]
    fn the_default_preset_is_the_defaults() {
        // "The default preset" and "the defaults" cannot come apart, because
        // the first is an empty override set over the second.
        let nino = find_preset("Nino").expect("the default preset");
        assert!(nino.params.is_empty());
        assert!(nino.bypass.is_empty());

        let schema = schema(&default_registry());
        assert_eq!(
            schema.settings_for("Nino").unwrap(),
            schema.defaults,
            "the default preset changed something"
        );
    }

    #[test]
    fn a_preset_resolves_to_the_defaults_with_its_overrides_on_top() {
        let schema = schema(&default_registry());
        let acx = schema.settings_for("ACX").expect("the ACX preset");

        let level = &acx.iter().find(|(s, _)| *s == "level").unwrap().1;
        assert_eq!(level["targetLufs"], json!(-20.0));
        assert_eq!(level["ceilingDb"], json!(-3.5));
        // An untouched key keeps its default rather than disappearing.
        assert!(level.contains_key("maxGainDb"));
    }

    #[test]
    fn every_preset_names_stages_and_keys_that_exist() {
        let registry = default_registry();
        for preset in presets() {
            for stage in &preset.bypass {
                assert!(DEFAULT_CHAIN.contains(stage), "{}: {stage}", preset.name);
            }
            for (stage, overrides) in &preset.params {
                let defaults = registry
                    .get(stage)
                    .unwrap_or_else(|| panic!("{}: no {stage} stage", preset.name))
                    .default_params();
                for key in overrides.keys() {
                    assert!(
                        defaults.get(key).is_some(),
                        "{}: {stage} has no \"{key}\"",
                        preset.name
                    );
                }
            }
        }
    }

    #[test]
    fn a_preset_can_be_found_however_it_is_capitalised() {
        assert!(find_preset("acx").is_some());
        assert!(find_preset("ACX").is_some());
        assert!(find_preset("nino").is_some());
        assert!(find_preset("Norwegian").is_none());
    }

    #[test]
    fn every_numeric_default_sits_inside_the_range_its_slider_offers() {
        // A slider whose default is off its own scale is a slider that jumps
        // the first time it is touched.
        let schema = schema(&default_registry());
        for group in &schema.groups {
            let defaults = &schema
                .defaults
                .iter()
                .find(|(s, _)| *s == group.stage)
                .unwrap()
                .1;
            for param in &group.params {
                let ParamSpec::Number { key, min, max, .. } = param else {
                    continue;
                };
                let value = defaults[*key].as_f64().unwrap_or_else(|| {
                    panic!(
                        "{}.{key} is not a number: {:?}",
                        group.stage, defaults[*key]
                    )
                });
                assert!(
                    value >= *min && value <= *max,
                    "{}.{key} defaults to {value}, outside {min}..{max}",
                    group.stage
                );
            }
        }
    }

    #[test]
    fn every_choice_default_is_one_of_the_choices() {
        let schema = schema(&default_registry());
        for group in &schema.groups {
            let defaults = &schema
                .defaults
                .iter()
                .find(|(s, _)| *s == group.stage)
                .unwrap()
                .1;
            for param in &group.params {
                let ParamSpec::Choice { key, options, .. } = param else {
                    continue;
                };
                let value = defaults[*key].as_str().unwrap();
                assert!(
                    options.iter().any(|(v, _)| *v == value),
                    "{}.{key} defaults to \"{value}\", which is not on offer",
                    group.stage
                );
            }
        }
    }

    #[test]
    fn the_chain_in_the_schema_is_the_registry_order() {
        assert_eq!(schema(&default_registry()).chain, DEFAULT_CHAIN.to_vec());
    }
}
