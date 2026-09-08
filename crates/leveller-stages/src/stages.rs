//! The eight stages, in chain order.
//!
//! Each is a thin adapter: it asks the analyzer what it needs to know, decides
//! whether it has anything to do, calls into the DSP, and reports what it
//! decided. The decisions live here; the processing does not.

use std::sync::Arc;

use leveller_dsp::{
    Signal, apply_cascade,
    biquad::Biquad,
    declick::{DeclickOptions, declick},
    dereverb::{DereverbOptions, dereverb},
    dynamics::{self, Compressor, Expander, Timing},
    dyneq::{DynEqOptions, dynamic_eq},
    eq::{self, EqBand, EqFitOptions},
    leveller::{self, LevellerOptions},
    loudness::Weighted,
    ltas::{self, LtasOptions},
    reverbtime::reverb_decay,
};
use leveller_pipeline::{Stage, StageContext, StageError, StageOutput, params_of, report_of};
use serde_json::{Value, json};

use crate::backend::{Backends, DenoiseRequest, Skipped};
use crate::support::{pause_loudness, pause_ranges, speech_ranges};

// ---------------------------------------------------------------- de-click --

/// De-click.
///
/// First in the chain, and the ordering earns its keep twice over: clicks are
/// out-of-distribution impulses for any denoiser, which smears them into chirps
/// rather than removing them; and — as the corpus showed — a click landing in a
/// pause can lift it over the silence threshold and merge two speech segments,
/// so de-clicking also repairs the leveller's segmentation.
///
/// Rate-agnostic: the model order is chosen per sample rate rather than
/// demanding a conversion.
pub struct Declick;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeclickReport {
    bursts: Vec<leveller_dsp::declick::ClickBurst>,
    detected: usize,
    repaired: usize,
    samples_repaired: usize,
    repaired_fraction: f64,
    aborted: bool,
    vetoed: usize,
}

impl Stage for Declick {
    fn name(&self) -> &'static str {
        "declick"
    }
    fn description(&self) -> &'static str {
        "Detect impulsive clicks and rebuild them by AR interpolation"
    }
    fn default_params(&self) -> Value {
        report_of(&DeclickOptions::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: DeclickOptions = params_of(self.name(), params)?;
        let result = declick(signal.channels(), signal.sample_rate(), &params);
        ctx.progress(1.0);

        let report = report_of(&DeclickReport {
            detected: result.bursts.len(),
            bursts: result.bursts,
            repaired: result.repaired,
            samples_repaired: result.samples_repaired,
            repaired_fraction: result.repaired_fraction,
            aborted: result.aborted,
            vetoed: result.vetoed,
        });

        // An abort changes nothing, so hand back the very signal given.
        if result.aborted {
            return Ok(StageOutput::unchanged(signal, report));
        }
        Ok(StageOutput::new(
            Signal::new(signal.sample_rate(), result.channels),
            report,
        ))
    }
}

// ----------------------------------------------------------------- denoise --

/// Noise reduction.
///
/// After de-click and before the EQ, because denoising changes the spectrum the
/// EQ would otherwise be fitting a curve to.
///
/// The stage measures what it achieved rather than assuming it: the floor in
/// the pauses is measured before and after, and that number goes in the report.
/// A denoiser that claims 12 dB and delivers 3 is a bug you cannot see any
/// other way.
pub struct Denoise {
    backends: Backends,
}

impl Denoise {
    pub fn new(backends: Backends) -> Self {
        Self { backends }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DenoiseParams {
    /// How far to push the noise floor down, in dB.
    ///
    /// Deliberately modest. Removing all of the noise is both impossible and
    /// undesirable — a recording with a natural, quiet floor sounds like a
    /// recording, while one scrubbed to digital silence between words sounds
    /// broken, and the artefacts needed to get there are worse than the noise.
    pub reduction_db: f64,
    /// Programme-to-floor distance, in dB, at which a recording counts as
    /// already clean and the stage backs off to nothing.
    ///
    /// Denoising is not free: it modifies the signal, and on material that was
    /// already quiet the modification is all you get. Measured on the corpus,
    /// running 12 dB of reduction over a recording whose floor sits 35 dB down
    /// *lowered* SI-SDR against the clean reference — the processing was the
    /// only thing it changed. So the reduction is scaled by how much noise is
    /// actually there: at 20 dB the full amount, at 35 dB none, tapering
    /// between. A backend may state its own threshold and does when it is more
    /// transparent than this default assumes.
    pub clean_snr_db: f64,
    /// How much gated programme loudness a backend may cost before its output
    /// is thrown away and the input returned instead, in dB.
    ///
    /// A denoiser is supposed to remove what is *around* the speech. Losing a
    /// little programme loudness is legitimate — on a genuinely noisy source
    /// the noise was contributing to the measurement — but losing a lot means
    /// the backend has decided the speech is the noise.
    ///
    /// This is not hypothetical. DeepFilterNet3 does exactly that to this
    /// project's synthetic corpus, which is speech-*shaped* rather than speech:
    /// it reads a harmonic stack with formants as noise and pulls the programme
    /// down by 10 dB. On real recordings the same model moves programme
    /// loudness by under 0.7 dB. So the guard costs nothing on material a model
    /// understands, and turns a wrecked render into a declined stage on
    /// material it does not — which matters more for a trained backend than a
    /// classical one, because a trained one fails by confidently rewriting
    /// rather than by doing too little.
    pub max_programme_loss_db: f64,
    /// Backend order to try. The first that can actually run wins.
    pub backends: Vec<String>,
}

impl Default for DenoiseParams {
    fn default() -> Self {
        Self {
            reduction_db: 12.0,
            clean_snr_db: 35.0,
            // Above what noise removal alone can explain: at 6 dB SNR the noise
            // is a fifth of the total power, so removing all of it costs about
            // 1 dB of measured loudness, and 3 leaves room for worse sources.
            max_programme_loss_db: 3.0,
            backends: crate::backend::default_preference(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DenoiseReport {
    /// Which backend actually ran, or null when none could.
    backend: Option<String>,
    /// Backends that were preferred but unavailable, with the reason.
    skipped: Vec<Skipped>,
    info: Value,
    reduction_requested_db: f64,
    /// What was actually asked of the backend after the clean-source taper.
    reduction_applied_db: f64,
    input_snr_db: f64,
    /// What the floor in the pauses actually did, in dB. Positive is quieter.
    reduction_achieved_db: f64,
    noise_floor_before_lufs: f64,
    noise_floor_after_lufs: f64,
    /// Gated programme loudness the backend cost, in dB.
    programme_loss_db: f64,
    /// Set when a backend ran and its output was rejected, saying why. The
    /// audio returned is then the input, unchanged.
    rejected: Option<String>,
    /// True when no backend could run and the audio was passed through.
    skipped_entirely: bool,
}

/// Scale the requested reduction by how much noise is actually present: full
/// strength on a noisy source, nothing on a clean one, tapering between.
fn adapt_reduction(requested_db: f64, snr_db: f64, clean_snr_db: f64) -> f64 {
    if !snr_db.is_finite() {
        return requested_db;
    }
    let headroom = (clean_snr_db - snr_db).max(0.0);
    requested_db.min(headroom).max(0.0)
}

impl Stage for Denoise {
    fn name(&self) -> &'static str {
        "denoise"
    }
    fn description(&self) -> &'static str {
        "Attenuate steady background noise (spectral suppression, or a model when present)"
    }
    fn default_params(&self) -> Value {
        report_of(&DenoiseParams::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: DenoiseParams = params_of(self.name(), params)?;
        let pauses = pause_ranges(&ctx.analysis.default_silence().regions);
        let (backend, skipped) = self
            .backends
            .resolve(&params.backends, signal.sample_rate());

        let before = pause_loudness(&signal, &pauses);
        let programme = ctx.analysis.integrated_lufs();
        let input_snr_db = if programme.is_finite() && before.is_finite() {
            programme - before
        } else {
            f64::NAN
        };
        // A backend that states its own threshold knows better than the stage
        // does how transparent it is on material that barely needs treating.
        let clean_snr_db = backend
            .as_ref()
            .and_then(|b| b.clean_snr_db())
            .unwrap_or(params.clean_snr_db);
        let applied = adapt_reduction(params.reduction_db, input_snr_db, clean_snr_db);

        let mut report = DenoiseReport {
            backend: None,
            skipped,
            info: json!({}),
            reduction_requested_db: params.reduction_db,
            reduction_applied_db: applied,
            input_snr_db,
            reduction_achieved_db: 0.0,
            noise_floor_before_lufs: before,
            noise_floor_after_lufs: before,
            programme_loss_db: 0.0,
            rejected: None,
            skipped_entirely: true,
        };

        // Nothing worth removing: hand back the signal itself, so a clean
        // recording leaves this stage bit-identical rather than merely similar.
        let Some(backend) = backend.filter(|_| applied > 0.0) else {
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        };

        ctx.progress(0.1);
        let response = backend.process(&DenoiseRequest {
            channels: signal.channels(),
            sample_rate: signal.sample_rate(),
            pauses: &pauses,
            reduction_db: applied,
        });
        ctx.progress(0.9);

        let output = Signal::new(signal.sample_rate(), response.channels);
        let after = pause_loudness(&output, &pauses);

        // What the backend cost the programme itself, measured the way the
        // leveller measures loudness: gated, so pauses do not drag it down.
        let output_programme = Weighted::new(output.channels(), output.sample_rate()).integrated();
        let programme_loss_db = if programme.is_finite() && output_programme.is_finite() {
            programme - output_programme
        } else {
            0.0
        };

        report.backend = Some(backend.name().to_string());
        report.info = response.info;
        report.programme_loss_db = programme_loss_db;
        report.skipped_entirely = false;
        ctx.progress(1.0);

        if programme_loss_db > params.max_programme_loss_db {
            report.rejected = Some(format!(
                "the {} backend cost {programme_loss_db:.1} dB of programme loudness \
                 (limit {} dB) — it is removing the speech, not the noise, so its output \
                 was discarded",
                backend.name(),
                params.max_programme_loss_db
            ));
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        }

        report.reduction_achieved_db = if before.is_finite() && after.is_finite() {
            before - after
        } else {
            0.0
        };
        report.noise_floor_after_lufs = after;
        Ok(StageOutput::new(output, report_of(&report)))
    }
}

// ---------------------------------------------------------------- dereverb --

/// Dereverberation.
///
/// Between the denoiser and the EQ. After denoising because the method's
/// statistics are cleaner without a noise floor under them, and before the EQ
/// because removing a room changes the spectrum the EQ would otherwise fit to.
///
/// It engages only when the recording is actually reverberant, on the same
/// principle as the denoiser: single-channel weighted prediction error is not
/// free, and running it over a dry recording buys nothing while measurably
/// altering the voice. Reverberation is detected blind, from how long the
/// signal takes to decay after speech stops.
///
/// On expectations: this is a modest tool. Measured on the corpus it takes
/// about 12% off the decay time and gains about 1 dB of SI-SDR against the dry
/// reference. Most published results that sound dramatic are multi-channel,
/// where the spatial information does the work. What it does offer is that it
/// cannot invent speech — it only subtracts a linear prediction — so it will
/// never put words in someone's mouth, which a generative enhancer can.
pub struct Dereverb;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DereverbParams {
    #[serde(flatten)]
    pub dsp: DereverbOptions,
    /// Engage only when the measured decay exceeds this, in milliseconds.
    ///
    /// Dry speech measures around 35 ms — that is the talker's own
    /// articulation, not a room. A noticeably live room lands past 150 ms. The
    /// default sits between them so ordinary recordings pass through
    /// untouched. Note the number is only meaningful against the 5 ms analysis
    /// frame the measurement uses: at 2 ms the same audio reads a third of
    /// this.
    pub min_decay_ms: f64,
}

impl Default for DereverbParams {
    fn default() -> Self {
        Self {
            dsp: DereverbOptions::default(),
            min_decay_ms: 90.0,
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DereverbReport {
    decay_before_ms: Option<f64>,
    decay_after_ms: Option<f64>,
    /// True when the recording was dry enough to leave alone.
    skipped: bool,
    bins_processed: usize,
}

impl Stage for Dereverb {
    fn name(&self) -> &'static str {
        "dereverb"
    }
    fn description(&self) -> &'static str {
        "Suppress late reverberation by weighted prediction error (no model)"
    }
    fn default_params(&self) -> Value {
        report_of(&DereverbParams::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: DereverbParams = params_of(self.name(), params)?;
        let decay_before_ms = reverb_decay(&signal);

        // A dry recording is returned as the same object, so this stage is
        // bit-transparent on material it should not touch.
        if decay_before_ms.is_none_or(|ms| ms < params.min_decay_ms) {
            return Ok(StageOutput::unchanged(
                signal,
                report_of(&DereverbReport {
                    decay_before_ms,
                    decay_after_ms: decay_before_ms,
                    skipped: true,
                    bins_processed: 0,
                }),
            ));
        }

        ctx.progress(0.1);
        let result = dereverb(signal.channels(), &params.dsp);
        let output = Signal::new(signal.sample_rate(), result.channels);
        ctx.progress(1.0);

        Ok(StageOutput::new(
            output.clone(),
            report_of(&DereverbReport {
                decay_before_ms,
                decay_after_ms: reverb_decay(&output),
                skipped: false,
                bins_processed: result.bins_processed,
            }),
        ))
    }
}

// ---------------------------------------------------------------------- EQ --

/// Corrective EQ, and the voicing that rides after it.
///
/// The stage measures the long-term average spectrum over *speech* only, and
/// the pauses separately — the second gates boosts, so the EQ never buys timbre
/// with noise.
pub struct Eq;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Voicing {
    Neutral,
    Warm,
}

/// Fixed voicing curves.
///
/// `warm` is the static tone curve of a commercial mastering service, recovered
/// from a 24-minute before-and-after pair. Getting it required separating tone
/// from dynamics, and the first attempt got that wrong in an instructive way:
/// on the loudest frames its 5–6.5 kHz region reads about −3 dB, which looks
/// like a de-harshing dip. Measured only on frames where *that band* sits 25 dB
/// above its own noise floor, the same region reads −0.2 dB. The dip was its
/// per-band noise suppression, not its EQ — even a loud vowel has little
/// genuine 6 kHz content, so most frames have that band near the floor where
/// the suppressor is working. Baking it in would have made every recording
/// duller for no reason anyone could hear.
///
/// What survives that correction is very gentle: about +1 dB under 130 Hz and
/// about −1 dB across 2.5–5 kHz. Which is the finding — that service's "sound"
/// is almost entirely its dynamics, not its tone. This curve is offered because
/// it is what was asked for and it is real, not because it is where the
/// character comes from.
impl Voicing {
    pub fn bands(self) -> Vec<EqBand> {
        match self {
            Self::Neutral => Vec::new(),
            Self::Warm => vec![
                // The +1.0 dB measured at 80–101 Hz, wide enough to read as
                // weight rather than as a bump on the fundamental.
                EqBand {
                    freq: 95.0,
                    gain_db: 1.0,
                    q: 0.7,
                },
                // The −1.1 dB centred near 3.2 kHz and spanning 2.5–5 kHz.
                // This is the one that reads as "smooth": it is where a close
                // microphone in an untreated room puts its hardness.
                EqBand {
                    freq: 3_400.0,
                    gain_db: -1.1,
                    q: 0.9,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EqParams {
    #[serde(flatten)]
    pub fit: EqFitOptions,
    /// Fit and apply a rumble high-pass when the sub-bass warrants one.
    pub rumble_enabled: bool,
    pub rumble_freq: f64,
    /// How close the sub-bass may come to the voice's fundamental region before
    /// the high-pass engages, in dB.
    pub rumble_margin_db: f64,
    /// A fixed tonal tilt applied after the corrective bands.
    ///
    /// Correction and voicing are different jobs and this stage does both,
    /// deliberately kept apart. Correction is fitted per recording and removes
    /// what that room and microphone added. Voicing is the same on every file:
    /// a taste, not a measurement, applied last so it survives whatever the
    /// fitter decided.
    pub voicing: Voicing,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            fit: EqFitOptions::default(),
            rumble_enabled: true,
            rumble_freq: 80.0,
            rumble_margin_db: -12.0,
            // On by default because it is what this project is trying to sound
            // like, and because at ±1 dB it is small enough that the corrective
            // fit still does the substantive work.
            voicing: Voicing::Warm,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EqReport {
    bands: Vec<EqBand>,
    rumble_freq: Option<f64>,
    rumble_excess_db: f64,
    deviation_before_db: f64,
    deviation_after_db: f64,
    /// Frames of speech and of pause that went into the two spectra.
    speech_frames: usize,
    noise_frames: usize,
    /// True when there was too little speech to measure and nothing was done.
    skipped: bool,
    /// Voicing kept separate from correction, so the report shows tone apart
    /// from what was fitted.
    voicing: Voicing,
    voicing_bands: Vec<EqBand>,
}

impl Stage for Eq {
    fn name(&self) -> &'static str {
        "eq"
    }
    fn description(&self) -> &'static str {
        "Correct room and microphone colouration from the long-term average spectrum"
    }
    fn default_params(&self) -> Value {
        report_of(&EqParams::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: EqParams = params_of(self.name(), params)?;
        // One curve is applied to every channel, so it is fitted to the mono
        // fold rather than to whichever channel happened to be first.
        let mono = signal.to_mono();
        let silences = ctx.analysis.default_silence();
        let pauses = pause_ranges(&silences.regions);
        let speech = speech_ranges(&silences.regions, signal.len());

        let options = LtasOptions::default();
        let speech_ltas = ltas::compute(&mono, signal.sample_rate(), &speech, &options);
        ctx.progress(0.4);

        let voicing_bands = params.voicing.bands();
        let mut report = EqReport {
            bands: Vec::new(),
            rumble_freq: None,
            rumble_excess_db: 0.0,
            deviation_before_db: 0.0,
            deviation_after_db: 0.0,
            speech_frames: 0,
            noise_frames: 0,
            skipped: true,
            voicing: params.voicing,
            voicing_bands: voicing_bands.clone(),
        };

        // Too little speech to characterise. Fitting to a couple of frames
        // would be fitting to an accident, so decline — and decline the voicing
        // too, since a file with no speech in it is not one to impose a voice
        // on.
        let Some(speech_ltas) = speech_ltas else {
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        };

        // The pauses, on the same grid, to gate boosts.
        let noise_ltas = ltas::compute(&mono, signal.sample_rate(), &pauses, &options);

        let fit = eq::fit_corrective(
            &speech_ltas,
            noise_ltas.as_ref(),
            signal.sample_rate(),
            &params.fit,
        );
        let rumble = if params.rumble_enabled {
            eq::decide_rumble_filter(&speech_ltas, params.rumble_freq, params.rumble_margin_db)
        } else {
            eq::RumbleDecision {
                freq: None,
                excess_db: 0.0,
            }
        };
        ctx.progress(0.6);

        report.bands = fit.bands.clone();
        report.rumble_freq = rumble.freq;
        report.rumble_excess_db = rumble.excess_db;
        report.deviation_before_db = fit.deviation_before_db;
        report.deviation_after_db = fit.deviation_after_db;
        report.speech_frames = speech_ltas.frames;
        report.noise_frames = noise_ltas.map_or(0, |n| n.frames);
        report.skipped = false;

        // Voicing rides after correction, so the fitter's decisions are not
        // reshaped by it and the two stay separable in the report.
        let rate = f64::from(signal.sample_rate());
        let mut cascade = eq::build_cascade(&fit, &rumble, signal.sample_rate());
        cascade.extend(
            voicing_bands
                .iter()
                .map(|b| Biquad::peaking(rate, b.freq, b.gain_db, b.q)),
        );

        // Nothing worth correcting and no voice to impose.
        if cascade.is_empty() {
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        }

        let channels = signal
            .channels()
            .iter()
            .map(|c| apply_cascade(c, &cascade))
            .collect();
        ctx.progress(1.0);

        Ok(StageOutput::new(
            Signal::new(signal.sample_rate(), channels),
            report_of(&report),
        ))
    }
}

// ------------------------------------------------------------- dynamic EQ --

/// Dynamic EQ, which is also the de-esser.
///
/// After the static EQ — that one has already removed whatever colouration is
/// constant, so what reaches here is the part that comes and goes: ringing
/// vowels, sibilance, the occasional boomy plosive. Sibilance is not a
/// different problem from resonance, only a more common one in a known band,
/// so it is handled by the same mechanism with more sensitivity applied there.
pub struct DynEq;

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DynEqReport {
    max_reduction_db: f64,
    mean_reduction_db: f64,
    /// Fraction of time-frequency cells attenuated at all.
    ///
    /// Worth watching: near zero means the stage found nothing, and a large
    /// value means it is reshaping the whole recording rather than catching
    /// resonances.
    active_fraction: f64,
}

impl Stage for DynEq {
    fn name(&self) -> &'static str {
        "dyneq"
    }
    fn description(&self) -> &'static str {
        "Suppress resonances and sibilance while they occur (dynamic EQ / de-esser)"
    }
    fn default_params(&self) -> Value {
        report_of(&DynEqOptions::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: DynEqOptions = params_of(self.name(), params)?;
        let result = dynamic_eq(signal.channels(), signal.sample_rate(), &params);
        ctx.progress(1.0);

        Ok(StageOutput::new(
            Signal::new(signal.sample_rate(), result.channels),
            report_of(&DynEqReport {
                max_reduction_db: result.max_reduction_db,
                mean_reduction_db: result.mean_reduction_db,
                active_fraction: result.active_fraction,
            }),
        ))
    }
}

// ------------------------------------------------------------------ expand --

/// Downward expansion — the quiet gets quieter.
///
/// The complement to the denoiser rather than a replacement for it. A denoiser
/// works on the spectrum and removes noise that is there *while the speech is*;
/// an expander works on the level and pushes down the gaps *between* words,
/// where nothing is masking anything and the room is most exposed. Neither does
/// the other's job, which is why the commercial reference this was measured
/// against runs both.
///
/// That reference attenuates by about 11 dB at 37 dB below its programme
/// loudness and 34 dB at 43 below, a slope near 2.8:1 with the knee about 27 dB
/// under the programme. Those are the defaults, with one deliberate departure:
/// the attenuation is capped. An uncapped expander is a gate, and a recording
/// gated to digital silence between words sounds broken in a way the noise
/// never did.
///
/// It runs before the compressor for a concrete reason. Both stages place their
/// thresholds relative to the programme loudness of the audio handed to them,
/// and this one additionally decides whether to act at all from the
/// programme-to-floor distance it measures. Expanding first leaves those
/// numbers intact: it only attenuates well below the programme, far under the
/// relative gate the loudness measurement applies. Compressing first does not —
/// it pulls the programme down while leaving the floor where it was, narrowing
/// the very distance this stage reads before choosing a threshold, and can flip
/// its skip decision outright.
pub struct Expand;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExpandParams {
    /// Threshold, in dB below the programme's integrated loudness.
    ///
    /// Relative for the same reason the compressor's is: it has to mean the
    /// same thing on a quiet recording as on a hot one.
    pub threshold_below_programme_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    /// Hard cap on attenuation, in dB. This is what makes it an expander rather
    /// than a gate.
    pub range_db: f64,
    /// Time constant while the gain falls — the floor being pushed down.
    pub close_ms: f64,
    /// Time constant while the gain recovers — a word starting.
    pub open_ms: f64,
    /// Programme-to-floor distance, in dB, above which the stage declines.
    ///
    /// A recording whose floor already sits this far down has nothing left
    /// worth pushing, and expanding it would only risk chewing the quiet ends
    /// of words for no audible gain.
    pub clean_floor_db: f64,
}

impl Default for ExpandParams {
    fn default() -> Self {
        Self {
            threshold_below_programme_db: 27.0,
            ratio: 2.8,
            knee_db: 8.0,
            range_db: 12.0,
            // Opening fast matters more than closing fast: a slow open clips
            // the front of a word, which is audible, while a slow close merely
            // lets the floor linger a moment, which is not.
            close_ms: 150.0,
            open_ms: 5.0,
            clean_floor_db: 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpandReport {
    threshold_dbfs: Option<f64>,
    ratio: f64,
    input_floor_distance_db: f64,
    /// What the floor in the pauses actually did, in dB. Positive is quieter.
    floor_reduction_db: f64,
    noise_floor_before_lufs: f64,
    noise_floor_after_lufs: f64,
    max_reduction_db: f64,
    active_fraction: f64,
    /// True when the floor was already low enough to leave alone.
    skipped: bool,
}

impl Stage for Expand {
    fn name(&self) -> &'static str {
        "expand"
    }
    fn description(&self) -> &'static str {
        "Push the floor down between words, where nothing is masking it"
    }
    fn default_params(&self) -> Value {
        report_of(&ExpandParams::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: ExpandParams = params_of(self.name(), params)?;
        let pauses = pause_ranges(&ctx.analysis.default_silence().regions);
        let before = pause_loudness(&signal, &pauses);
        let programme = ctx.analysis.integrated_lufs();
        let distance = if programme.is_finite() && before.is_finite() {
            programme - before
        } else {
            f64::NAN
        };

        let mut report = ExpandReport {
            threshold_dbfs: None,
            ratio: params.ratio,
            input_floor_distance_db: distance,
            floor_reduction_db: 0.0,
            noise_floor_before_lufs: before,
            noise_floor_after_lufs: before,
            max_reduction_db: 0.0,
            active_fraction: 0.0,
            skipped: true,
        };

        // Nothing worth pushing: hand back the signal itself, so a recording
        // with an already-deep floor leaves this stage bit-identical.
        if !distance.is_finite() || distance > params.clean_floor_db {
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        }

        let threshold_dbfs = programme - params.threshold_below_programme_db;
        ctx.progress(0.1);
        let result = dynamics::apply(
            signal.channels(),
            signal.sample_rate(),
            &Expander {
                threshold_db: threshold_dbfs,
                ratio: params.ratio,
                knee_db: params.knee_db,
                range_db: params.range_db,
            },
            &Timing {
                down_ms: params.close_ms,
                up_ms: params.open_ms,
                detector_ms: Timing::default().detector_ms,
            },
        );
        ctx.progress(0.9);

        let output = Signal::new(signal.sample_rate(), result.channels);
        let after = pause_loudness(&output, &pauses);
        ctx.progress(1.0);

        report.threshold_dbfs = Some(threshold_dbfs);
        report.floor_reduction_db = if before.is_finite() && after.is_finite() {
            before - after
        } else {
            0.0
        };
        report.noise_floor_after_lufs = after;
        report.max_reduction_db = result.max_reduction_db;
        report.active_fraction = result.active_fraction;
        report.skipped = false;

        Ok(StageOutput::new(output, report_of(&report)))
    }
}

// ---------------------------------------------------------------- compress --

/// Compression.
///
/// The stage that separates a recording that has been *levelled* from one that
/// sounds *mastered*. The leveller sets one gain per speech segment, which
/// fixes the difference between a passage recorded close and one recorded far
/// away. It does nothing about the difference between the start and the end of
/// a single sentence, and that is where most of the unevenness in speech
/// actually lives.
///
/// The numbers were measured rather than chosen. A 24-minute before-and-after
/// pair from a commercial service was aligned to the sample and differenced:
/// across 14 441 frames its applied gain falls as input level rises above about
/// 2 dB under the programme loudness, at a slope corresponding to roughly
/// 1.7:1, and its gain trajectory fits a first-order smoother with a 33 ms fall
/// and a 168 ms rise. Within one continuous utterance its gain moves a median
/// of 4.5 dB, against 3.3 dB between utterances — so more than half of what it
/// does is inside a phrase, where a segment leveller cannot reach.
pub struct Compress;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompressParams {
    /// Threshold, in dB relative to the programme's integrated loudness.
    ///
    /// Relative rather than absolute so it means the same thing on a quiet
    /// recording as on a hot one — an absolute dBFS threshold would compress a
    /// loud file hard and miss a quiet one entirely. The measured reference
    /// sits about 2 dB below its own programme loudness, which puts the knee in
    /// the middle of ordinary speech rather than only on its peaks.
    pub threshold_relative_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub max_reduction_db: f64,
    /// Loudness range, in LU, below which the stage declines to act.
    ///
    /// Material that is already even has nothing for a compressor to even out,
    /// and compressing it only costs what little life it has left. Speech that
    /// genuinely needs this measures 5 LU and up; a heavily processed source
    /// can arrive at 2 and should be left alone.
    pub min_loudness_range_lu: f64,
}

impl Default for CompressParams {
    fn default() -> Self {
        Self {
            threshold_relative_db: -2.0,
            ratio: 1.7,
            knee_db: 10.0,
            attack_ms: 33.0,
            release_ms: 168.0,
            max_reduction_db: 12.0,
            min_loudness_range_lu: 3.0,
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CompressReport {
    threshold_dbfs: Option<f64>,
    ratio: f64,
    /// Loudness range before and after, in LU — what the stage is for.
    loudness_range_before_lu: f64,
    loudness_range_after_lu: f64,
    max_reduction_db: f64,
    /// Mean gain across the programme, in dB. Negative: it only ever
    /// attenuates.
    mean_gain_db: f64,
    active_fraction: f64,
    /// True when the material was already even enough to leave alone.
    skipped: bool,
}

impl Stage for Compress {
    fn name(&self) -> &'static str {
        "compress"
    }
    fn description(&self) -> &'static str {
        "Even out level within a phrase, where segment levelling cannot reach"
    }
    fn default_params(&self) -> Value {
        report_of(&CompressParams::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: CompressParams = params_of(self.name(), params)?;
        let range_before = ctx.analysis.loudness_range();
        let programme = ctx.analysis.integrated_lufs();

        let mut report = CompressReport {
            threshold_dbfs: None,
            ratio: params.ratio,
            loudness_range_before_lu: range_before,
            loudness_range_after_lu: range_before,
            max_reduction_db: 0.0,
            mean_gain_db: 0.0,
            active_fraction: 0.0,
            skipped: true,
        };

        if !programme.is_finite() || range_before < params.min_loudness_range_lu {
            return Ok(StageOutput::unchanged(signal, report_of(&report)));
        }

        // The detector measures RMS in dBFS; the programme is measured as gated
        // K-weighted loudness. For speech the two scales differ by little, and
        // what matters is that the threshold tracks the programme rather than
        // the absolute level, so the offset is carried through as it stands.
        let threshold_dbfs = programme + params.threshold_relative_db;
        ctx.progress(0.1);
        let result = dynamics::apply(
            signal.channels(),
            signal.sample_rate(),
            &Compressor {
                threshold_db: threshold_dbfs,
                ratio: params.ratio,
                knee_db: params.knee_db,
                max_reduction_db: params.max_reduction_db,
            },
            &Timing {
                down_ms: params.attack_ms,
                up_ms: params.release_ms,
                detector_ms: Timing::default().detector_ms,
            },
        );
        ctx.progress(0.9);

        let output = Signal::new(signal.sample_rate(), result.channels);
        ctx.progress(1.0);

        report.threshold_dbfs = Some(threshold_dbfs);
        // Measured, not assumed: a compressor that claims a ratio and delivers
        // nothing is a bug only this number makes visible.
        report.loudness_range_after_lu =
            Weighted::new(output.channels(), output.sample_rate()).range();
        report.max_reduction_db = result.max_reduction_db;
        report.mean_gain_db = result.mean_gain_db;
        report.active_fraction = result.active_fraction;
        report.skipped = false;

        Ok(StageOutput::new(output, report_of(&report)))
    }
}

// ------------------------------------------------------------------- level --

/// The leveller.
///
/// Last, so the loudness target is measured on the audio that actually gets
/// written and the true-peak limiter sees what the compressor produced.
///
/// It declares no required sample rate: the K-weighting coefficients are
/// derived analytically per rate, so it is correct at 44.1 kHz just as it is at
/// 48 and needs no conversion.
pub struct Level;

impl Stage for Level {
    fn name(&self) -> &'static str {
        "level"
    }
    fn description(&self) -> &'static str {
        "Normalise each speech segment to a target loudness, ramping across silences"
    }
    fn default_params(&self) -> Value {
        report_of(&LevellerOptions::default())
    }

    fn render(
        &self,
        signal: Arc<Signal>,
        params: &Value,
        ctx: &mut StageContext,
    ) -> Result<StageOutput, StageError> {
        let params: LevellerOptions = params_of(self.name(), params)?;
        let result = leveller::level(&signal, &params);
        ctx.progress(1.0);

        let report = json!({
            "sampleRate": result.report.sample_rate,
            "integratedLufs": result.report.integrated_lufs,
            "thresholdLufs": result.report.threshold_lufs,
            "floorLufs": result.report.floor_lufs,
            "segments": result.report.segments,
            "silences": result.report.silences,
            "limiterGainReductionDb": result.report.limiter_gain_reduction_db,
            "roomTone": {
                "clips": result.report.room_tone.clips,
                "instances": result.report.room_tone.instances,
                "gainDb": result.report.room_tone.gain_db,
                "length": result.report.room_tone.length,
                "durationSec": result.report.room_tone.duration_sec,
            },
        });

        let mut output = StageOutput::new(result.signal, report);
        // Only offer a bed when there was usable silence to build one from.
        if !result.room_tone.is_empty() {
            output = output.with_extra("roomtone", result.room_tone);
        }
        Ok(output)
    }
}
