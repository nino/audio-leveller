/**
 * Biquad filter design (RBJ audio-EQ cookbook) and analytic response.
 *
 * The K-weighting filters in `kweighting.ts` predate this file and keep their
 * own analytically-derived coefficients; this is the general toolkit the EQ
 * stages draw on. Application reuses `applyBiquad` from there – same
 * `BiquadCoefficients` shape, normalised so a0 = 1.
 */

import type { BiquadCoefficients } from "./kweighting";
import { applyBiquad } from "./kweighting";

export type { BiquadCoefficients };

/** Peaking (bell) EQ: `gainDb` at `freq`, bandwidth set by `q`. */
export function peakingEq(
  sampleRate: number,
  freq: number,
  gainDb: number,
  q: number,
): BiquadCoefficients {
  const A = Math.pow(10, gainDb / 40);
  const w0 = (2 * Math.PI * freq) / sampleRate;
  const alpha = Math.sin(w0) / (2 * q);
  const cosW0 = Math.cos(w0);

  const a0 = 1 + alpha / A;
  return {
    b0: (1 + alpha * A) / a0,
    b1: (-2 * cosW0) / a0,
    b2: (1 - alpha * A) / a0,
    a1: (-2 * cosW0) / a0,
    a2: (1 - alpha / A) / a0,
  };
}

/** Low shelf: `gainDb` below `freq`, S = 1 slope. */
export function lowShelf(sampleRate: number, freq: number, gainDb: number): BiquadCoefficients {
  const A = Math.pow(10, gainDb / 40);
  const w0 = (2 * Math.PI * freq) / sampleRate;
  const cosW0 = Math.cos(w0);
  const alpha = (Math.sin(w0) / 2) * Math.SQRT2;
  const twoSqrtAAlpha = 2 * Math.sqrt(A) * alpha;

  const a0 = A + 1 + (A - 1) * cosW0 + twoSqrtAAlpha;
  return {
    b0: (A * (A + 1 - (A - 1) * cosW0 + twoSqrtAAlpha)) / a0,
    b1: (2 * A * (A - 1 - (A + 1) * cosW0)) / a0,
    b2: (A * (A + 1 - (A - 1) * cosW0 - twoSqrtAAlpha)) / a0,
    a1: (-2 * (A - 1 + (A + 1) * cosW0)) / a0,
    a2: (A + 1 + (A - 1) * cosW0 - twoSqrtAAlpha) / a0,
  };
}

/** High shelf: `gainDb` above `freq`, S = 1 slope. */
export function highShelf(sampleRate: number, freq: number, gainDb: number): BiquadCoefficients {
  const A = Math.pow(10, gainDb / 40);
  const w0 = (2 * Math.PI * freq) / sampleRate;
  const cosW0 = Math.cos(w0);
  const alpha = (Math.sin(w0) / 2) * Math.SQRT2;
  const twoSqrtAAlpha = 2 * Math.sqrt(A) * alpha;

  const a0 = A + 1 - (A - 1) * cosW0 + twoSqrtAAlpha;
  return {
    b0: (A * (A + 1 + (A - 1) * cosW0 + twoSqrtAAlpha)) / a0,
    b1: (-2 * A * (A - 1 + (A + 1) * cosW0)) / a0,
    b2: (A * (A + 1 + (A - 1) * cosW0 - twoSqrtAAlpha)) / a0,
    a1: (2 * (A - 1 - (A + 1) * cosW0)) / a0,
    a2: (A + 1 - (A - 1) * cosW0 - twoSqrtAAlpha) / a0,
  };
}

/** Second-order Butterworth high-pass (-3 dB at `freq`). */
export function butterworthHighPass(sampleRate: number, freq: number): BiquadCoefficients {
  const w0 = (2 * Math.PI * freq) / sampleRate;
  const cosW0 = Math.cos(w0);
  const alpha = Math.sin(w0) / Math.SQRT2; // Q = 1/sqrt(2)

  const a0 = 1 + alpha;
  return {
    b0: (1 + cosW0) / 2 / a0,
    b1: -(1 + cosW0) / a0,
    b2: (1 + cosW0) / 2 / a0,
    a1: (-2 * cosW0) / a0,
    a2: (1 - alpha) / a0,
  };
}

/** Magnitude response of one biquad at `freq`, in dB. */
export function magnitudeDb(c: BiquadCoefficients, freq: number, sampleRate: number): number {
  const w = (2 * Math.PI * freq) / sampleRate;
  const cos1 = Math.cos(w);
  const cos2 = Math.cos(2 * w);
  const sin1 = Math.sin(w);
  const sin2 = Math.sin(2 * w);

  const numRe = c.b0 + c.b1 * cos1 + c.b2 * cos2;
  const numIm = -(c.b1 * sin1 + c.b2 * sin2);
  const denRe = 1 + c.a1 * cos1 + c.a2 * cos2;
  const denIm = -(c.a1 * sin1 + c.a2 * sin2);

  const num = numRe * numRe + numIm * numIm;
  const den = denRe * denRe + denIm * denIm;
  if (den === 0) return Infinity;
  return 10 * Math.log10(num / den);
}

/** Summed magnitude response of a cascade at `freq`, in dB. */
export function cascadeMagnitudeDb(
  cascade: BiquadCoefficients[],
  freq: number,
  sampleRate: number,
): number {
  let db = 0;
  for (const c of cascade) db += magnitudeDb(c, freq, sampleRate);
  return db;
}

/** Run a cascade over one channel. */
export function applyCascade(input: Float32Array, cascade: BiquadCoefficients[]): Float32Array {
  let out = input;
  for (const c of cascade) out = applyBiquad(out, c);
  return out;
}
