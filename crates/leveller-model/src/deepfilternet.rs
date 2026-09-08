//! DeepFilterNet3: everything around the three neural graphs.
//!
//! The ONNX export is not a denoiser. It is three graphs — encoder, ERB
//! decoder, deep-filter decoder — that consume normalised spectral features and
//! emit an ERB gain mask plus a set of complex filter taps. The transform, the
//! ERB filterbank, the running feature normalisation, the mask application, the
//! deep filter and the resynthesis all live outside the model, and every one of
//! them has to match what the model was trained on or the output is subtly
//! wrong rather than obviously broken. This module is that half, ported from
//! upstream `libDF` (`libDF/src/lib.rs` and `libDF/src/tract.rs`), and it is
//! deliberately free of any ONNX Runtime dependency so it can be tested without
//! weights present.
//!
//! Two things about the export are worth knowing before reading further,
//! because both contradict the obvious guess:
//!
//! - **There is no recurrent state on the graph boundary.** The GRUs are inside
//!   the graph and run over a whole time axis in one call. So this is not a
//!   frame-at-a-time streaming runner threading hidden state between chunks; it
//!   feeds long spans of frames and lets the graph unroll them. State is only
//!   restarted where the chunking splits a long file, and a discarded warm-up
//!   prefix covers the seam.
//!
//! - **The network's lookahead is applied by the caller, not by the graph.**
//!   The PyTorch model shifts its own input by `conv_lookahead` frames; the ONNX
//!   export does not include that shift, so upstream's runtime instead applies
//!   the output of model frame `t` to spectrum frame `t − lookahead`. Get this
//!   wrong and the mask still "works" — it is simply 40 ms early, which smears
//!   onsets and sounds like a bad denoiser rather than like a bug.

use leveller_dsp::FftPlan;
use rustfft::num_complex::Complex64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub sample_rate: u32,
    /// Transform size. 960 = 20 ms at 48 kHz, and not a power of two.
    pub fft_size: usize,
    pub hop_size: usize,
    /// Number of ERB bands the gain mask covers (the whole spectrum).
    pub nb_erb: usize,
    /// Number of low bins the deep filter covers (0 – 4.8 kHz at 48 kHz).
    pub nb_df: usize,
    /// Floor on how few FFT bins an ERB band may hold.
    pub min_nb_erb_freqs: usize,
    /// Number of filter taps per bin.
    pub df_order: usize,
    /// Frames of lookahead between a model output and the frame it describes.
    pub lookahead: usize,
    /// Time constant of the running feature normalisation, in seconds.
    pub norm_tau: f64,
    /// Below this local SNR the frame is judged noise-only and zeroed.
    pub min_db_thresh: f64,
    /// Above this local SNR the frame is judged clean and left alone entirely.
    pub max_db_erb_thresh: f64,
    /// Above this local SNR the gain mask runs but the deep filter does not.
    pub max_db_df_thresh: f64,
}

/// Values read from the `[df]` and `[deepfilternet]` sections of the shipped
/// `config.ini`.
pub const DFN3: Config = Config {
    sample_rate: 48_000,
    fft_size: 960,
    hop_size: 480,
    nb_erb: 32,
    nb_df: 96,
    min_nb_erb_freqs: 2,
    df_order: 5,
    // config.ini has conv_lookahead = 2 and df_lookahead = 2; upstream uses the
    // larger of the two, and the export requires them to be equal in practice.
    lookahead: 2,
    norm_tau: 1.0,
    // libDF `RuntimeParams::default()`.
    min_db_thresh: -10.0,
    max_db_erb_thresh: 30.0,
    max_db_df_thresh: 20.0,
};

impl Config {
    pub fn bins(&self) -> usize {
        self.fft_size / 2 + 1
    }
}

/// Initial state of the running ERB mean, in dB: a ramp from −60 to −90.
const MEAN_NORM_INIT: (f64, f64) = (-60.0, -90.0);
/// Initial state of the running complex magnitude, per bin.
const UNIT_NORM_INIT: (f64, f64) = (0.001, 0.0001);

fn freq_to_erb(hz: f64) -> f64 {
    9.265 * (hz / (24.7 * 9.265)).ln_1p()
}

fn erb_to_freq(erb: f64) -> f64 {
    24.7 * 9.265 * ((erb / 9.265).exp() - 1.0)
}

/// Width, in FFT bins, of each ERB band.
///
/// A direct port of libDF's `erb_fb`, including its quirks: the `freq_over`
/// carry that repays bins borrowed by the `min_nb_freqs` floor, and the final
/// band absorbing the leftover so the widths sum to exactly `fft_size/2 + 1`.
/// Reimplementing this "cleanly" would shift every band edge and invalidate the
/// trained weights.
pub fn erb_widths(config: &Config) -> Vec<usize> {
    let freq_width = f64::from(config.sample_rate) / config.fft_size as f64;
    let erb_low = freq_to_erb(0.0);
    let erb_high = freq_to_erb(f64::from(config.sample_rate) / 2.0);
    let step = (erb_high - erb_low) / config.nb_erb as f64;

    let mut widths = vec![0usize; config.nb_erb];
    let mut prev_freq = 0i64;
    let mut freq_over = 0i64;
    for i in 1..=config.nb_erb {
        let fb = (erb_to_freq(erb_low + i as f64 * step) / freq_width).round() as i64;
        let mut nb_freqs = fb - prev_freq - freq_over;
        if nb_freqs < config.min_nb_erb_freqs as i64 {
            freq_over = config.min_nb_erb_freqs as i64 - nb_freqs;
            nb_freqs = config.min_nb_erb_freqs as i64;
        } else {
            freq_over = 0;
        }
        widths[i - 1] = nb_freqs.max(0) as usize;
        prev_freq = fb;
    }

    let last = config.nb_erb - 1;
    widths[last] += 1;
    let total: usize = widths.iter().sum();
    if total > config.bins() {
        widths[last] -= total - config.bins();
    }
    widths
}

/// The Vorbis power-complementary window, `sin(π/2·sin²(π(n+½)/N))`.
///
/// Not the sqrt-Hann the rest of the pipeline uses. At 50% overlap this one
/// satisfies the Princen–Bradley condition, so applying it on both analysis and
/// synthesis sums to unity — and it is what the model was trained through.
pub fn vorbis_window(fft_size: usize) -> Vec<f64> {
    let half = (fft_size / 2) as f64;
    (0..fft_size)
        .map(|i| {
            let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / half).sin();
            (0.5 * std::f64::consts::PI * s * s).sin()
        })
        .collect()
}

/// Number of frames needed to cover `length` samples with overlap-add both
/// ends.
pub fn frame_count(length: usize, hop_size: usize) -> usize {
    length.div_ceil(hop_size) + 1
}

/// One frame per entry, each `bins` complex values long.
pub type Spectrogram = Vec<Vec<Complex64>>;

/// Analyse a channel into DeepFilterNet's spectrogram.
///
/// Frame `t` windows the samples starting at `(t − 1)·hop`, matching the
/// streaming implementation, where the first half of each window is the
/// previous input frame held in `analysis_mem`. The `1/fft_size` scaling is
/// part of the contract, not a convenience: the feature normalisation states
/// are initialised in absolute units, so a spectrum scaled differently walks
/// into the model with the wrong dynamic range.
pub fn analyse(samples: &[f32], frames: usize, config: &Config, plan: &mut FftPlan) -> Spectrogram {
    let window = vorbis_window(config.fft_size);
    let bins = config.bins();
    let scale = 1.0 / config.fft_size as f64;
    let mut buffer = vec![Complex64::default(); config.fft_size];

    (0..frames)
        .map(|t| {
            let start = t as isize - 1;
            let start = start * config.hop_size as isize;
            for (i, slot) in buffer.iter_mut().enumerate() {
                let at = start + i as isize;
                let sample = if at >= 0 && (at as usize) < samples.len() {
                    f64::from(samples[at as usize]) * window[i]
                } else {
                    0.0
                };
                *slot = Complex64::new(sample, 0.0);
            }
            plan.forward(&mut buffer);
            buffer[..bins].iter().map(|c| c * scale).collect()
        })
        .collect()
}

/// Rebuild a channel from (possibly modified) spectra.
///
/// The inverse transform here is normalised by `1/fft_size`, where libDF's is
/// not; the `fft_size` factor below cancels that difference so the round trip
/// still reconstructs exactly. No overlap normalisation is accumulated because
/// the Vorbis window squared already sums to one at this hop.
pub fn synthesise(
    spectra: &[Vec<Complex64>],
    length: usize,
    config: &Config,
    plan: &mut FftPlan,
) -> Vec<f32> {
    let window = vorbis_window(config.fft_size);
    let bins = config.bins();
    let mut buffer = vec![Complex64::default(); config.fft_size];
    let mut out = vec![0.0f32; length];

    for (t, spectrum) in spectra.iter().enumerate() {
        buffer[..bins].copy_from_slice(&spectrum[..bins]);
        // A real signal's spectrum is conjugate-symmetric; rebuild the upper
        // half.
        for k in bins..config.fft_size {
            buffer[k] = spectrum[config.fft_size - k].conj();
        }
        plan.inverse(&mut buffer);

        let start = (t as isize - 1) * config.hop_size as isize;
        for (i, value) in buffer.iter().enumerate() {
            let at = start + i as isize;
            if at < 0 {
                continue;
            }
            let at = at as usize;
            if at >= length {
                break;
            }
            out[at] += (value.re * config.fft_size as f64 * window[i]) as f32;
        }
    }

    out
}

/// The exponential-averaging coefficient, ported including its rounding.
///
/// Upstream rounds `exp(−hop/sr / tau)` to three decimals (extending the
/// precision only if that would round up to 1.0). At 48 kHz this makes alpha
/// exactly 0.99 rather than 0.99005, and the whole feature sequence is
/// conditioned on it, so the rounding is copied rather than corrected.
pub fn norm_alpha(config: &Config) -> f64 {
    let dt = config.hop_size as f64 / f64::from(config.sample_rate);
    let alpha = (-dt / config.norm_tau).exp();
    let mut precision = 3i32;
    let mut rounded = 1.0;
    while rounded >= 1.0 {
        let scale = 10f64.powi(precision);
        rounded = (alpha * scale).round() / scale;
        precision += 1;
    }
    rounded
}

fn linspace(from: f64, to: f64, count: usize) -> Vec<f64> {
    let step = (to - from) / (count - 1) as f64;
    (0..count).map(|i| from + i as f64 * step).collect()
}

pub struct Features {
    /// ERB band energies, `[frames][nb_erb]`, flattened.
    pub erb: Vec<f32>,
    /// Unit-normalised low bins as [real block, imaginary block], flattened.
    pub spec: Vec<f32>,
    pub frames: usize,
}

/// Compute both model inputs for a whole spectrogram.
///
/// The two normalisations are running averages over time, so this must see the
/// frames in order and from the start of the file. That is also why features
/// are computed for the whole signal even when inference is chunked: chunking
/// the features too would restart these averages mid-file and put a step in the
/// input the model has no reason to expect.
pub fn compute_features(
    spectrogram: &Spectrogram,
    config: &Config,
    widths: &[usize],
) -> Features {
    let alpha = norm_alpha(config);
    let frames = spectrogram.len();

    let mut mean_state = linspace(MEAN_NORM_INIT.0, MEAN_NORM_INIT.1, config.nb_erb);
    let mut unit_state = linspace(UNIT_NORM_INIT.0, UNIT_NORM_INIT.1, config.nb_df);

    let mut erb = vec![0.0f32; frames * config.nb_erb];
    // Laid out as the model wants it: [1, 2, frames, nb_df], real block first.
    let mut spec = vec![0.0f32; 2 * frames * config.nb_df];

    for (t, spectrum) in spectrogram.iter().enumerate() {
        // ERB band power, then dB, then subtract the running per-band mean.
        let mut bin = 0usize;
        for (b, width) in widths.iter().take(config.nb_erb).enumerate() {
            let sum: f64 = spectrum[bin..bin + width].iter().map(|c| c.norm_sqr()).sum();
            bin += width;

            let db = 10.0 * (sum / *width as f64 + 1e-10).log10();
            mean_state[b] = db * (1.0 - alpha) + mean_state[b] * alpha;
            erb[t * config.nb_erb + b] = ((db - mean_state[b]) / 40.0) as f32;
        }

        // Complex low bins divided by the square root of their running
        // magnitude.
        for k in 0..config.nb_df {
            let value = spectrum[k];
            unit_state[k] = value.norm() * (1.0 - alpha) + unit_state[k] * alpha;
            let norm = unit_state[k].sqrt();
            spec[t * config.nb_df + k] = (value.re / norm) as f32;
            spec[frames * config.nb_df + t * config.nb_df + k] = (value.im / norm) as f32;
        }
    }

    Features { erb, spec, frames }
}

/// What the three graphs produce for one span of frames.
pub struct ModelOutput {
    /// ERB gains, `[frames][nb_erb]`.
    pub gains: Vec<f32>,
    /// Deep filter taps, `[frames][nb_df][df_order]` as interleaved [re, im].
    pub coefs: Vec<f32>,
    /// Local SNR estimate per frame, in dB.
    pub lsnr: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
pub struct ChunkOptions {
    /// Frames per inference call.
    ///
    /// The graphs unroll their recurrence over whatever time axis they are
    /// handed, so a whole file in one call would be both correct and, on a long
    /// recording, ruinous — the encoder's intermediate tensors alone run to
    /// hundreds of megabytes.
    pub chunk_frames: usize,
    /// Frames prepended to each chunk, whose outputs are not kept.
    ///
    /// Splitting restarts the GRU state at each seam, and this model's state
    /// takes a long time to settle — measured against a whole-file run, a frame
    /// needs several hundred frames of history before its local-SNR estimate is
    /// within half a dB, and it is still creeping at 800. So the warm-up is
    /// generous; it costs only compute, which is not the binding constraint.
    pub warmup_frames: usize,
}

/// Span is chunk + warm-up, and it sets peak memory: ONNX Runtime holds the
/// whole span's activations, measured at roughly 210 MB per 1000 frames. 2000
/// frames is about 420 MB at the peak of one inference call, which is the most
/// worth spending inside a desktop app for a stage that has a working classical
/// alternative.
///
/// What this split costs is small and was measured rather than assumed. Against
/// a whole-file run the chunked render differs by about −61 dB, but that
/// difference is not damage — neither estimate is ground truth — so the number
/// that decided these values is SI-SDR against clean audio, where a whole-file
/// run scores 25.58 dB on a noisy fixture and 58.03 on a clean one, and this
/// split scores 25.62 and 57.88. Crossfading the seam was tried and removed: it
/// moved neither figure, because the residual difference is spread through the
/// chunk rather than concentrated at the join.
impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            chunk_frames: 1000, // 10 s at 48 kHz
            warmup_frames: 1000,
        }
    }
}

/// Run a feature sequence through `run_span` in chunks and stitch the result.
///
/// Only the inference is chunked. The features were computed once over the
/// whole file so their running normalisation never restarts; the only
/// discontinuity here is the network's own hidden state, which the warm-up
/// covers. Taking a callback keeps the index arithmetic — the part that can be
/// wrong in a way no ear would localise — testable without weights present.
pub fn run_chunked<E>(
    features: &Features,
    config: &Config,
    options: &ChunkOptions,
    mut run_span: impl FnMut(&[f32], &[f32], usize) -> Result<ModelOutput, E>,
    mut on_progress: impl FnMut(f64),
) -> Result<ModelOutput, E> {
    let frames = features.frames;
    let (nb_erb, nb_df) = (config.nb_erb, config.nb_df);
    let coefs_per_frame = nb_df * config.df_order * 2;

    if frames <= options.chunk_frames {
        on_progress(1.0);
        return run_span(&features.erb, &features.spec, frames);
    }

    let mut gains = vec![0.0f32; frames * nb_erb];
    let mut coefs = vec![0.0f32; frames * coefs_per_frame];
    let mut lsnr = vec![0.0f32; frames];

    let mut start = 0usize;
    while start < frames {
        let end = frames.min(start + options.chunk_frames);
        let from = start.saturating_sub(options.warmup_frames);
        let span = end - from;

        // The complex feature is two contiguous blocks over the whole file, so
        // a slice of it has to be assembled from both.
        let mut span_spec = vec![0.0f32; 2 * span * nb_df];
        span_spec[..span * nb_df].copy_from_slice(&features.spec[from * nb_df..end * nb_df]);
        span_spec[span * nb_df..].copy_from_slice(
            &features.spec[(frames + from) * nb_df..(frames + end) * nb_df],
        );

        let out = run_span(
            &features.erb[from * nb_erb..end * nb_erb],
            &span_spec,
            span,
        )?;

        let skip = start - from; // warm-up frames, discarded
        gains[start * nb_erb..end * nb_erb]
            .copy_from_slice(&out.gains[skip * nb_erb..span * nb_erb]);
        coefs[start * coefs_per_frame..end * coefs_per_frame]
            .copy_from_slice(&out.coefs[skip * coefs_per_frame..span * coefs_per_frame]);
        lsnr[start..end].copy_from_slice(&out.lsnr[skip..span]);
        on_progress(end as f64 / frames as f64);

        start += options.chunk_frames;
    }

    Ok(ModelOutput {
        gains,
        coefs,
        lsnr,
    })
}

/// Which of the two stages to run for a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stages {
    pub gains: bool,
    pub zeros: bool,
    pub deep_filter: bool,
}

/// Which of the two stages to run for a frame, from its local SNR.
///
/// Ported from libDF's `apply_stages`. The top branch matters most for
/// transparency: above `max_db_erb_thresh` the model declines to touch the
/// frame at all, which is the same instinct the rest of this pipeline is built
/// on. The bottom branch is a mute, which sounds worse than it reads — but the
/// attenuation-limit mix in [`apply_model`] turns it into an attenuation of
/// exactly the requested reduction rather than digital silence.
pub fn stages_for(lsnr: f64, config: &Config) -> Stages {
    if lsnr < config.min_db_thresh {
        return Stages {
            gains: false,
            zeros: true,
            deep_filter: false,
        };
    }
    if lsnr > config.max_db_erb_thresh {
        return Stages::default();
    }
    if lsnr > config.max_db_df_thresh {
        return Stages {
            gains: true,
            zeros: false,
            deep_filter: false,
        };
    }
    Stages {
        gains: true,
        zeros: false,
        deep_filter: true,
    }
}

/// What [`apply_model`] did, for the stage's report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub frames_gained: usize,
    pub frames_deep_filtered: usize,
    pub frames_left_alone: usize,
}

/// Apply the model's output to the noisy spectrogram.
///
/// `output[t]` is built from model frame `t + lookahead`, per the module note:
/// the graph emits its estimate for frame `t` two frames late. The deep filter
/// reads the *noisy* spectra, not the mask's output — it is a separate estimate
/// of the same frame from a span of neighbours, and it replaces rather than
/// refines the low bins.
///
/// ## The ERB mask is only valid above `nb_df`
///
/// This is the one place that deliberately departs from libDF's runtime, and it
/// is worth the paragraph. The training graph keeps the masked spectrum only
/// for the bins the deep filter does not cover — `spec_e[..., nb_df:, :] =
/// spec_m[..., nb_df:, :]` — so the network was never given a reason to emit
/// sensible ERB gains below that boundary, and it does not: on clean speech the
/// bands below bin 96 come back at about 0.25, a flat −12 dB, while the bands
/// above it correctly sit near unity.
///
/// libDF gets away with masking the whole spectrum because the deep filter then
/// overwrites the low bins. But its `apply_stages` has a branch — local SNR
/// between `max_db_df_thresh` and `max_db_erb_thresh` — where the mask runs and
/// the deep filter does not, and there the junk gains survive into the output.
/// Measured on the fixture used to develop this: applying the mask across the
/// full spectrum scores 3.8 dB SI-SDR where confining it to the bins it was
/// trained for scores 25.4, and on *clean* input the difference is 6.6 dB
/// against 51.6 — the mask below `nb_df` does not merely fail to help, it
/// destroys speech that needed nothing. So the mask is applied only from
/// `nb_df` up, and the low bins are either deep-filtered or left alone.
///
/// `attenuation_limit_db` is upstream's `atten_lim_db` and is how a target
/// reduction is honoured: mixing a fixed fraction of the noisy spectrum back in
/// bounds the attenuation of any bin to that many dB, while leaving bins the
/// model already passes through essentially untouched. A plain wet/dry blend
/// would instead dilute the speech by the same fraction it dilutes the noise.
pub fn apply_model(
    noisy: &Spectrogram,
    model: &ModelOutput,
    config: &Config,
    widths: &[usize],
    attenuation_limit_db: f64,
) -> (Spectrogram, Applied) {
    let bins = noisy.first().map_or(0, Vec::len);
    let limit = if attenuation_limit_db > 0.0 {
        10f64.powf(-attenuation_limit_db / 20.0)
    } else {
        0.0
    };

    let mut out = Vec::with_capacity(noisy.len());
    let mut applied = Applied::default();

    for (t, source) in noisy.iter().enumerate() {
        let mut enhanced = vec![Complex64::default(); bins];
        let m = t + config.lookahead;
        let stages = if m < model.lsnr.len() {
            stages_for(f64::from(model.lsnr[m]), config)
        } else {
            Stages::default()
        };

        if stages.gains {
            // The mask above nb_df, the source below it — the deep filter
            // overwrites the low bins next when it runs, and when it does not,
            // passing them through is the only honest thing left to do with
            // them. The test is per bin rather than per band because the band
            // holding bin nb_df straddles the boundary.
            enhanced.copy_from_slice(source);
            let mut bin = 0usize;
            for (b, width) in widths.iter().take(config.nb_erb).enumerate() {
                let gain = f64::from(model.gains[m * config.nb_erb + b]);
                for k in bin..bin + width {
                    if k >= config.nb_df && k < bins {
                        enhanced[k] = source[k] * gain;
                    }
                }
                bin += width;
            }
            applied.frames_gained += 1;
        } else if stages.zeros {
            // enhanced is already zero
        } else {
            enhanced.copy_from_slice(source);
            applied.frames_left_alone += 1;
        }

        if stages.deep_filter {
            // out[t][f] = Σ_o noisy[t − lookahead + o][f] · coefs[t + lookahead][f][o]
            for (k, enhanced) in enhanced.iter_mut().take(config.nb_df.min(bins)).enumerate() {
                let mut sum = Complex64::default();
                for o in 0..config.df_order {
                    let at = t as isize - config.lookahead as isize + o as isize;
                    if at < 0 || at as usize >= noisy.len() {
                        continue;
                    }
                    let c = 2 * ((m * config.nb_df + k) * config.df_order + o);
                    let coef = Complex64::new(f64::from(model.coefs[c]), f64::from(model.coefs[c + 1]));
                    sum += noisy[at as usize][k] * coef;
                }
                *enhanced = sum;
            }
            applied.frames_deep_filtered += 1;
        }

        if limit > 0.0 {
            for (enhanced, source) in enhanced.iter_mut().zip(source) {
                *enhanced = *enhanced * (1.0 - limit) + source * limit;
            }
        }

        out.push(enhanced);
    }

    (out, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        DFN3
    }

    #[test]
    fn erb_bands_cover_exactly_the_spectrum() {
        // The widths have to sum to the bin count or the filterbank silently
        // reads past the end of a frame, and every band edge after the mistake
        // is wrong.
        let config = config();
        let widths = erb_widths(&config);
        assert_eq!(widths.len(), config.nb_erb);
        assert_eq!(widths.iter().sum::<usize>(), config.bins());
        assert!(
            widths.iter().all(|w| *w >= config.min_nb_erb_freqs),
            "no band may fall under the floor: {widths:?}"
        );
        // Low bands are narrow and high ones wide — that is what makes it ERB.
        assert!(widths[0] < widths[config.nb_erb - 1]);
    }

    #[test]
    fn the_window_sums_to_unity_at_fifty_percent_overlap() {
        // Princen–Bradley: applied on both analysis and synthesis, the squared
        // window must sum to one across the hop, or the output is amplitude
        // modulated at the frame rate.
        let config = config();
        let window = vorbis_window(config.fft_size);
        for i in 0..config.hop_size {
            let sum = window[i] * window[i]
                + window[i + config.hop_size] * window[i + config.hop_size];
            assert!((sum - 1.0).abs() < 1e-12, "at {i}: {sum}");
        }
    }

    #[test]
    fn the_normalisation_coefficient_keeps_upstreams_rounding() {
        // Exactly 0.99, not 0.99005. The whole feature sequence is conditioned
        // on it, so copying the rounding is the point.
        assert_eq!(norm_alpha(&config()), 0.99);
    }

    #[test]
    fn the_rounding_extends_rather_than_returning_one() {
        // A long time constant rounds to 1.0 at three decimals, which would
        // freeze the running average at its initial state forever.
        let config = Config {
            norm_tau: 1000.0,
            ..config()
        };
        let alpha = norm_alpha(&config);
        assert!(alpha < 1.0, "alpha must stay under one, got {alpha}");
        assert!(alpha > 0.9999);
    }

    #[test]
    fn analysis_and_synthesis_reconstruct_the_signal() {
        // The transform pair is the floor everything else stands on: if the
        // round trip is not exact, every measurement of what the model did is
        // measuring the transform instead.
        let config = config();
        let length = 4800;
        let samples: Vec<f32> = (0..length)
            .map(|i| {
                let t = i as f64 / f64::from(config.sample_rate);
                (0.3 * (std::f64::consts::TAU * 220.0 * t).sin()
                    + 0.2 * (std::f64::consts::TAU * 1310.0 * t).sin()) as f32
            })
            .collect();

        let mut plan = FftPlan::new(config.fft_size);
        let frames = frame_count(length, config.hop_size);
        let spectrogram = analyse(&samples, frames, &config, &mut plan);
        let rebuilt = synthesise(&spectrogram, length, &config, &mut plan);

        // The first and last hop cannot be reconstructed: only one of the two
        // overlapping windows covers them.
        let from = config.hop_size;
        let to = length - config.hop_size;
        let worst = samples[from..to]
            .iter()
            .zip(&rebuilt[from..to])
            .map(|(a, b)| f64::from((a - b).abs()))
            .fold(0.0, f64::max);
        assert!(worst < 1e-6, "worst sample error {worst}");
    }

    #[test]
    fn features_come_out_in_the_shapes_the_graphs_expect() {
        let config = config();
        let widths = erb_widths(&config);
        let samples = vec![0.01f32; 4800];
        let mut plan = FftPlan::new(config.fft_size);
        let frames = frame_count(samples.len(), config.hop_size);
        let spectrogram = analyse(&samples, frames, &config, &mut plan);
        let features = compute_features(&spectrogram, &config, &widths);

        assert_eq!(features.frames, frames);
        assert_eq!(features.erb.len(), frames * config.nb_erb);
        assert_eq!(features.spec.len(), 2 * frames * config.nb_df);
        assert!(
            features.erb.iter().all(|v| v.is_finite()),
            "a NaN here poisons the whole graph silently"
        );
        assert!(features.spec.iter().all(|v| v.is_finite()));
    }

    /// A stand-in for the graphs: deterministic outputs derived from the
    /// inputs, so stitching can be checked without weights.
    fn fake_span(config: &Config) -> impl FnMut(&[f32], &[f32], usize) -> Result<ModelOutput, ()> {
        let (nb_erb, nb_df, df_order) = (config.nb_erb, config.nb_df, config.df_order);
        move |erb: &[f32], _spec: &[f32], frames: usize| {
            // Each frame's outputs are a function of its own first ERB value,
            // so a frame that came out of the wrong place in the stitch is
            // detectable.
            let gains: Vec<f32> = (0..frames * nb_erb)
                .map(|i| erb[(i / nb_erb) * nb_erb])
                .collect();
            let coefs: Vec<f32> = (0..frames * nb_df * df_order * 2)
                .map(|i| erb[(i / (nb_df * df_order * 2)) * nb_erb])
                .collect();
            let lsnr: Vec<f32> = (0..frames).map(|t| erb[t * nb_erb]).collect();
            Ok(ModelOutput {
                gains,
                coefs,
                lsnr,
            })
        }
    }

    fn features_of(frames: usize, config: &Config) -> Features {
        Features {
            erb: (0..frames * config.nb_erb)
                .map(|i| (i / config.nb_erb) as f32)
                .collect(),
            spec: (0..2 * frames * config.nb_df).map(|i| i as f32).collect(),
            frames,
        }
    }

    #[test]
    fn chunking_stitches_back_to_what_one_pass_would_have_given() {
        // The index arithmetic here is the kind of thing that can be wrong in a
        // way no ear would localise — a frame from the wrong chunk sounds like
        // nothing in particular.
        let config = config();
        let frames = 2500;
        let features = features_of(frames, &config);

        let whole: ModelOutput = run_chunked(
            &features,
            &config,
            &ChunkOptions {
                chunk_frames: frames * 2,
                warmup_frames: 0,
            },
            fake_span(&config),
            |_| {},
        )
        .unwrap();

        let split: ModelOutput = run_chunked(
            &features,
            &config,
            &ChunkOptions {
                chunk_frames: 400,
                warmup_frames: 300,
            },
            fake_span(&config),
            |_| {},
        )
        .unwrap();

        assert_eq!(whole.lsnr, split.lsnr);
        assert_eq!(whole.gains, split.gains);
        assert_eq!(whole.coefs, split.coefs);
    }

    #[test]
    fn chunking_reports_progress_to_completion() {
        let config = config();
        let features = features_of(2500, &config);
        let mut seen = Vec::new();
        let _: ModelOutput = run_chunked(
            &features,
            &config,
            &ChunkOptions {
                chunk_frames: 400,
                warmup_frames: 300,
            },
            fake_span(&config),
            |f| seen.push(f),
        )
        .unwrap();

        assert!(seen.len() > 1);
        assert_eq!(seen.last().copied(), Some(1.0));
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "progress must not go backwards: {seen:?}"
        );
    }

    /// A spectrogram of ones, and a model output that asks for `gain`
    /// everywhere at the given local SNR.
    fn scene(config: &Config, gain: f32, lsnr: f32) -> (Spectrogram, ModelOutput) {
        let frames = 8;
        let noisy: Spectrogram = (0..frames)
            .map(|_| vec![Complex64::new(1.0, 0.0); config.bins()])
            .collect();
        let model = ModelOutput {
            gains: vec![gain; frames * config.nb_erb],
            coefs: vec![0.0; frames * config.nb_df * config.df_order * 2],
            lsnr: vec![lsnr; frames],
        };
        (noisy, model)
    }

    #[test]
    fn the_erb_mask_leaves_the_bins_below_nb_df_alone() {
        // The model was never trained to emit sensible gains down there, and
        // applying them anyway destroys speech that needed nothing.
        let config = config();
        let widths = erb_widths(&config);
        // An SNR between the two thresholds: mask on, deep filter off, which is
        // the branch where junk gains would survive into the output.
        let (noisy, model) = scene(&config, 0.25, 25.0);
        let (out, applied) = apply_model(&noisy, &model, &config, &widths, 0.0);

        // The last `lookahead` frames have no model output to apply — the graph
        // describes frame t at index t + lookahead, and the sequence ends
        // first — so they pass through rather than being masked.
        assert_eq!(applied.frames_gained, noisy.len() - config.lookahead);
        assert_eq!(applied.frames_left_alone, config.lookahead);
        assert_eq!(applied.frames_deep_filtered, 0);
        for (k, value) in out[0].iter().take(config.nb_df).enumerate() {
            assert_eq!(value.re, 1.0, "bin {k} should be untouched");
        }
        for (k, value) in out[0].iter().enumerate().skip(config.nb_df) {
            assert!(
                (value.re - 0.25).abs() < 1e-12,
                "bin {k} should carry the mask"
            );
        }
    }

    #[test]
    fn a_frame_the_model_calls_clean_passes_through_untouched() {
        let config = config();
        let widths = erb_widths(&config);
        let (noisy, model) = scene(&config, 0.25, 40.0);
        let (out, applied) = apply_model(&noisy, &model, &config, &widths, 0.0);

        assert_eq!(applied.frames_left_alone, noisy.len());
        assert_eq!(applied.frames_gained, 0);
        assert!(out[0].iter().all(|c| c.re == 1.0));
    }

    #[test]
    fn no_bin_is_attenuated_by_more_than_the_requested_reduction() {
        // This is how a target reduction is honoured, and the mute branch is
        // the case that matters: without the mix it is digital silence.
        let config = config();
        let widths = erb_widths(&config);
        let (noisy, model) = scene(&config, 0.0, -20.0);
        let (out, applied) = apply_model(&noisy, &model, &config, &widths, 12.0);

        assert_eq!(applied.frames_gained, 0, "a muted frame is not a gained one");
        let expected = 10f64.powf(-12.0 / 20.0);
        for (t, frame) in out.iter().take(noisy.len() - config.lookahead).enumerate() {
            for (k, value) in frame.iter().enumerate() {
                assert!(
                    (value.re - expected).abs() < 1e-12,
                    "frame {t} bin {k}: {} should be {expected}",
                    value.re
                );
            }
        }
        // And the tail, which has no model output, is passed through whole
        // rather than muted — the mix runs on it too, but source and enhanced
        // are the same signal there, so it is a no-op.
        for frame in out.iter().skip(noisy.len() - config.lookahead) {
            assert!(frame.iter().all(|c| (c.re - 1.0).abs() < 1e-12));
        }
    }

    #[test]
    fn the_stage_gate_matches_upstream() {
        let config = config();
        assert_eq!(
            stages_for(-20.0, &config),
            Stages {
                gains: false,
                zeros: true,
                deep_filter: false
            },
            "below minDbThresh the frame is noise"
        );
        assert_eq!(
            stages_for(0.0, &config),
            Stages {
                gains: true,
                zeros: false,
                deep_filter: true
            },
            "in the working range both stages run"
        );
        assert_eq!(
            stages_for(25.0, &config),
            Stages {
                gains: true,
                zeros: false,
                deep_filter: false
            },
            "above maxDbDfThresh the deep filter stops"
        );
        assert_eq!(
            stages_for(40.0, &config),
            Stages::default(),
            "above maxDbErbThresh the model declines to act"
        );
    }

    #[test]
    fn the_deep_filter_reads_the_noisy_spectra_at_the_right_offset() {
        // The lookahead is applied by the caller, not the graph. Getting it
        // wrong leaves the mask 40 ms early, which smears onsets and sounds
        // like a bad denoiser rather than like a bug — so it is asserted
        // directly rather than left to the ear.
        let config = Config {
            df_order: 2,
            lookahead: 2,
            ..config()
        };
        let widths = erb_widths(&config);
        let frames = 6;

        // Frame t carries the value t in every bin, so where a tap read from is
        // recoverable from what came out.
        let noisy: Spectrogram = (0..frames)
            .map(|t| vec![Complex64::new(t as f64, 0.0); config.bins()])
            .collect();
        // One tap of 1 at o = 0, which should read frame t − lookahead.
        let mut coefs = vec![0.0f32; frames * config.nb_df * config.df_order * 2];
        for t in 0..frames {
            for k in 0..config.nb_df {
                coefs[2 * ((t * config.nb_df + k) * config.df_order)] = 1.0;
            }
        }
        let model = ModelOutput {
            gains: vec![1.0; frames * config.nb_erb],
            coefs,
            lsnr: vec![0.0; frames], // in range: both stages run
        };

        let (out, _) = apply_model(&noisy, &model, &config, &widths, 0.0);
        // Only the frames that have a model output — the last `lookahead` of
        // them do not, and pass through.
        for (t, frame) in out
            .iter()
            .enumerate()
            .take(frames - config.lookahead)
            .skip(config.lookahead)
        {
            assert_eq!(
                frame[0].re,
                (t - config.lookahead) as f64,
                "frame {t} read the wrong source frame"
            );
        }
        for (t, frame) in out.iter().enumerate().skip(frames - config.lookahead) {
            assert_eq!(frame[0].re, t as f64, "frame {t} should pass through");
        }
    }
}
