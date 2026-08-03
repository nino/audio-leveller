/**
 * Dynamic EQ: suppress resonances only while they are resonating.
 *
 * The static EQ in `eq.ts` corrects colouration that is present throughout —
 * a room mode, a microphone's character. What it cannot fix is the kind that
 * comes and goes: a vowel that rings on one note, a sibilant that spikes at
 * 7 kHz, a plosive that booms. Those need a filter that reacts.
 *
 * The rule here is the one behind the "spectral smoothing" plugins: for each
 * frame, compare the spectrum against a *smoothed version of itself*, and pull
 * down whatever protrudes. Anything narrow and loud relative to its own
 * neighbourhood is a resonance almost by definition, while the broad shape —
 * which is the voice — passes through untouched. It needs no threshold in dB,
 * which is what makes it work across voices and levels without tuning.
 *
 * Three things keep it from sounding processed:
 *
 * - **Attack and release across frames.** Gain that changes freely frame to
 *   frame modulates the signal at the frame rate, which is audible as a
 *   fluttering artefact. Reacting fast to a rising resonance and slowly to a
 *   falling one is both more natural and less audible.
 *
 * - **A limit on total attenuation**, so a genuinely loud note is tamed rather
 *   than erased.
 *
 * - **Frequency-dependent sensitivity.** Sibilance sits in a known band and is
 *   the most common complaint, so the stage leans harder there. That is the
 *   de-esser: not a separate device, just this one weighted by frequency.
 */

import { stft, istft, binCount, magnitudes, applyGains } from "./stft";

export interface DynEqOptions {
  frameSize: number;
  hopSize: number;
  /** Smoothing half-width, in octaves, for the per-frame reference envelope. */
  smoothingOctaves: number;
  /**
   * Floor on the smoothing window width, in Hz.
   *
   * Without it the reference is too narrow at the bottom to be a reference at
   * all. Half an octave at 400 Hz spans 336-476 Hz, which for a 110 Hz voice
   * holds barely one harmonic — so every harmonic reads as a peak above its own
   * neighbourhood, and the stage attenuates the voice's harmonic structure
   * rather than any resonance. Measured before this existed: 92% of all
   * time-frequency cells were being attenuated on perfectly clean speech.
   */
  minSmoothingHz: number;
  /**
   * Protrusion above the envelope, in dB, before any attenuation starts.
   *
   * Higher than it looks like it should be, because the reference it is
   * compared against is biased low. The envelope is the dB-mean of a
   * neighbourhood of magnitudes, and the dB-mean of a fluctuating spectrum sits
   * well below its typical value — so with a "reasonable" 6 dB threshold, 92%
   * of all time-frequency cells in clean speech read as protruding. Sweeping
   * against both a clean recording and one with a sustained resonance: at 12 dB
   * the stage touches 5% of cells and still delivers the same +4.9 dB SI-SDR on
   * the resonance that 6 dB did. All the extra activity was cost with no
   * benefit.
   */
  thresholdDb: number;
  /** Fraction of the excess above threshold that is removed. */
  ratio: number;
  /** Most attenuation any bin may receive, in dB. */
  maxReductionDb: number;
  /** Time constants, in milliseconds. */
  attackMs: number;
  releaseMs: number;
  /** Sibilance band, given extra sensitivity. */
  sibilanceLowHz: number;
  sibilanceHighHz: number;
  /** How much lower the threshold is inside the sibilance band, in dB. */
  sibilanceExtraDb: number;
  /** Ignore everything below this; the fundamental region is not a resonance. */
  minFreqHz: number;
}

export const DEFAULT_DYNEQ_OPTIONS: DynEqOptions = {
  frameSize: 1024,
  hopSize: 256,
  smoothingOctaves: 0.5,
  minSmoothingHz: 400,
  thresholdDb: 12,
  ratio: 0.5,
  maxReductionDb: 8,
  attackMs: 2,
  releaseMs: 60,
  sibilanceLowHz: 5000,
  sibilanceHighHz: 9000,
  sibilanceExtraDb: 3,
  minFreqHz: 300,
};

export interface DynEqResult {
  channels: Float32Array[];
  /** Worst attenuation applied to any bin, in dB (positive number). */
  maxReductionDb: number;
  /** Mean attenuation over bins that were reduced at all, in dB. */
  meanReductionDb: number;
  /** Fraction of time-frequency cells that were touched at all. */
  activeFraction: number;
}

/**
 * Precompute, for each bin, the range of bins its smoothing window covers.
 * Constant across frames, so it is worth doing once.
 */
function smoothingWindows(
  bins: number,
  sampleRate: number,
  frameSize: number,
  octaves: number,
  minWidthHz: number,
): { lo: Int32Array; hi: Int32Array } {
  const lo = new Int32Array(bins);
  const hi = new Int32Array(bins);
  const binHz = sampleRate / frameSize;
  const half = Math.pow(2, octaves / 2);

  for (let k = 0; k < bins; k++) {
    const freq = k * binHz;
    let loHz = freq > 0 ? freq / half : 0;
    let hiHz = freq > 0 ? freq * half : binHz * 2;
    // Widen to the Hz floor where the octave window is too narrow to hold
    // several harmonics — see `minSmoothingHz`.
    if (hiHz - loHz < minWidthHz) {
      loHz = freq - minWidthHz / 2;
      hiHz = freq + minWidthHz / 2;
    }
    lo[k] = Math.max(1, Math.floor(loHz / binHz));
    hi[k] = Math.min(bins - 1, Math.ceil(hiHz / binHz));
    if (hi[k] < lo[k]) hi[k] = lo[k];
  }

  return { lo, hi };
}

/** Per-bin threshold offset, lower inside the sibilance band. */
function thresholdCurve(
  bins: number,
  sampleRate: number,
  frameSize: number,
  opts: DynEqOptions,
): Float64Array {
  const curve = new Float64Array(bins);
  const binHz = sampleRate / frameSize;

  for (let k = 0; k < bins; k++) {
    const freq = k * binHz;
    if (freq < opts.minFreqHz) {
      // Effectively disabled: the fundamental and the first formant are the
      // voice, not a resonance to be shaved.
      curve[k] = Infinity;
    } else if (freq >= opts.sibilanceLowHz && freq <= opts.sibilanceHighHz) {
      curve[k] = opts.thresholdDb - opts.sibilanceExtraDb;
    } else {
      curve[k] = opts.thresholdDb;
    }
  }

  return curve;
}

/**
 * Suppress dynamic resonances across every channel.
 *
 * Channels are processed independently, which is right for damage but does mean
 * a resonance present in both will be attenuated in both — the desired outcome,
 * since it is the same resonance.
 */
export function dynamicEq(
  channels: Float32Array[],
  sampleRate: number,
  options: Partial<DynEqOptions> = {},
): DynEqResult {
  const opts = { ...DEFAULT_DYNEQ_OPTIONS, ...options };
  const bins = binCount(opts.frameSize);
  const { lo, hi } = smoothingWindows(
    bins,
    sampleRate,
    opts.frameSize,
    opts.smoothingOctaves,
    opts.minSmoothingHz,
  );
  const thresholds = thresholdCurve(bins, sampleRate, opts.frameSize, opts);

  // One hop is the time step for the envelope followers.
  const hopSec = opts.hopSize / sampleRate;
  const attack = Math.exp(-hopSec / Math.max(1e-6, opts.attackMs / 1000));
  const release = Math.exp(-hopSec / Math.max(1e-6, opts.releaseMs / 1000));

  let worst = 0;
  let reductionSum = 0;
  let reducedCells = 0;
  let totalCells = 0;

  const out = channels.map((samples) => {
    const analysis = stft(samples, { frameSize: opts.frameSize, hopSize: opts.hopSize });
    const mags = new Float64Array(bins);
    const targetDb = new Float64Array(bins);
    // Smoothed gain state per bin, in dB (negative = attenuating).
    const state = new Float64Array(bins);
    const gains = new Float64Array(bins);

    for (const frame of analysis.frames) {
      magnitudes(frame, mags);

      for (let k = 0; k < bins; k++) {
        const power = mags[k] * mags[k];
        const levelDb = 10 * Math.log10(power + 1e-30);

        // The reference: this bin's own neighbourhood, averaged in dB. A dB
        // average is deliberate — averaging in power would let the peak lift
        // its own reference and hide from the comparison.
        let acc = 0;
        let count = 0;
        for (let j = lo[k]; j <= hi[k]; j++) {
          acc += 10 * Math.log10(mags[j] * mags[j] + 1e-30);
          count++;
        }
        const envelopeDb = acc / Math.max(1, count);

        const excess = levelDb - envelopeDb - thresholds[k];
        targetDb[k] = excess > 0 ? -Math.min(opts.maxReductionDb, excess * opts.ratio) : 0;
      }

      for (let k = 0; k < bins; k++) {
        // Attack when the required attenuation deepens, release when it eases.
        const coefficient = targetDb[k] < state[k] ? attack : release;
        state[k] = targetDb[k] + (state[k] - targetDb[k]) * coefficient;
        gains[k] = Math.pow(10, state[k] / 20);

        totalCells++;
        if (state[k] < -0.01) {
          reducedCells++;
          reductionSum += -state[k];
          if (-state[k] > worst) worst = -state[k];
        }
      }

      applyGains(frame, gains);
    }

    return istft(analysis);
  });

  return {
    channels: out,
    maxReductionDb: worst,
    meanReductionDb: reducedCells > 0 ? reductionSum / reducedCells : 0,
    activeFraction: totalCells > 0 ? reducedCells / totalCells : 0,
  };
}
