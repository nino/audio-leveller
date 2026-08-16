/**
 * DeepFilterNet3: everything around the three neural graphs.
 *
 * The ONNX export is not a denoiser. It is three graphs — encoder, ERB decoder,
 * deep-filter decoder — that consume normalised spectral features and emit an
 * ERB gain mask plus a set of complex filter taps. The transform, the ERB
 * filterbank, the running feature normalisation, the mask application, the
 * deep filter and the resynthesis all live outside the model, and every one of
 * them has to match what the model was trained on or the output is subtly
 * wrong rather than obviously broken. This module is that half, ported from
 * upstream `libDF` (`libDF/src/lib.rs` and `libDF/src/tract.rs`), and it is
 * deliberately free of any ONNX Runtime dependency so it can be tested without
 * weights present.
 *
 * Two things about the export are worth knowing before reading further, because
 * both contradict the obvious guess:
 *
 * - **There is no recurrent state on the graph boundary.** The GRUs are inside
 *   the graph and run over a whole time axis in one call. So this is not a
 *   frame-at-a-time streaming runner threading hidden state between chunks; it
 *   feeds long spans of frames and lets the graph unroll them. State is only
 *   restarted where {@link CHUNK_FRAMES} splits a long file, and a discarded
 *   warm-up prefix covers the seam.
 *
 * - **The network's lookahead is applied by the caller, not by the graph.** The
 *   PyTorch model shifts its own input by `conv_lookahead` frames; the ONNX
 *   export does not include that shift, so upstream's runtime instead applies
 *   the output of model frame `t` to spectrum frame `t − lookahead`. Get this
 *   wrong and the mask still "works" — it is simply 40 ms early, which smears
 *   onsets and sounds like a bad denoiser rather than like a bug.
 */

import { createFftPlan, type FftPlan } from "../dsp/fft";

export interface DeepFilterConfig {
  sampleRate: number;
  /** Transform size. 960 = 20 ms at 48 kHz, and not a power of two. */
  fftSize: number;
  hopSize: number;
  /** Number of ERB bands the gain mask covers (the whole spectrum). */
  nbErb: number;
  /** Number of low bins the deep filter covers (0 .. 4.8 kHz at 48 kHz). */
  nbDf: number;
  /** Floor on how few FFT bins an ERB band may hold. */
  minNbErbFreqs: number;
  /** Number of filter taps per bin. */
  dfOrder: number;
  /** Frames of lookahead between a model output and the frame it describes. */
  lookahead: number;
  /** Time constant of the running feature normalisation, in seconds. */
  normTau: number;
  /** Below this local SNR the frame is judged noise-only and zeroed. */
  minDbThresh: number;
  /** Above this local SNR the frame is judged clean and left alone entirely. */
  maxDbErbThresh: number;
  /** Above this local SNR the gain mask runs but the deep filter does not. */
  maxDbDfThresh: number;
}

/** Values read from the `[df]` and `[deepfilternet]` sections of the shipped `config.ini`. */
export const DFN3_CONFIG: DeepFilterConfig = {
  sampleRate: 48000,
  fftSize: 960,
  hopSize: 480,
  nbErb: 32,
  nbDf: 96,
  minNbErbFreqs: 2,
  dfOrder: 5,
  // config.ini has conv_lookahead = 2 and df_lookahead = 2; upstream uses the
  // larger of the two, and the export requires them to be equal in practice.
  lookahead: 2,
  normTau: 1,
  // libDF `RuntimeParams::default()`.
  minDbThresh: -10,
  maxDbErbThresh: 30,
  maxDbDfThresh: 20,
};

/** Initial state of the running ERB mean, in dB: a ramp from -60 to -90. */
const MEAN_NORM_INIT: [number, number] = [-60, -90];
/** Initial state of the running complex magnitude, per bin. */
const UNIT_NORM_INIT: [number, number] = [0.001, 0.0001];

const freqToErb = (hz: number): number => 9.265 * Math.log1p(hz / (24.7 * 9.265));
const erbToFreq = (erb: number): number => 24.7 * 9.265 * (Math.exp(erb / 9.265) - 1);

/**
 * Width, in FFT bins, of each ERB band.
 *
 * A direct port of libDF's `erb_fb`, including its quirks: the `freq_over`
 * carry that repays bins borrowed by the `min_nb_freqs` floor, and the final
 * band absorbing the leftover so the widths sum to exactly `fftSize/2 + 1`.
 * Reimplementing this "cleanly" would shift every band edge and invalidate the
 * trained weights.
 */
export function erbWidths(config: DeepFilterConfig): number[] {
  const { sampleRate, fftSize, nbErb, minNbErbFreqs } = config;
  const freqWidth = sampleRate / fftSize;
  const erbLow = freqToErb(0);
  const erbHigh = freqToErb(sampleRate / 2);
  const step = (erbHigh - erbLow) / nbErb;

  const widths: number[] = new Array(nbErb).fill(0);
  let prevFreq = 0;
  let freqOver = 0;
  for (let i = 1; i <= nbErb; i++) {
    const fb = Math.round(erbToFreq(erbLow + i * step) / freqWidth);
    let nbFreqs = fb - prevFreq - freqOver;
    if (nbFreqs < minNbErbFreqs) {
      freqOver = minNbErbFreqs - nbFreqs;
      nbFreqs = minNbErbFreqs;
    } else {
      freqOver = 0;
    }
    widths[i - 1] = nbFreqs;
    prevFreq = fb;
  }

  const bins = fftSize / 2 + 1;
  widths[nbErb - 1] += 1;
  const tooLarge = widths.reduce((a, b) => a + b, 0) - bins;
  if (tooLarge > 0) widths[nbErb - 1] -= tooLarge;
  return widths;
}

/**
 * The Vorbis power-complementary window, `sin(π/2·sin²(π(n+½)/N))`.
 *
 * Not the sqrt-Hann the rest of the pipeline uses. At 50 % overlap this one
 * satisfies the Princen–Bradley condition, so applying it on both analysis and
 * synthesis sums to unity — and it is what the model was trained through.
 */
export function vorbisWindow(fftSize: number): Float64Array {
  const half = fftSize / 2;
  const window = new Float64Array(fftSize);
  for (let i = 0; i < fftSize; i++) {
    const s = Math.sin((0.5 * Math.PI * (i + 0.5)) / half);
    window[i] = Math.sin(0.5 * Math.PI * s * s);
  }
  return window;
}

/** Number of frames needed to cover `length` samples with overlap-add both ends. */
export function frameCount(length: number, hopSize: number): number {
  return Math.ceil(length / hopSize) + 1;
}

export interface Spectrogram {
  /** One frame per entry, interleaved [re, im] over bins 0..fftSize/2. */
  frames: Float64Array[];
  bins: number;
}

/**
 * Analyse a channel into DeepFilterNet's spectrogram.
 *
 * Frame `t` windows the samples starting at `(t − 1)·hop`, matching the
 * streaming implementation, where the first half of each window is the
 * previous input frame held in `analysis_mem`. The `1/fftSize` scaling is part
 * of the contract, not a convenience: the feature normalisation states are
 * initialised in absolute units, so a spectrum scaled differently walks into
 * the model with the wrong dynamic range.
 */
export function analyse(
  samples: Float32Array,
  frames: number,
  config: DeepFilterConfig,
  plan: FftPlan = createFftPlan(config.fftSize),
): Spectrogram {
  const { fftSize, hopSize } = config;
  const window = vorbisWindow(fftSize);
  const bins = fftSize / 2 + 1;
  const scale = 1 / fftSize;
  const buffer = new Float64Array(2 * fftSize);
  const out: Float64Array[] = [];

  for (let t = 0; t < frames; t++) {
    const start = (t - 1) * hopSize;
    for (let i = 0; i < fftSize; i++) {
      const at = start + i;
      buffer[2 * i] = at >= 0 && at < samples.length ? samples[at] * window[i] : 0;
      buffer[2 * i + 1] = 0;
    }
    plan.forward(buffer);

    const spectrum = new Float64Array(2 * bins);
    for (let k = 0; k < bins; k++) {
      spectrum[2 * k] = buffer[2 * k] * scale;
      spectrum[2 * k + 1] = buffer[2 * k + 1] * scale;
    }
    out.push(spectrum);
  }

  return { frames: out, bins };
}

/**
 * Rebuild a channel from (possibly modified) spectra.
 *
 * The inverse transform here is normalised by `1/fftSize`, where libDF's is
 * not; the `fftSize` factor below cancels that difference so the round trip
 * still reconstructs exactly. No overlap normalisation is accumulated because
 * the Vorbis window squared already sums to one at this hop.
 */
export function synthesise(
  spectra: Float64Array[],
  length: number,
  config: DeepFilterConfig,
  plan: FftPlan = createFftPlan(config.fftSize),
): Float32Array {
  const { fftSize, hopSize } = config;
  const window = vorbisWindow(fftSize);
  const bins = fftSize / 2 + 1;
  const buffer = new Float64Array(2 * fftSize);
  const out = new Float32Array(length);

  spectra.forEach((spectrum, t) => {
    for (let k = 0; k < bins; k++) {
      buffer[2 * k] = spectrum[2 * k];
      buffer[2 * k + 1] = spectrum[2 * k + 1];
    }
    // A real signal's spectrum is conjugate-symmetric; rebuild the upper half.
    for (let k = bins; k < fftSize; k++) {
      const mirror = fftSize - k;
      buffer[2 * k] = spectrum[2 * mirror];
      buffer[2 * k + 1] = -spectrum[2 * mirror + 1];
    }
    plan.inverse(buffer);

    const start = (t - 1) * hopSize;
    for (let i = 0; i < fftSize; i++) {
      const at = start + i;
      if (at < 0) continue;
      if (at >= length) break;
      out[at] += buffer[2 * i] * fftSize * window[i];
    }
  });

  return out;
}

/**
 * The exponential-averaging coefficient, ported including its rounding.
 *
 * Upstream rounds `exp(-hop/sr / tau)` to three decimals (extending the
 * precision only if that would round up to 1.0). At 48 kHz this makes alpha
 * exactly 0.99 rather than 0.99005, and the whole feature sequence is
 * conditioned on it, so the rounding is copied rather than corrected.
 */
export function normAlpha(config: DeepFilterConfig): number {
  const dt = config.hopSize / config.sampleRate;
  const alpha = Math.exp(-dt / config.normTau);
  let precision = 3;
  let rounded = 1;
  while (rounded >= 1) {
    const scale = 10 ** precision;
    rounded = Math.round(alpha * scale) / scale;
    precision += 1;
  }
  return rounded;
}

function linspace(from: number, to: number, count: number): Float64Array {
  const out = new Float64Array(count);
  const step = (to - from) / (count - 1);
  for (let i = 0; i < count; i++) out[i] = from + i * step;
  return out;
}

export interface Features {
  /** ERB band energies, `[frames][nbErb]`, flattened. */
  erb: Float32Array;
  /** Unit-normalised low bins as [real block, imaginary block], flattened. */
  spec: Float32Array;
  frames: number;
}

/**
 * Compute both model inputs for a whole spectrogram.
 *
 * The two normalisations are running averages over time, so this must see the
 * frames in order and from the start of the file. That is also why features
 * are computed for the whole signal even when inference is chunked: chunking
 * the features too would restart these averages mid-file and put a step in the
 * input the model has no reason to expect.
 */
export function computeFeatures(
  spectrogram: Spectrogram,
  config: DeepFilterConfig,
  widths: number[] = erbWidths(config),
): Features {
  const { nbErb, nbDf } = config;
  const alpha = normAlpha(config);
  const frames = spectrogram.frames.length;

  const meanState = linspace(MEAN_NORM_INIT[0], MEAN_NORM_INIT[1], nbErb);
  const unitState = linspace(UNIT_NORM_INIT[0], UNIT_NORM_INIT[1], nbDf);

  const erb = new Float32Array(frames * nbErb);
  // Laid out as the model wants it: [1, 2, frames, nbDf], real block first.
  const spec = new Float32Array(2 * frames * nbDf);

  for (let t = 0; t < frames; t++) {
    const spectrum = spectrogram.frames[t];

    // ERB band power, then dB, then subtract the running per-band mean.
    let bin = 0;
    for (let b = 0; b < nbErb; b++) {
      const width = widths[b];
      let sum = 0;
      for (let j = 0; j < width; j++) {
        const re = spectrum[2 * (bin + j)];
        const im = spectrum[2 * (bin + j) + 1];
        sum += re * re + im * im;
      }
      bin += width;

      const db = 10 * Math.log10(sum / width + 1e-10);
      meanState[b] = db * (1 - alpha) + meanState[b] * alpha;
      erb[t * nbErb + b] = (db - meanState[b]) / 40;
    }

    // Complex low bins divided by the square root of their running magnitude.
    for (let k = 0; k < nbDf; k++) {
      const re = spectrum[2 * k];
      const im = spectrum[2 * k + 1];
      unitState[k] = Math.hypot(re, im) * (1 - alpha) + unitState[k] * alpha;
      const norm = Math.sqrt(unitState[k]);
      spec[t * nbDf + k] = re / norm;
      spec[frames * nbDf + t * nbDf + k] = im / norm;
    }
  }

  return { erb, spec, frames };
}

/** What the three graphs produce for one span of frames. */
export interface ModelOutput {
  /** ERB gains, `[frames][nbErb]`. */
  gains: Float32Array;
  /** Deep filter taps, `[frames][nbDf][dfOrder]` as interleaved [re, im]. */
  coefs: Float32Array;
  /** Local SNR estimate per frame, in dB. */
  lsnr: Float32Array;
}

/** One inference call over a contiguous span of frames. */
export type SpanRunner = (erb: Float32Array, spec: Float32Array, frames: number) => ModelOutput;

export interface ChunkOptions {
  /**
   * Frames per inference call.
   *
   * The graphs unroll their recurrence over whatever time axis they are
   * handed, so a whole file in one call would be both correct and, on a long
   * recording, ruinous — the encoder's intermediate tensors alone run to
   * hundreds of megabytes.
   */
  chunkFrames: number;
  /**
   * Frames prepended to each chunk, whose outputs are not kept outright.
   *
   * Splitting restarts the GRU state at each seam, and this model's state
   * takes a long time to settle — measured against a whole-file run, a frame
   * needs several hundred frames of history before its local-SNR estimate is
   * within half a dB, and it is still creeping at 800. So the warm-up is
   * generous; it costs only compute, which is not the binding constraint.
   */
  warmupFrames: number;
}

/**
 * Span is chunk + warm-up, and it sets peak memory: ONNX Runtime holds the
 * whole span's activations, measured at roughly 210 MB per 1000 frames. 2000
 * frames is about 420 MB at the peak of one inference call, which is the most
 * worth spending inside a desktop app for a stage that has a working classical
 * alternative.
 *
 * What this split costs is small and was measured rather than assumed. Against
 * a whole-file run the chunked render differs by about -61 dB, but that
 * difference is not damage — neither estimate is ground truth — so the number
 * that decided these values is SI-SDR against clean audio, where a whole-file
 * run scores 25.58 dB on a noisy fixture and 58.03 on a clean one, and this
 * split scores 25.62 and 57.88. Crossfading the seam was tried and removed: it
 * moved neither figure, because the residual difference is spread through the
 * chunk rather than concentrated at the join.
 */
export const DEFAULT_CHUNK_OPTIONS: ChunkOptions = {
  chunkFrames: 1000, // 10 s at 48 kHz
  warmupFrames: 1000, // 10 s
};

/**
 * Run a feature sequence through `runSpan` in chunks and stitch the result.
 *
 * Only the inference is chunked. The features were computed once over the
 * whole file so their running normalisation never restarts; the only
 * discontinuity here is the network's own hidden state, which the warm-up
 * covers. Taking a callback keeps the index arithmetic — the part that can be
 * wrong in a way no ear would localise — testable without weights present.
 */
export function runChunked(
  features: Features,
  config: DeepFilterConfig,
  options: ChunkOptions,
  runSpan: SpanRunner,
  onProgress: (fraction: number) => void = () => {},
): ModelOutput {
  const { nbErb, nbDf, dfOrder } = config;
  const { chunkFrames, warmupFrames } = options;
  const { erb, spec, frames } = features;
  const coefsPerFrame = nbDf * dfOrder * 2;

  if (frames <= chunkFrames) {
    onProgress(1);
    return runSpan(erb, spec, frames);
  }

  const gains = new Float32Array(frames * nbErb);
  const coefs = new Float32Array(frames * coefsPerFrame);
  const lsnr = new Float32Array(frames);

  for (let start = 0; start < frames; start += chunkFrames) {
    const end = Math.min(frames, start + chunkFrames);
    const from = Math.max(0, start - warmupFrames);
    const span = end - from;

    // The complex feature is two contiguous blocks over the whole file, so a
    // slice of it has to be assembled from both.
    const spanSpec = new Float32Array(2 * span * nbDf);
    spanSpec.set(spec.subarray(from * nbDf, end * nbDf), 0);
    spanSpec.set(spec.subarray((frames + from) * nbDf, (frames + end) * nbDf), span * nbDf);

    const out = runSpan(erb.slice(from * nbErb, end * nbErb), spanSpec, span);

    const skip = start - from; // warm-up frames, discarded
    gains.set(out.gains.subarray(skip * nbErb, span * nbErb), start * nbErb);
    coefs.set(out.coefs.subarray(skip * coefsPerFrame, span * coefsPerFrame), start * coefsPerFrame);
    lsnr.set(out.lsnr.subarray(skip, span), start);
    onProgress(end / frames);
  }

  return { gains, coefs, lsnr };
}

/**
 * Which of the two stages to run for a frame, from its local SNR.
 *
 * Ported from libDF's `apply_stages`. The top branch matters most for
 * transparency: above `maxDbErbThresh` the model declines to touch the frame
 * at all, which is the same instinct the rest of this pipeline is built on.
 * The bottom branch is a mute, which sounds worse than it reads — but the
 * attenuation-limit mix in {@link applyModel} turns it into an attenuation of
 * exactly the requested reduction rather than digital silence.
 */
export function stagesFor(
  lsnr: number,
  config: DeepFilterConfig,
): { gains: boolean; zeros: boolean; deepFilter: boolean } {
  if (lsnr < config.minDbThresh) return { gains: false, zeros: true, deepFilter: false };
  if (lsnr > config.maxDbErbThresh) return { gains: false, zeros: false, deepFilter: false };
  if (lsnr > config.maxDbDfThresh) return { gains: true, zeros: false, deepFilter: false };
  return { gains: true, zeros: false, deepFilter: true };
}

/**
 * Apply the model's output to the noisy spectrogram.
 *
 * `output[t]` is built from model frame `t + lookahead`, per the module note:
 * the graph emits its estimate for frame `t` two frames late. The deep filter
 * reads the *noisy* spectra, not the mask's output — it is a separate estimate
 * of the same frame from a span of neighbours, and it replaces rather than
 * refines the low bins.
 *
 * ## The ERB mask is only valid above `nbDf`
 *
 * This is the one place that deliberately departs from libDF's runtime, and it
 * is worth the paragraph. The training graph keeps the masked spectrum only
 * for the bins the deep filter does not cover — `spec_e[..., nb_df:, :] =
 * spec_m[..., nb_df:, :]` — so the network was never given a reason to emit
 * sensible ERB gains below that boundary, and it does not: on clean speech the
 * bands below bin 96 come back at about 0.25, a flat -12 dB, while the bands
 * above it correctly sit near unity.
 *
 * libDF gets away with masking the whole spectrum because the deep filter then
 * overwrites the low bins. But its `apply_stages` has a branch — local SNR
 * between `maxDbDfThresh` and `maxDbErbThresh` — where the mask runs and the
 * deep filter does not, and there the junk gains survive into the output.
 * Measured on the fixture used to develop this: applying the mask across the
 * full spectrum scores 3.8 dB SI-SDR where confining it to the bins it was
 * trained for scores 25.4, and on *clean* input the difference is 6.6 dB
 * against 51.6 — the mask below `nbDf` does not merely fail to help, it
 * destroys speech that needed nothing. So the mask is applied only from
 * `nbDf` up, and the low bins are either deep-filtered or left alone.
 *
 * `attenuationLimitDb` is upstream's `atten_lim_db` and is how a target
 * reduction is honoured: mixing a fixed fraction of the noisy spectrum back in
 * bounds the attenuation of any bin to that many dB, while leaving bins the
 * model already passes through essentially untouched. A plain wet/dry blend
 * would instead dilute the speech by the same fraction it dilutes the noise.
 */
export function applyModel(
  noisy: Float64Array[],
  model: ModelOutput,
  config: DeepFilterConfig,
  widths: number[],
  attenuationLimitDb: number,
): { frames: Float64Array[]; framesGained: number; framesDeepFiltered: number; framesLeftAlone: number } {
  const { nbErb, nbDf, dfOrder, lookahead } = config;
  const bins = noisy[0].length / 2;
  const limit = attenuationLimitDb > 0 ? 10 ** (-attenuationLimitDb / 20) : 0;

  const out: Float64Array[] = [];
  let framesGained = 0;
  let framesDeepFiltered = 0;
  let framesLeftAlone = 0;

  for (let t = 0; t < noisy.length; t++) {
    const source = noisy[t];
    const enhanced = new Float64Array(2 * bins);
    const m = t + lookahead;
    const stages =
      m < model.lsnr.length
        ? stagesFor(model.lsnr[m], config)
        : { gains: false, zeros: false, deepFilter: false };

    if (stages.gains) {
      // The mask above nbDf, the source below it — the deep filter overwrites
      // the low bins next when it runs, and when it does not, passing them
      // through is the only honest thing left to do with them. The test is
      // per bin rather than per band because the band holding bin nbDf
      // straddles the boundary.
      enhanced.set(source);
      let bin = 0;
      for (let b = 0; b < nbErb; b++) {
        const gain = model.gains[m * nbErb + b];
        for (let j = 0; j < widths[b]; j++) {
          const k = bin + j;
          if (k < nbDf) continue;
          enhanced[2 * k] = source[2 * k] * gain;
          enhanced[2 * k + 1] = source[2 * k + 1] * gain;
        }
        bin += widths[b];
      }
      framesGained++;
    } else if (stages.zeros) {
      // enhanced is already zero
    } else {
      enhanced.set(source);
      framesLeftAlone++;
    }

    if (stages.deepFilter) {
      // out[t][f] = Σ_o noisy[t - lookahead + o][f] · coefs[t + lookahead][f][o]
      for (let k = 0; k < nbDf; k++) {
        let re = 0;
        let im = 0;
        for (let o = 0; o < dfOrder; o++) {
          const at = t - lookahead + o;
          if (at < 0 || at >= noisy.length) continue;
          const sRe = noisy[at][2 * k];
          const sIm = noisy[at][2 * k + 1];
          const c = 2 * ((m * nbDf + k) * dfOrder + o);
          const cRe = model.coefs[c];
          const cIm = model.coefs[c + 1];
          re += sRe * cRe - sIm * cIm;
          im += sRe * cIm + sIm * cRe;
        }
        enhanced[2 * k] = re;
        enhanced[2 * k + 1] = im;
      }
      framesDeepFiltered++;
    }

    if (limit > 0) {
      for (let k = 0; k < bins; k++) {
        enhanced[2 * k] = enhanced[2 * k] * (1 - limit) + source[2 * k] * limit;
        enhanced[2 * k + 1] = enhanced[2 * k + 1] * (1 - limit) + source[2 * k + 1] * limit;
      }
    }

    out.push(enhanced);
  }

  return { frames: out, framesGained, framesDeepFiltered, framesLeftAlone };
}
