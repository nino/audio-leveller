/**
 * Silence detection.
 *
 * We slide a short window across the whole file, measure K-weighted loudness
 * in each window, then decide a loudness threshold below which a window is
 * "silence". Runs of silent windows longer than `minSilenceSec` become
 * silence regions; everything else is speech.
 */

import { integratedLoudness, loudnessOfRange } from "./loudness";

export type ThresholdMethod = "fraction" | "otsu";

export interface SilenceOptions {
  /** Detection window length in seconds. */
  windowSec: number;
  /** Hop between windows in seconds (detection resolution). */
  hopSec: number;
  /** Minimum duration for a gap to count as silence. */
  minSilenceSec: number;
  /** How to pick the loudness threshold. */
  method: ThresholdMethod;
  /**
   * For method="fraction": threshold sits this fraction of the way from the
   * estimated noise floor up to the integrated loudness. 0.25 ≈ the user's
   * "a quarter of the way from the floor to the integrated loudness".
   */
  fraction: number;
}

export const DEFAULT_SILENCE_OPTIONS: SilenceOptions = {
  windowSec: 0.1,
  hopSec: 0.025,
  minSilenceSec: 1.0,
  method: "fraction",
  fraction: 0.25,
};

export interface SilenceRegion {
  /** First sample of the silence (inclusive). */
  start: number;
  /** One past the last sample of the silence (exclusive). */
  end: number;
  /** Midpoint sample — the segment boundary. */
  mid: number;
}

export interface SilenceAnalysis {
  regions: SilenceRegion[];
  thresholdLufs: number;
  floorLufs: number;
  integratedLufs: number;
  /** Per-window loudness (LUFS), for diagnostics / plotting. */
  windowLoudness: number[];
  hopSamples: number;
  windowSamples: number;
}

function percentile(sortedAsc: number[], p: number): number {
  if (sortedAsc.length === 0) return -Infinity;
  const idx = Math.min(
    sortedAsc.length - 1,
    Math.max(0, Math.round((p / 100) * (sortedAsc.length - 1))),
  );
  return sortedAsc[idx];
}

/**
 * Otsu's method: find the loudness that best splits the histogram of finite
 * window-loudness values into two clusters (minimising intra-class variance).
 * This is the "smart" bimodal-valley heuristic. Returns the threshold in the
 * same units as the input values (LUFS).
 */
export function otsuThreshold(values: number[], bins = 128): number {
  const finite = values.filter((v) => Number.isFinite(v));
  if (finite.length === 0) return -Infinity;
  let min = Infinity;
  let max = -Infinity;
  for (const v of finite) {
    if (v < min) min = v;
    if (v > max) max = v;
  }
  if (min === max) return min;

  const width = (max - min) / bins;
  const hist = new Array(bins).fill(0);
  for (const v of finite) {
    const b = Math.min(bins - 1, Math.floor((v - min) / width));
    hist[b]++;
  }

  const total = finite.length;
  let sumAll = 0;
  for (let i = 0; i < bins; i++) sumAll += i * hist[i];

  let wB = 0;
  let sumB = 0;
  let maxVar = -1;
  let bestBin = 0;
  for (let i = 0; i < bins; i++) {
    wB += hist[i];
    if (wB === 0) continue;
    const wF = total - wB;
    if (wF === 0) break;
    sumB += i * hist[i];
    const mB = sumB / wB;
    const mF = (sumAll - sumB) / wF;
    const between = wB * wF * (mB - mF) * (mB - mF);
    if (between > maxVar) {
      maxVar = between;
      bestBin = i;
    }
  }
  // Threshold at the upper edge of the best bin.
  return min + (bestBin + 1) * width;
}

/** Compute the loudness threshold for silence given per-window loudness. */
export function computeThreshold(
  windowLoudness: number[],
  integratedLufs: number,
  opts: SilenceOptions,
): { threshold: number; floor: number } {
  const finite = windowLoudness.filter((v) => Number.isFinite(v)).sort((a, b) => a - b);
  const floor = percentile(finite, 10);

  if (opts.method === "otsu") {
    return { threshold: otsuThreshold(windowLoudness), floor };
  }

  // fraction method: floor + f·(integrated − floor)
  const threshold = floor + opts.fraction * (integratedLufs - floor);
  return { threshold, floor };
}

export function analyzeSilence(
  filtered: Float32Array[],
  sampleRate: number,
  options: Partial<SilenceOptions> = {},
): SilenceAnalysis {
  const opts = { ...DEFAULT_SILENCE_OPTIONS, ...options };
  const length = filtered[0]?.length ?? 0;

  const hopSamples = Math.max(1, Math.round(opts.hopSec * sampleRate));
  const windowSamples = Math.max(1, Math.round(opts.windowSec * sampleRate));
  const halfWindow = Math.floor(windowSamples / 2);

  const numFrames = Math.max(0, Math.floor(length / hopSamples));
  const windowLoudness: number[] = new Array(numFrames);
  for (let f = 0; f < numFrames; f++) {
    const center = f * hopSamples + Math.floor(hopSamples / 2);
    const start = Math.max(0, center - halfWindow);
    const end = Math.min(length, center + halfWindow);
    windowLoudness[f] = loudnessOfRange(filtered, start, end);
  }

  const integratedLufs = integratedLoudness(filtered, sampleRate, 0, length);
  const { threshold, floor } = computeThreshold(windowLoudness, integratedLufs, opts);

  // Group consecutive silent frames into runs.
  const minSilenceFrames = Math.max(1, Math.ceil(opts.minSilenceSec / opts.hopSec));
  const regions: SilenceRegion[] = [];
  let runStart = -1;
  for (let f = 0; f <= numFrames; f++) {
    const silent = f < numFrames && windowLoudness[f] < threshold;
    if (silent && runStart < 0) {
      runStart = f;
    } else if (!silent && runStart >= 0) {
      const runFrames = f - runStart;
      if (runFrames >= minSilenceFrames) {
        const startSample = runStart * hopSamples;
        const endSample = Math.min(length, f * hopSamples);
        regions.push({
          start: startSample,
          end: endSample,
          mid: Math.floor((startSample + endSample) / 2),
        });
      }
      runStart = -1;
    }
  }

  return {
    regions,
    thresholdLufs: threshold,
    floorLufs: floor,
    integratedLufs,
    windowLoudness,
    hopSamples,
    windowSamples,
  };
}
