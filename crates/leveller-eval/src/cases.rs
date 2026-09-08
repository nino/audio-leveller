//! The evaluation corpus.
//!
//! Each case pairs a synthetic input with expectations that make the run pass
//! or fail. Expectations are bounds with a stated reason, not snapshots of
//! whatever the code currently does — a bound you cannot justify in a sentence
//! is a bound that will be quietly relaxed the first time it fails.
//!
//! Cases carrying a `reference` (the undegraded signal) get SI-SDR scored
//! against that reference *put through the same chain*, so the score measures
//! what the degradation did rather than what the chain was asked to do.

use std::sync::Arc;

use leveller_corpus::{
    ClickOptions, RirOptions, SpeechOptions, SpeechSegment, Spurt, add_clicks, add_noise,
    add_reverb, synthetic_speech,
};
use leveller_dsp::{Biquad, Signal, apply_cascade};
use leveller_pipeline::StageSpec;
use leveller_stages::{ChainOptions, DEFAULT_CHAIN, backend::Backends, build_chain};
use serde_json::{Map, Value, json};

const SR: u32 = 48_000;
const TARGET: f64 = -18.0;

/// A bound, and why it is the right one.
#[derive(Clone, Debug)]
pub struct Expectation {
    /// Key from [`crate::metrics::compute`]. An unknown key fails the run
    /// rather than passing it.
    pub metric: &'static str,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Printed when it fails.
    pub because: &'static str,
}

impl Expectation {
    fn min(metric: &'static str, min: f64, because: &'static str) -> Self {
        Self {
            metric,
            min: Some(min),
            max: None,
            because,
        }
    }

    fn max(metric: &'static str, max: f64, because: &'static str) -> Self {
        Self {
            metric,
            min: None,
            max: Some(max),
            because,
        }
    }

    fn between(metric: &'static str, min: f64, max: f64, because: &'static str) -> Self {
        Self {
            metric,
            min: Some(min),
            max: Some(max),
            because,
        }
    }

    /// The same bound with a different justification, for a case where the
    /// number means something slightly different.
    fn because(mut self, because: &'static str) -> Self {
        self.because = because;
        self
    }

    fn with_max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }
}

/// What a case feeds the chain, and what it knows about it.
pub struct CaseInput {
    pub input: Arc<Signal>,
    pub reference: Option<Arc<Signal>>,
    pub segments: Vec<SpeechSegment>,
    pub click_positions: Vec<usize>,
    pub target_lufs: Option<f64>,
}

impl CaseInput {
    fn new(input: Signal) -> Self {
        Self {
            input: Arc::new(input),
            reference: None,
            segments: Vec::new(),
            click_positions: Vec::new(),
            target_lufs: None,
        }
    }

    fn reference(mut self, reference: Signal) -> Self {
        self.reference = Some(Arc::new(reference));
        self
    }

    fn segments(mut self, segments: Vec<SpeechSegment>) -> Self {
        self.segments = segments;
        self
    }

    fn clicks(mut self, positions: Vec<usize>) -> Self {
        self.click_positions = positions;
        self
    }

    fn target(mut self) -> Self {
        self.target_lufs = Some(TARGET);
        self
    }
}

pub struct EvalCase {
    pub name: String,
    pub description: String,
    pub chain: Vec<StageSpec>,
    pub build: Box<dyn Fn() -> CaseInput + Send + Sync>,
    pub expectations: Vec<Expectation>,
    /// Why this case cannot run here, or `None` when it can.
    ///
    /// A case needing model weights reports them missing and is skipped rather
    /// than failing — but it says so in the output, because a silently absent
    /// check is worse than a missing one.
    pub unavailable: Option<String>,
}

/// Pin the denoiser to the classical backend.
///
/// Every case that runs the default chain says this explicitly, so the corpus
/// measures the same thing on a machine with model weights installed as on one
/// without. Leaving it to the preference order would make the baseline depend
/// on what happens to be in `~/.audio-leveller`, and "the numbers moved because
/// your home directory changed" is exactly the sort of thing this harness
/// exists to rule out. The model backend gets its own cases.
fn spectral() -> Map<String, Value> {
    params(json!({ "backends": ["spectral"] }))
}

fn params(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// The whole chain, with the denoiser pinned.
fn chain() -> Vec<StageSpec> {
    build_chain(&ChainOptions {
        params: vec![("denoise".into(), spectral())],
        ..ChainOptions::default()
    })
    .expect("the default chain names only stages that exist")
}

/// One stage on its own.
fn only(names: &[&str]) -> Vec<StageSpec> {
    only_with(names, Vec::new())
}

fn only_with(names: &[&str], overrides: Vec<(String, Map<String, Value>)>) -> Vec<StageSpec> {
    build_chain(&ChainOptions {
        only: names.iter().map(|n| (*n).to_string()).collect(),
        params: overrides,
        ..ChainOptions::default()
    })
    .expect("stage names in the corpus are checked by the compiler being wrong loudly")
}

/// The standard three-spurt programme, at whatever levels a case needs.
struct Programme {
    levels: Vec<f64>,
    sample_rate: u32,
    floor_dbfs: f64,
    channels: usize,
}

impl Programme {
    fn new(levels: [f64; 3]) -> Self {
        Self {
            levels: levels.to_vec(),
            sample_rate: SR,
            floor_dbfs: -62.0,
            channels: 1,
        }
    }

    fn floor(mut self, dbfs: f64) -> Self {
        self.floor_dbfs = dbfs;
        self
    }

    fn rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    fn stereo(mut self) -> Self {
        self.channels = 2;
        self
    }

    fn build(&self) -> leveller_corpus::Speech {
        synthetic_speech(&SpeechOptions {
            sample_rate: self.sample_rate,
            spurts: self
                .levels
                .iter()
                .enumerate()
                .map(|(i, level)| Spurt::new(3.5, *level).at_pitch(105.0 + i as f64 * 22.0))
                .collect(),
            pause_sec: 1.4,
            floor_dbfs: self.floor_dbfs,
            seed: 4242,
            channels: self.channels,
        })
    }
}

/// Colour a signal with fixed resonances, as a room and a cheap mic would.
fn colour(signal: &Signal, bands: &[(f64, f64, f64)]) -> Signal {
    let cascade: Vec<Biquad> = bands
        .iter()
        .map(|(freq, gain_db, q)| {
            Biquad::peaking(f64::from(signal.sample_rate()), *freq, *gain_db, *q)
        })
        .collect();
    Signal::new(
        signal.sample_rate(),
        signal
            .channels()
            .iter()
            .map(|channel| apply_cascade(channel, &cascade))
            .collect(),
    )
}

/// Add a low-frequency tone, standing in for traffic or handling noise.
fn add_rumble(signal: &Signal, freq: f64, amplitude: f64) -> Signal {
    let rate = f64::from(signal.sample_rate());
    let mut out = signal.clone();
    for channel in out.channels_mut() {
        for (i, sample) in channel.iter_mut().enumerate() {
            *sample += (amplitude
                * (std::f64::consts::TAU * freq * i as f64 / rate).sin())
                as f32;
        }
    }
    out
}

// ------------------------------------------------------- shared bounds --

/// What the whole chain does to segment-local SNR on clean material.
///
/// This bound used to read −1 to 1, on the reasoning that levelling applies one
/// gain per segment and therefore moves speech and noise together. That premise
/// expired when the expander joined the chain: an expander deliberately
/// attenuates the quiet parts, and the quiet parts inside a segment are the
/// gaps between words, so it moves this number by design. Widening it is a
/// consequence of the chain changing, not of the bound being inconvenient — and
/// the two jobs the old bound was doing have been separated rather than
/// dropped.
///
/// Note this is the *segment-local* SNR, not the whole-file `snrGainDb`. The
/// whole-file figure legitimately drops when segments are pulled together, so
/// bounding that one would be asserting something false about what levelling
/// does.
fn snr_expanded() -> Expectation {
    Expectation::between(
        "segmentSnrGainDb",
        6.0,
        14.0,
        "on clean material the expander is the only stage that moves this, so \
         the bound is a statement about the expander: it must engage on a floor \
         this close to the programme, and it must stay inside its own range \
         cap. The upper bound is that cap plus the little the leveller \
         contributes — past 14 dB means rangeDb has stopped holding, which is \
         the difference between an expander and a gate. The other job this \
         bound used to do, checking that the denoiser backs off on clean \
         sources, now lives on `clean-denoise`, where it is asserted as \
         bit-identity through the denoise stage alone — a stronger check than \
         an SNR window ever was",
    )
}

/// Cases noisy enough that the denoiser is supposed to engage.
///
/// On the full chain this number is the denoiser and the expander together,
/// which is why the windows sit higher than the denoiser alone would justify.
/// The two are separable in the report — `floorReductionDb` is the expander's
/// alone.
fn snr_improved(min: f64, max: f64, note: &'static str) -> Expectation {
    Expectation {
        metric: "segmentSnrGainDb",
        min: Some(min),
        max: Some(max),
        because: note,
    }
}

/// Below the limiter ceiling, with a hair of slack for float rounding.
fn under_ceiling() -> Expectation {
    Expectation::max(
        "outputPeakDbfs",
        -0.95,
        "the −1 dBFS limiter ceiling must hold even when segments are boosted",
    )
}

/// The ceiling that actually matters.
///
/// Between two samples the waveform a converter reconstructs can overshoot both
/// of them, so a sample-peak ceiling of −1 dBFS routinely passes material that
/// hits −0.3 dBTP on the way out — and lossy encoders, which reconstruct the
/// same inter-sample content, clip on it. The limiter detects on the
/// 4x-oversampled envelope so this holds too.
fn under_true_peak_ceiling() -> Expectation {
    Expectation::max(
        "outputTruePeakDbfs",
        -0.95,
        "the ceiling is only meaningful as a true-peak ceiling; a sample-peak \
         limiter leaves inter-sample overshoot for the converter to clip",
    )
}

fn on_target() -> Expectation {
    Expectation::max(
        "segmentLufsError",
        1.0,
        "every speech segment should land within 1 LU of the target",
    )
}

/// Bit-identity, expressed as a bound on an energy ratio.
///
/// `changeDb` is −∞ for an untouched signal, so any real number at all means
/// something wrote to the samples. −300 is the bound rather than −∞ so the
/// message says "essentially nothing changed" rather than depending on an
/// exact float comparison.
fn untouched(because: &'static str) -> Expectation {
    Expectation::max("changeDb", -300.0, because)
}

/// Every case in the corpus.
///
/// `backends` decides which of them can run: a case needing a backend this
/// build does not have is marked unavailable rather than dropped.
pub fn all(backends: &Backends) -> Vec<EvalCase> {
    let mut cases = vec![
        EvalCase {
            name: "clean".into(),
            description: "Three segments at moderately different levels, quiet floor".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target(),
                under_ceiling(),
                under_true_peak_ceiling(),
                snr_expanded(),
                Expectation::max(
                    "lufsError",
                    1.5,
                    "with every segment on target the programme loudness should be too",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "bypass-null".into(),
            description: "The same programme with every stage bypassed".into(),
            // Every stage, by name from the chain itself — a new stage added to
            // DEFAULT_CHAIN is covered by this null test automatically.
            chain: build_chain(&ChainOptions {
                bypass: DEFAULT_CHAIN.iter().map(|n| (*n).to_string()).collect(),
                ..ChainOptions::default()
            })
            .expect("the default chain names only stages that exist"),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal).segments(speech.segments)
            }),
            expectations: vec![untouched(
                "a bypassed chain must return the input untouched; bit-identical \
                 gives −∞, so any real number here means something wrote to the signal",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "level-drift".into(),
            description: "Segments 25 LU apart — the case the leveller exists for".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-40.0, -15.0, -33.0]).build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target()
                    .with_max(1.5)
                    .because("large corrections may cost a little accuracy"),
                under_ceiling(),
                snr_expanded().with_max(15.0).because(
                    "the same expander check as elsewhere, with headroom for a \
                     measurement artefact this case makes unavoidable: adjacent \
                     segments here differ by ~18 dB of gain, and the noise sample \
                     sits in the ramp between them, so it is measured a fraction of \
                     that difference away from the speech it is compared against. \
                     Nothing may reduce SNR, and nothing may exceed the expander's \
                     range by more than that artefact explains",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "quiet".into(),
            description: "A whole recording 22 dB too quiet".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-45.0, -44.0, -46.0]).floor(-78.0).build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target(),
                under_ceiling(),
                snr_improved(
                    8.0,
                    16.0,
                    "the denoiser's own contribution is small — the floor sits ~31 dB \
                     down, so its clean-source taper gives it only a few dB, which is \
                     intended rather than a shortfall — and the expander supplies the \
                     rest. The upper bound matters as much as the lower: \
                     over-delivering means something is reaching past what the source \
                     supports, which is where artefacts come from",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "noisy-20db".into(),
            description: "Broadband noise 20 dB below programme".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(add_noise(&speech.signal, 20.0, 991))
                    .reference(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target()
                    .with_max(1.5)
                    .because("noise costs the measurement a little accuracy"),
                under_ceiling(),
                snr_improved(
                    16.0,
                    26.0,
                    "a 20 dB floor is squarely in range for the denoiser's full 12 dB, \
                     and the expander then finds the gaps it leaves. The upper bound \
                     matters as much as the lower: over-delivering means something is \
                     reaching past what the source supports",
                ),
                Expectation::min(
                    "siSdrGainDb",
                    -8.0,
                    "kept as a collapse detector, not as a measure of the denoiser. It \
                     is confounded here: the reference goes through the same chain, but \
                     the chain is input-dependent — the clean reference is quiet enough \
                     that the denoiser backs off, while the noisy input gets the full \
                     12 dB — so the two take different paths and the score reflects \
                     that as much as any damage. `segmentSnrGainDb` is the honest \
                     number for this stage",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "noisy-6db".into(),
            description: "Broadband noise only 6 dB below programme — the hard case".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(add_noise(&speech.signal, 6.0, 7717))
                    .reference(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                under_ceiling(),
                snr_improved(
                    6.0,
                    14.0,
                    "at 6 dB SNR there is plenty to remove. The upper bound matters as \
                     much as the lower: over-delivering means something is reaching \
                     past what the source supports",
                ),
                Expectation::min(
                    "siSdrGainDb",
                    0.0,
                    "the one case where SI-SDR is not confounded: at 6 dB SNR there is \
                     so much noise that the denoiser engages on the reference too, and \
                     the score must actually improve. If removing noise this thick does \
                     not move the signal closer to clean, the stage is not working",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "clicky".into(),
            description: "Clean programme with 40 sharp clicks scattered through it".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                let clicked = add_clicks(&speech.signal, &clicks());
                CaseInput::new(clicked.signal)
                    .reference(speech.signal)
                    .segments(speech.segments)
                    .clicks(clicked.positions)
                    .target()
            }),
            expectations: vec![
                under_ceiling(),
                on_target().with_max(1.5).because(
                    "phase 1 found clicks were breaking segmentation itself — a click \
                     in a pause lifted it over the silence threshold and merged two \
                     segments at different levels (~8 LU of error). With de-click ahead \
                     of the leveller the pauses are clean again, so levelling must be \
                     back on target",
                ),
                Expectation::min(
                    "siSdrGainDb",
                    10.0,
                    "repairing the clicks should recover most of the 20 dB they cost; \
                     well short of this means bursts are being missed or repairs are poor",
                ),
                // Click audibility is bounded on `clicky-stage` instead: at
                // full chain the output-vs-processed-reference comparison at
                // click sites is dominated by micro-differences in the
                // leveller's silence boundaries between the two renders, which
                // segmentLufsError already bounds — not by surviving clicks.
            ],
            unavailable: None,
        },
        EvalCase {
            name: "clicky-stage".into(),
            description: "The same clicks, de-click alone — the repair itself, unconfounded"
                .into(),
            chain: only(&["declick"]),
            build: Box::new(|| {
                // 2x the waveform peak, 50 ms apart. The corpus voice's glottal
                // source is several times more impulsive than a real one (a
                // Rosenberg pulse stops dead at closure), so a click that would
                // tower over a real voice's excitation is barely above this
                // voice's, and the de-clicker's pulse-train veto — correctly —
                // leaves such a click alone. Clicks that stand out from *this*
                // voice the way real clicks do from a real one are therefore
                // louder; the real test of sensitivity is a fixture.
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                let clicked = add_clicks(&speech.signal, &clicks());
                CaseInput::new(clicked.signal)
                    .reference(speech.signal)
                    .clicks(clicked.positions)
            }),
            expectations: vec![
                Expectation::max(
                    "outputClickResidualDb",
                    2.0,
                    "no repair may poke meaningfully above the peaks already present \
                     around it (±10 ms) — that is what makes a click audible. The bound \
                     sits just above the theoretical floor: at a pause site the \
                     residual of even a perfect repair is the unknowable noise \
                     realisation that was under the click, which lands at ~0 dB on this \
                     peak-vs-local-peak measure. A missed click reads +40",
                ),
                Expectation::max(
                    "changeDb",
                    -10.0,
                    "repairing ~40 bursts of a few samples each must leave the rest of \
                     the file untouched. The bound is loose because the metric is an \
                     energy ratio, not a sample count: 400 repaired samples out of 600k \
                     is 6e-4 of the file, but each carries click-sized energy (2x the \
                     peak here) against a crest-heavy programme, so a *correct* repair \
                     lands near −13 dB. Transparency is asserted properly on \
                     `clean-declick`, where nothing should change",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "stereo".into(),
            description: "Two-channel programme, to keep the multi-channel paths honest".into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).stereo().build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![on_target(), under_ceiling(), snr_expanded()],
            unavailable: None,
        },
        EvalCase {
            name: "rate-44k1".into(),
            description: "44.1 kHz source — must not be resampled by a rate-agnostic chain"
                .into(),
            chain: chain(),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).rate(44_100).build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target(),
                under_ceiling(),
                Expectation::max(
                    "resampled",
                    0.0,
                    "no enabled stage requires a fixed rate, so conversion would be pure loss",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "clean-declick".into(),
            description: "Clean speech through de-click alone — the transparency check".into(),
            chain: only(&["declick"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal).segments(speech.segments)
            }),
            expectations: vec![Expectation::max(
                "changeDb",
                -60.0,
                "most material has no clicks at all, so the cost of a de-clicker is \
                 measured on clean audio, where it must do essentially nothing. Voiced \
                 speech is driven by a glottal pulse every pitch period — impulsive \
                 excitation that an outlier detector will happily mistake for clicks — \
                 so this is the bound that keeps the analysis block short enough for \
                 those pulses to set their own threshold",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "clean-denoise".into(),
            description: "Clean speech through the classical denoiser alone — it should decline"
                .into(),
            chain: only_with(&["denoise"], vec![("denoise".into(), spectral())]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![untouched(
                "this programme's floor sits ~32 dB below the speech, past the point \
                 where the clean-source taper has scaled the reduction to zero, so the \
                 stage must return the signal itself rather than a slightly processed \
                 copy. Any real number means the taper stopped working",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "uneven".into(),
            description: "Speech whose level drifts within each phrase — compressor alone".into(),
            chain: only(&["compress"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                // A slow swell inside each spurt: the unevenness a segment
                // leveller cannot see, because it is one gain per segment and
                // this moves under it.
                let mut uneven = speech.signal.clone();
                for channel in uneven.channels_mut() {
                    for segment in &speech.segments {
                        let span = (segment.end - segment.start) as f64;
                        for (i, sample) in channel[segment.start..segment.end].iter_mut().enumerate()
                        {
                            let t = i as f64 / span;
                            // −6 dB to +6 dB across the phrase.
                            *sample *= 10f64.powf((-6.0 + 12.0 * t) / 20.0) as f32;
                        }
                    }
                }
                CaseInput::new(uneven)
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "loudnessRangeReductionLu",
                    1.0,
                    "loudness range is what a compressor is for, so it is what the \
                     compressor is measured on. A phrase swelling 12 dB is exactly the \
                     unevenness the leveller cannot reach — one gain per segment moves \
                     with it rather than against it. Under 1 LU of reduction means the \
                     threshold has drifted above the programme and the stage is idling",
                ),
                Expectation::max(
                    "compressorMaxReductionDb",
                    12.0,
                    "the cap exists so a mis-set threshold cannot turn the stage into a \
                     fader. If this ever sits at the cap the curve is wrong, not the cap",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "even-compress".into(),
            description: "Already-even speech through the compressor — it should decline".into(),
            chain: only(&["compress"]),
            build: Box::new(|| {
                // Three spurts at the same level with no drift: nothing to even
                // out.
                let speech = Programme::new([-23.0, -23.0, -23.0]).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![untouched(
                "compressing material that is already even costs whatever life it has \
                 left and buys nothing, so the stage measures the loudness range first \
                 and declines below minLoudnessRangeLu. Bit-identical output is the \
                 check that it does — a compressor with no off switch is a sound, not a \
                 tool",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "floor-expand".into(),
            description: "Audible room tone between words — expander alone".into(),
            chain: only(&["expand"]),
            build: Box::new(|| {
                // A floor 30 dB down: close enough to the voice to be heard in
                // the gaps.
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-52.0).build();
                // The same programme with the floor driven out of hearing. Same
                // seed, so the speech is sample-identical and the only
                // difference is the noise — which makes this a genuine clean
                // reference rather than a copy of the input, and lets SI-SDR
                // say whether the speech survived.
                let quiet = Programme::new([-30.0, -18.0, -25.0]).floor(-100.0).build();
                CaseInput::new(speech.signal)
                    .reference(quiet.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "floorReductionDb",
                    3.0,
                    "the whole job: push the floor down in the gaps, where no speech is \
                     masking it. The reference this was measured from manages 3.6 dB of \
                     programme-to-floor on real material, so 3 dB is the floor of useful",
                ),
                Expectation::between(
                    "segmentSnrGainDb",
                    3.0,
                    14.0,
                    "unlike every other stage measured this way, an expander is \
                     *supposed* to move segment-local SNR — that is its mechanism, not \
                     a side effect. It attenuates the quiet parts, and the quiet parts \
                     inside a segment are the gaps between words. The upper bound is \
                     what keeps it a bound: rangeDb caps attenuation at 12 dB, so \
                     anything past 14 means the cap has stopped holding",
                ),
                Expectation::min(
                    "siSdrGainDb",
                    0.0,
                    "the check that it removes noise rather than speech. Scored against \
                     the same programme with an inaudible floor, so a stage that chewed \
                     word onsets to get its floor reduction would move away from the \
                     reference even while `floorReductionDb` looked healthy",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "clean-expand".into(),
            description: "A recording whose floor is already deep — the expander should decline"
                .into(),
            chain: only(&["expand"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-85.0).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![untouched(
                "a floor already more than cleanFloorDb below the programme has nothing \
                 left worth pushing, and expanding anyway risks chewing the quiet ends \
                 of words for no audible gain. Bit-identical or the decline is not working",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "warm-voicing".into(),
            description: "The measured tonal curve, applied to clean speech — it must stay a whisper"
                .into(),
            chain: only_with(
                &["eq"],
                vec![("eq".into(), params(json!({ "voicing": "warm" })))],
            ),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![Expectation::min(
                "outputSiSdrDb",
                12.0,
                "voicing is a taste rather than a correction, so the only honest bound \
                 on it is that it stays small. The curve it applies is +1 dB at 95 Hz \
                 and −1.1 dB at 3.4 kHz — recovered by measurement, and much gentler \
                 than a first pass suggested, because most of what looked like a \
                 de-harshing dip turned out to be that service's per-band noise \
                 suppression rather than its EQ. A voicing that scores below this has \
                 stopped being a tilt and become a filter",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "clean-eq".into(),
            description: "Clean speech through EQ alone — an auto-EQ that always acts is not corrective"
                .into(),
            // Voicing off: this case is about the corrective fitter, and the
            // two are separate jobs. A fixed tilt riding on top would make the
            // number measure both and bound neither. The voicing has
            // `warm-voicing` to itself.
            chain: only_with(
                &["eq"],
                vec![("eq".into(), params(json!({ "voicing": "neutral" })))],
            ),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal).segments(speech.segments)
            }),
            expectations: vec![Expectation::between(
                "spectralFlatteningDb",
                -0.5,
                1.5,
                "uncoloured speech has nothing to correct; the EQ may shave a little \
                 but must not reshape a voice that arrived fine, and must never make \
                 the spectrum less even than it found it",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "boxy".into(),
            description: "Speech through room resonances and sub-bass rumble".into(),
            // Voicing off, for the same reason as `clean-eq`.
            chain: only_with(
                &["eq"],
                vec![("eq".into(), params(json!({ "voicing": "neutral" })))],
            ),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                // A boxy low-mid mode and a harsh upper-mid peak, plus traffic
                // underneath.
                let coloured = colour(&speech.signal, &[(240.0, 9.0, 3.0), (3200.0, 7.0, 3.5)]);
                CaseInput::new(add_rumble(&coloured, 38.0, 0.02))
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "spectralFlatteningDb",
                    1.5,
                    "two injected resonances and a rumble tone are exactly what \
                     corrective EQ is for; if this does not measurably flatten, the \
                     stage is not working",
                ),
                Expectation::min(
                    "segmentSnrGainDb",
                    -1.0,
                    "removing colouration must not cost signal-to-noise",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "hot".into(),
            description: "Quiet crest-heavy speech boosted hard into the ceiling".into(),
            chain: chain(),
            build: Box::new(|| {
                // Quiet enough to need ~20 dB of boost, so the limiter has real
                // work to do and inter-sample overshoot is where the ceiling
                // gets decided.
                let speech = Programme::new([-43.0, -41.0, -44.0]).floor(-80.0).build();
                CaseInput::new(speech.signal)
                    .segments(speech.segments)
                    .target()
            }),
            expectations: vec![
                on_target(),
                under_ceiling(),
                under_true_peak_ceiling(),
                snr_expanded().because(
                    "limiting hard must not change speech-to-noise within a segment \
                     beyond what the expander is already doing — the limiter works on \
                     peaks, and peaks are not where a noise floor lives, so it must \
                     contribute nothing to this number in either direction",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "reverberant".into(),
            description: "Speech in a live room, dereverb stage alone".into(),
            chain: only(&["dereverb"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-75.0).build();
                let live = add_reverb(
                    &speech.signal,
                    &RirOptions {
                        rt60_sec: 0.7,
                        direct_to_reverb_db: 4.0,
                        ..RirOptions::default()
                    },
                );
                CaseInput::new(live)
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "decayShorteningMs",
                    10.0,
                    "the room's tail must measurably shorten. Single-channel WPE is a \
                     modest tool — roughly 12% off the decay here — and the bound is \
                     set to catch it stopping working, not to claim it transforms the \
                     recording",
                ),
                Expectation::min(
                    "siSdrGainDb",
                    0.0,
                    "whatever it removes must move the signal toward the dry reference, \
                     not away",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "dry-dereverb".into(),
            description: "A dry recording through the dereverb stage — it should decline to act"
                .into(),
            chain: only(&["dereverb"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-75.0).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![untouched(
                "dry speech decays in ~35 ms, far below the engagement threshold, so \
                 the stage must pass the signal through untouched rather than \
                 processing it slightly. WPE is not transparent enough to run \
                 unconditionally: at the pipeline's usual frame size it reduces a dry \
                 recording to 1 dB SI-SDR",
            )],
            unavailable: None,
        },
        EvalCase {
            name: "ringing".into(),
            description: "Speech with a sustained resonance riding on it, dynamic EQ alone".into(),
            chain: only(&["dyneq"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-75.0).build();
                let mut ringing = speech.signal.clone();
                for channel in ringing.channels_mut() {
                    for (i, sample) in channel.iter_mut().enumerate() {
                        *sample += (0.02
                            * (std::f64::consts::TAU * 2500.0 * i as f64 / f64::from(SR)).sin())
                            as f32;
                    }
                }
                CaseInput::new(ringing)
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "siSdrGainDb",
                    0.5,
                    "a sustained resonance is exactly what dynamic EQ is for; \
                     suppressing it must move the signal toward the clean reference",
                ),
                Expectation::min(
                    "spectralFlatteningDb",
                    0.5,
                    "the resonance should measurably flatten out of the spectrum",
                ),
            ],
            unavailable: None,
        },
        EvalCase {
            name: "clean-dyneq".into(),
            description: "Clean speech through dynamic EQ alone — suppression, not reshaping"
                .into(),
            chain: only(&["dyneq"]),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).floor(-75.0).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![
                Expectation::min(
                    "outputSiSdrDb",
                    15.0,
                    "unlike the other spectral stages this one cannot decline to act — \
                     it is per-frame by nature — so its transparency has to be measured \
                     rather than arranged. A voice with no resonances must come back \
                     essentially intact",
                ),
                Expectation::between(
                    "spectralFlatteningDb",
                    -0.5,
                    1.5,
                    "nothing to suppress means nothing much should change",
                ),
            ],
            unavailable: None,
        },
    ];

    cases.extend(model_cases(backends));
    cases
}

fn clicks() -> ClickOptions {
    ClickOptions {
        count: 40,
        relative_amplitude: 2.0,
        width_samples: 3,
        min_gap_sec: 0.05,
        seed: 5150,
    }
}

/// Cases that exercise a model backend rather than the classical one.
///
/// They are skipped, loudly, when the backend is not in this build — which is
/// the normal state of a fresh checkout, since nothing here bundles a 9 MB
/// model.
///
/// There is no synthetic *quality* case here on purpose. A trained denoiser has
/// learned what speech looks like, and the corpus voice is not speech: it is a
/// glottal pulse train through a fixed formant filter, with no coarticulation,
/// no fricatives worth the name and no prosody. Scoring the model on it would
/// measure how far that synthesis sits from the model's training distribution,
/// which is a fact about the corpus rather than about the model. What can
/// honestly be asserted synthetically is that the backend runs, refuses rates
/// it cannot handle, and does not eat the programme; quality belongs on a
/// fixture.
fn model_cases(backends: &Backends) -> Vec<EvalCase> {
    let unavailable = match backends.get("onnx") {
        None => Some("the onnx backend is not registered in this build".to_string()),
        Some(backend) => backend.unavailable_reason(SR),
    };

    vec![
        EvalCase {
            name: "ood-denoise-onnx".into(),
            description: "Out-of-distribution synthetic speech, model denoiser alone".into(),
            chain: only_with(
                &["denoise"],
                vec![("denoise".into(), params(json!({ "backends": ["onnx"] })))],
            ),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(add_noise(&speech.signal, 20.0, 991))
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![Expectation::max(
                "programmeLossDb",
                3.0,
                "the honest synthetic assertion about a trained model: whatever it \
                 removes, it must not be the programme. Quality is not asserted here \
                 because the corpus voice is out of distribution — a glottal pulse \
                 train is not speech — and a number measured on it would be a fact \
                 about the corpus. What it does catch is a model that has started \
                 subtracting whatever it does not recognise",
            )],
            unavailable: unavailable.clone(),
        },
        EvalCase {
            name: "clean-denoise-onnx".into(),
            description: "Clean speech through the model denoiser — it should back off too".into(),
            chain: only_with(
                &["denoise"],
                vec![("denoise".into(), params(json!({ "backends": ["onnx"] })))],
            ),
            build: Box::new(|| {
                let speech = Programme::new([-30.0, -18.0, -25.0]).build();
                CaseInput::new(speech.signal.clone())
                    .reference(speech.signal)
                    .segments(speech.segments)
            }),
            expectations: vec![untouched(
                "the clean-source taper is the stage's, not a backend's, so it must \
                 hold whichever backend is selected. A model that runs anyway on a \
                 source with nothing to remove is a model inventing detail for no reason",
            )],
            unavailable,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_case_has_a_distinct_name() {
        // The runner keys the baseline by name, so a duplicate would silently
        // overwrite another case's numbers.
        let cases = all(&leveller_stages::default_backends());
        let mut seen = HashSet::new();
        for case in &cases {
            assert!(seen.insert(case.name.clone()), "duplicate: {}", case.name);
        }
        assert!(cases.len() >= 20, "the corpus should not shrink by accident");
    }

    #[test]
    fn every_expectation_says_why() {
        // A bound you cannot justify in a sentence is a bound that will be
        // quietly relaxed the first time it fails.
        for case in all(&leveller_stages::default_backends()) {
            for expectation in &case.expectations {
                assert!(
                    expectation.because.len() > 40,
                    "{} / {}: the reason is too short to be one",
                    case.name,
                    expectation.metric
                );
                assert!(
                    expectation.min.is_some() || expectation.max.is_some(),
                    "{} / {}: a bound with neither end is not a bound",
                    case.name,
                    expectation.metric
                );
            }
        }
    }

    #[test]
    fn the_model_cases_are_unavailable_in_a_build_without_the_backend() {
        // Skipped, not dropped: a check nobody runs must not look like one that
        // passed.
        let cases = all(&leveller_stages::default_backends());
        let onnx: Vec<&EvalCase> = cases.iter().filter(|c| c.name.ends_with("onnx")).collect();
        assert!(!onnx.is_empty(), "the model cases should still be listed");
        for case in onnx {
            assert!(
                case.unavailable.is_some(),
                "{} claims it can run without the backend",
                case.name
            );
        }
    }
}
