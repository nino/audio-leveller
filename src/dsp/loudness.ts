/**
 * BS.1770 / EBU R128 loudness measurement.
 *
 * Works on *pre-filtered* (K-weighted) channels so the expensive filtering
 * happens once and sub-ranges can be measured cheaply. See {@link kWeight}.
 */

import { kWeight } from "./kweighting";

/** The BS.1770 loudness calibration offset (dB). */
const OFFSET = -0.691;

/** Absolute silence gate (LUFS). Blocks quieter than this never count. */
const ABSOLUTE_GATE = -70;

/** Relative gate, in LU below the ungated mean loudness. */
const RELATIVE_GATE = -10;

/**
 * Per-channel weighting from BS.1770. Channels 0-2 (L/R/C) weight 1.0;
 * surround channels (index >= 3) weight ~1.41. The LFE channel is excluded
 * in the full spec, but we keep this simple for mono/stereo spoken word.
 */
function channelWeight(index: number): number {
  return index >= 3 ? 1.41 : 1.0;
}

/** K-weight every channel once, up front. */
export function preFilter(channels: Float32Array[], sampleRate: number): Float32Array[] {
  return channels.map((ch) => kWeight(ch, sampleRate));
}

/**
 * Weighted mean square ("z" in the spec) of pre-filtered channels over the
 * half-open sample range [start, end).
 */
export function meanSquare(
  filtered: Float32Array[],
  start: number,
  end: number,
): number {
  const n = end - start;
  if (n <= 0) return 0;
  let sum = 0;
  for (let c = 0; c < filtered.length; c++) {
    const ch = filtered[c];
    let acc = 0;
    for (let i = start; i < end; i++) acc += ch[i] * ch[i];
    sum += channelWeight(c) * (acc / n);
  }
  return sum;
}

/** Convert a weighted mean square to loudness in LUFS. */
export function loudnessFromMeanSquare(z: number): number {
  if (z <= 0) return -Infinity;
  return OFFSET + 10 * Math.log10(z);
}

/** Ungated loudness of pre-filtered channels over [start, end) in LUFS. */
export function loudnessOfRange(
  filtered: Float32Array[],
  start: number,
  end: number,
): number {
  return loudnessFromMeanSquare(meanSquare(filtered, start, end));
}

/**
 * Gated integrated loudness (LUFS) over the sample range [start, end),
 * following the BS.1770 block gating procedure:
 *   - 400 ms blocks with 75% overlap (100 ms hop)
 *   - drop blocks below the absolute -70 LUFS gate
 *   - drop blocks below (mean loudness of survivors - 10 LU)
 *   - integrated loudness = loudness of the mean-square of the survivors
 *
 * For ranges too short to hold a single 400 ms block, falls back to the
 * plain ungated loudness of the range.
 */
export function integratedLoudness(
  filtered: Float32Array[],
  sampleRate: number,
  start = 0,
  end = filtered[0]?.length ?? 0,
): number {
  const blockSamples = Math.round(0.4 * sampleRate);
  const hopSamples = Math.round(0.1 * sampleRate);
  const total = end - start;

  if (total < blockSamples) {
    return loudnessOfRange(filtered, start, end);
  }

  // Mean square of each overlapping block.
  const blocks: number[] = [];
  for (let s = start; s + blockSamples <= end; s += hopSamples) {
    blocks.push(meanSquare(filtered, s, s + blockSamples));
  }

  // Absolute gate.
  const absSurvivors = blocks.filter((z) => loudnessFromMeanSquare(z) >= ABSOLUTE_GATE);
  if (absSurvivors.length === 0) return -Infinity;

  // Relative gate, computed from the mean of the absolute-gated blocks.
  const absMean = absSurvivors.reduce((a, b) => a + b, 0) / absSurvivors.length;
  const relThreshold = loudnessFromMeanSquare(absMean) + RELATIVE_GATE;

  const relSurvivors = blocks.filter(
    (z) =>
      loudnessFromMeanSquare(z) >= ABSOLUTE_GATE &&
      loudnessFromMeanSquare(z) >= relThreshold,
  );
  if (relSurvivors.length === 0) return -Infinity;

  const mean = relSurvivors.reduce((a, b) => a + b, 0) / relSurvivors.length;
  return loudnessFromMeanSquare(mean);
}

/**
 * Loudness range (EBU Tech 3342), in LU.
 *
 * The spread between a recording's quiet and loud passages, measured on 3-second
 * short-term blocks: the 10th to 95th percentile of the blocks that survive a
 * relative gate 20 LU below the 95th percentile. It answers the question
 * integrated loudness cannot — whether a programme sitting on target does so
 * evenly or by averaging a shout and a mumble.
 *
 * It is what a compressor is for, and therefore what tells a compressor whether
 * it has anything to do.
 */
export function loudnessRange(filtered: Float32Array[], sampleRate: number): number {
  const window = Math.round(3 * sampleRate);
  const hop = Math.round(sampleRate);
  const length = filtered[0]?.length ?? 0;
  if (length < window) return 0;

  const blocks: number[] = [];
  for (let start = 0; start + window <= length; start += hop) {
    const value = loudnessOfRange(filtered, start, start + window);
    // -70 LUFS absolute gate, as for integrated loudness.
    if (value >= -70) blocks.push(value);
  }
  if (blocks.length < 2) return 0;

  blocks.sort((a, b) => a - b);
  const at = (p: number): number => blocks[Math.min(blocks.length - 1, Math.floor(blocks.length * p))];
  const gated = blocks.filter((v) => v > at(0.95) - 20);
  if (gated.length < 2) return 0;

  const pick = (p: number): number => gated[Math.min(gated.length - 1, Math.floor(gated.length * p))];
  return pick(0.95) - pick(0.1);
}
