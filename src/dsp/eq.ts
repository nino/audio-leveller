/**
 * Corrective EQ fitting.
 *
 * The deviation to correct is `LTAS − broad target` (see `ltas.ts`): the
 * resonances and notches a room or a microphone stamped on the voice, with the
 * voice's own broad tilt already subtracted out. The fitter places a small
 * number of peaking filters against that deviation, greedily: find the worst
 * remaining deviation, place a band that cancels it, subtract that band's
 * analytic response from the deviation, repeat.
 *
 * What separates "professionally corrected" from "obviously auto-EQ'd" is the
 * constraint set, not the fitter:
 *
 *   - few bands (default 5) – a forest of filters is a spectral match, and
 *     spectral matches sound processed;
 *   - gain clamped to ±6 dB, and *cut-biased*: cuts are capped at the full
 *     ±6 dB but boosts at half that. Cutting a resonance is nearly always
 *     safe; boosting a notch dredges up whatever lives down there;
 *   - no boosts where the noise floor is close – boosting a band whose SNR is
 *     poor is buying timbre with noise;
 *   - deviations under a threshold (2 dB) are left alone entirely. Small
 *     wiggles are what voices sound like.
 */

import type { BiquadCoefficients } from "./biquad";
import { peakingEq, butterworthHighPass, magnitudeDb } from "./biquad";
import type { Ltas } from "./ltas";
import { broadTarget, bandEnergyDb } from "./ltas";

export interface EqBand {
  freq: number;
  gainDb: number;
  q: number;
}

export interface EqFitOptions {
  /** Most bands the fitter may place. */
  maxBands: number;
  /** Leave deviations smaller than this alone (dB). */
  minDeviationDb: number;
  /** Cut limit (positive number of dB). */
  maxCutDb: number;
  /** Boost limit – deliberately half the cut limit. */
  maxBoostDb: number;
  /** Q clamp. Wide enough to correct, never surgical. */
  minQ: number;
  maxQ: number;
  /** Fraction of a deviation the band tries to remove. Under-correcting is
   *  how human engineers EQ; full cancellation rings and pumps the fit. */
  correction: number;
  /** No boost where speech-to-noise at that grid point is below this (dB). */
  minBoostSnrDb: number;
  /** Restrict fitting to this range; outside it the mic/room data is unreliable. */
  minFreq: number;
  maxFreq: number;
}

export const DEFAULT_EQ_FIT_OPTIONS: EqFitOptions = {
  maxBands: 5,
  minDeviationDb: 2,
  maxCutDb: 6,
  maxBoostDb: 3,
  minQ: 0.7,
  maxQ: 4,
  correction: 0.8,
  minBoostSnrDb: 15,
  minFreq: 80,
  maxFreq: 12000,
};

export interface EqFit {
  bands: EqBand[];
  /** Max |deviation| before fitting, within the fit range (dB). */
  deviationBeforeDb: number;
  /** Max |deviation| the fitter predicts after its bands are applied (dB). */
  deviationAfterDb: number;
}

/**
 * Estimate a band's Q from how wide the deviation bump is: walk outward from
 * the peak until the deviation falls to half, convert that width to Q.
 */
function estimateQ(
  deviation: Float64Array,
  freqs: Float64Array,
  peakIndex: number,
  options: EqFitOptions,
): number {
  const peak = deviation[peakIndex];
  const halfLevel = peak / 2;
  const sameSign = (v: number): boolean => (peak > 0 ? v > 0 : v < 0);

  let lo = peakIndex;
  while (lo > 0 && sameSign(deviation[lo - 1]) && Math.abs(deviation[lo - 1]) > Math.abs(halfLevel)) {
    lo--;
  }
  let hi = peakIndex;
  while (
    hi < deviation.length - 1 &&
    sameSign(deviation[hi + 1]) &&
    Math.abs(deviation[hi + 1]) > Math.abs(halfLevel)
  ) {
    hi++;
  }

  const bandwidthOctaves = Math.max(0.1, Math.log2(freqs[hi] / freqs[lo]));
  // Standard octave-bandwidth to Q conversion.
  const q = Math.sqrt(Math.pow(2, bandwidthOctaves)) / (Math.pow(2, bandwidthOctaves) - 1);
  return Math.min(options.maxQ, Math.max(options.minQ, q));
}

/**
 * Fit peaking bands to flatten `speech`'s deviation from its own broad shape.
 * `noise` (the LTAS of the pauses, same grid) gates boosts; pass null to
 * forbid boosting outright.
 */
export function fitCorrectiveEq(
  speech: Ltas,
  noise: Ltas | null,
  sampleRate: number,
  options: Partial<EqFitOptions> = {},
): EqFit {
  const opts = { ...DEFAULT_EQ_FIT_OPTIONS, ...options };
  const { freqs } = speech;
  const target = broadTarget(speech);

  // Deviation, masked to the trustworthy range.
  const deviation = new Float64Array(freqs.length);
  const inRange = new Array<boolean>(freqs.length);
  for (let i = 0; i < freqs.length; i++) {
    inRange[i] = freqs[i] >= opts.minFreq && freqs[i] <= opts.maxFreq;
    deviation[i] = inRange[i] ? speech.db[i] - target[i] : 0;
  }

  const maxAbs = (): { value: number; index: number } => {
    let value = 0;
    let index = -1;
    for (let i = 0; i < deviation.length; i++) {
      if (!inRange[i]) continue;
      if (Math.abs(deviation[i]) > value) {
        value = Math.abs(deviation[i]);
        index = i;
      }
    }
    return { value, index };
  };

  const deviationBeforeDb = maxAbs().value;
  const bands: EqBand[] = [];

  // Bounded by *placed* bands, not by attempts: a deviation that gets masked
  // out (a notch we decline to fill) must not consume the band budget, or a
  // few ungated notches early on leave nothing for the resonances that follow.
  // `attempts` is only a runaway guard – each iteration either places a band
  // or removes a grid point from play, so it always terminates.
  for (let attempts = 0; bands.length < opts.maxBands && attempts < freqs.length; attempts++) {
    const worst = maxAbs();
    if (worst.index < 0 || worst.value < opts.minDeviationDb) break;

    const freq = freqs[worst.index];
    const rawGain = -deviation[worst.index] * opts.correction;

    // Boosting into a poor noise floor is buying timbre with noise.
    if (rawGain > 0) {
      const snr = noise ? speech.db[worst.index] - noise.db[worst.index] : -Infinity;
      if (snr < opts.minBoostSnrDb) {
        // A notch we may not fill: take it out of play so the fitter spends
        // its remaining bands somewhere useful.
        inRange[worst.index] = false;
        deviation[worst.index] = 0;
        continue;
      }
    }

    const gainDb = Math.max(-opts.maxCutDb, Math.min(opts.maxBoostDb, rawGain));
    const q = estimateQ(deviation, freqs, worst.index, opts);
    const band: EqBand = { freq, gainDb, q };
    bands.push(band);

    // Subtract this band's analytic response from the remaining deviation.
    const coeffs = peakingEq(sampleRate, freq, gainDb, q);
    for (let i = 0; i < freqs.length; i++) {
      if (!inRange[i]) continue;
      deviation[i] += magnitudeDb(coeffs, freqs[i], sampleRate);
    }
  }

  return { bands, deviationBeforeDb, deviationAfterDb: maxAbs().value };
}

export interface RumbleDecision {
  /** Corner frequency of the high-pass, or null when none is warranted. */
  freq: number | null;
  /** Sub-bass excess that triggered it (dB). */
  excessDb: number;
}

/**
 * Decide whether a rumble high-pass is warranted and where to put it.
 *
 * Compares the energy well below the voice (20 Hz to `cornerCandidate`)
 * against the fundamental region (120–300 Hz). Speech has no business carrying
 * energy an octave below its own fundamental; when the sub-bass sits within
 * `marginDb` of the fundamentals, something non-vocal (traffic, handling,
 * HVAC) is down there and the high-pass earns its place. On clean material it
 * stays off – an always-on filter is not "corrective".
 *
 * This reads the **raw** PSD, not the smoothed grid. The grid's minimum
 * bandwidth deliberately blurs the bottom octaves so the EQ sees a spectral
 * envelope instead of a harmonic comb, and that same blur smears a voice's
 * fundamental down into the sub-bass region – enough to trip this test on
 * perfectly clean speech.
 */
export function decideRumbleFilter(
  speech: Ltas,
  cornerCandidate = 80,
  marginDb = -12,
): RumbleDecision {
  const subDb = bandEnergyDb(speech.psd, speech.sampleRate, 20, cornerCandidate);
  const voiceDb = bandEnergyDb(speech.psd, speech.sampleRate, 120, 300);
  if (!Number.isFinite(subDb) || !Number.isFinite(voiceDb)) {
    return { freq: null, excessDb: 0 };
  }

  const excessDb = subDb - voiceDb;
  return excessDb > marginDb
    ? { freq: cornerCandidate, excessDb }
    : { freq: null, excessDb };
}

/** Build the filter cascade for a fit: optional rumble HPF plus the bands. */
export function buildCascade(
  fit: EqFit,
  rumble: RumbleDecision,
  sampleRate: number,
): BiquadCoefficients[] {
  const cascade: BiquadCoefficients[] = [];
  if (rumble.freq !== null) cascade.push(butterworthHighPass(sampleRate, rumble.freq));
  for (const band of fit.bands) {
    cascade.push(peakingEq(sampleRate, band.freq, band.gainDb, band.q));
  }
  return cascade;
}
