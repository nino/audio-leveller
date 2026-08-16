/**
 * Long-term average spectrum (LTAS), measured the way the EQ needs it.
 *
 * Two decisions here matter more than the Welch mechanics:
 *
 * - **Speech frames only.** Averaging over the whole file lets the pauses'
 *   noise floor drag the spectrum down between syllables, so the curve stops
 *   describing the voice. The caller passes the speech ranges (the silence
 *   analysis already knows them) and frames wholly inside them are averaged.
 *
 * - **Fractional-octave smoothing on a log grid.** Raw FFT bins are linearly
 *   spaced, so at the low end one bin spans a third of an octave and at the
 *   top hundreds of bins fit inside one. Ears work in octaves; the EQ reasons
 *   on a log-spaced curve where each point averages the PSD across a constant
 *   fraction of an octave.
 */

import { hannWindow, powerSpectrum } from "./fft";

export interface LtasOptions {
  /** FFT size. 8192 at 48 kHz gives ~5.9 Hz raw resolution. */
  fftSize: number;
  /** Log-grid extent. */
  minFreq: number;
  maxFreq: number;
  /** Grid points per octave. */
  pointsPerOctave: number;
  /** Smoothing bandwidth in octaves (total, centred). 1/3 octave by default. */
  smoothingOctaves: number;
  /**
   * Floor on the smoothing bandwidth, in Hz.
   *
   * Fractional-octave smoothing alone is too fine at the bottom: 1/3 octave at
   * 100 Hz is 23 Hz, narrower than the spacing between a voice's harmonics. The
   * LTAS then resolves the harmonic comb rather than the spectral envelope, and
   * an EQ fitted to it would chase peaks and troughs that are the pitch, not
   * the room. Holding the bandwidth to at least ~120 Hz down low — the same
   * shape auditory filters have, roughly constant-Hz below 500 Hz and
   * constant-octave above — measures the envelope instead.
   */
  minBandwidthHz: number;
}

export const DEFAULT_LTAS_OPTIONS: LtasOptions = {
  fftSize: 8192,
  minFreq: 50,
  maxFreq: 16000,
  pointsPerOctave: 12,
  smoothingOctaves: 1 / 3,
  minBandwidthHz: 120,
};

export interface Ltas {
  /** Grid frequencies, log-spaced. */
  freqs: Float64Array;
  /** Smoothed level at each grid point, in dB (arbitrary reference). */
  db: Float64Array;
  /** Frames that went into the average. */
  frames: number;
  /**
   * The unsmoothed mean power spectrum, linear bins. Kept because the smoothed
   * grid deliberately blurs the bottom octaves (see `minBandwidthHz`), which
   * makes it useless for questions that need to separate sub-bass from a
   * voice's fundamental — the rumble decision being exactly that.
   */
  psd: Float64Array;
  sampleRate: number;
}

/** Mean power in [lo, hi) Hz of a raw PSD, in dB. */
export function bandEnergyDb(
  psd: Float64Array,
  sampleRate: number,
  lo: number,
  hi: number,
): number {
  const binHz = sampleRate / ((psd.length - 1) * 2);
  const from = Math.max(1, Math.round(lo / binHz));
  const to = Math.min(psd.length - 1, Math.round(hi / binHz));
  if (to < from) return -Infinity;

  let acc = 0;
  for (let k = from; k <= to; k++) acc += psd[k];
  return 10 * Math.log10(acc / (to - from + 1) + 1e-30);
}

export interface SampleRange {
  start: number;
  end: number;
}

/**
 * Mean PSD over frames lying wholly inside the given ranges, then smoothed
 * onto the log grid. Frames hop by half the FFT size. Returns null when the
 * ranges hold less than a handful of frames — an LTAS from two frames is an
 * anecdote, and the caller should decline to EQ rather than fit to it.
 */
export function computeLtas(
  samples: Float32Array,
  sampleRate: number,
  ranges: SampleRange[],
  options: Partial<LtasOptions> = {},
): Ltas | null {
  const opts = { ...DEFAULT_LTAS_OPTIONS, ...options };
  const { fftSize } = opts;
  const hop = fftSize / 2;
  const window = hannWindow(fftSize);

  const sum = new Float64Array(fftSize / 2 + 1);
  let frames = 0;

  for (const range of ranges) {
    for (let at = range.start; at + fftSize <= range.end; at += hop) {
      const psd = powerSpectrum(samples, at, window);
      for (let k = 0; k < sum.length; k++) sum[k] += psd[k];
      frames++;
    }
  }

  if (frames < 4) return null;
  for (let k = 0; k < sum.length; k++) sum[k] /= frames;

  return smoothToLogGrid(sum, sampleRate, opts, frames);
}

/** Fold a linear-bin PSD onto the smoothed log grid. */
export function smoothToLogGrid(
  psd: Float64Array,
  sampleRate: number,
  opts: LtasOptions,
  frames: number,
): Ltas {
  const { minFreq, pointsPerOctave, smoothingOctaves, minBandwidthHz } = opts;
  const maxFreq = Math.min(opts.maxFreq, sampleRate / 2 - 1);
  const binHz = sampleRate / ((psd.length - 1) * 2);

  const octaves = Math.log2(maxFreq / minFreq);
  const points = Math.max(2, Math.round(octaves * pointsPerOctave) + 1);

  const freqs = new Float64Array(points);
  const db = new Float64Array(points);
  const half = Math.pow(2, smoothingOctaves / 2);

  for (let p = 0; p < points; p++) {
    const f = minFreq * Math.pow(2, (p / (points - 1)) * octaves);
    freqs[p] = f;

    // Fractional-octave bandwidth, widened to the Hz floor where it is too
    // narrow to span a voice's harmonic spacing.
    let loHz = f / half;
    let hiHz = f * half;
    if (hiHz - loHz < minBandwidthHz) {
      loHz = f - minBandwidthHz / 2;
      hiHz = f + minBandwidthHz / 2;
    }

    let lo = Math.max(1, Math.floor(loHz / binHz));
    let hi = Math.min(psd.length - 1, Math.ceil(hiHz / binHz));
    if (hi < lo) hi = lo;

    let acc = 0;
    for (let k = lo; k <= hi; k++) acc += psd[k];
    db[p] = 10 * Math.log10(acc / (hi - lo + 1) + 1e-30);
  }

  return { freqs, db, frames, psd, sampleRate };
}

/**
 * The reference the corrective EQ pulls deviations toward: the LTAS smoothed
 * much more broadly (default 1.5 octaves). Correcting toward a heavily
 * smoothed version of *itself* removes resonances and notches while leaving
 * the voice's broad tilt — its actual character — alone. An absolute target
 * curve would impose one announcer's timbre on every voice.
 */
export function broadTarget(ltas: Ltas, smoothingOctaves = 1.5): Float64Array {
  const { freqs, db } = ltas;
  const out = new Float64Array(db.length);
  const half = smoothingOctaves / 2;

  for (let i = 0; i < db.length; i++) {
    // Average in *power*, over points within the smoothing half-width.
    let acc = 0;
    let count = 0;
    for (let j = 0; j < db.length; j++) {
      if (Math.abs(Math.log2(freqs[j] / freqs[i])) > half) continue;
      acc += Math.pow(10, db[j] / 10);
      count++;
    }
    out[i] = 10 * Math.log10(acc / Math.max(1, count) + 1e-30);
  }

  return out;
}
