/**
 * Downward expansion as a pipeline stage — the quiet gets quieter.
 *
 * The complement to the denoiser rather than a replacement for it. A denoiser
 * works on the spectrum and removes noise that is *there while the speech is*;
 * an expander works on the level and pushes down the gaps *between* words,
 * where no speech is masking anything and the room is most exposed. Neither
 * does the other's job, which is why the commercial reference this was
 * measured against runs both.
 *
 * That reference attenuates by about 11 dB at 37 dB below its programme
 * loudness and 34 dB at 43 below, a slope near 2.8:1 with the knee about 27 dB
 * under the programme. Those are the defaults here, with one deliberate
 * departure: the attenuation is capped. An uncapped expander is a gate, and a
 * recording gated to digital silence between words sounds broken in a way the
 * noise never did — the room vanishes and reappears with every syllable, which
 * is exactly what the room-tone bed elsewhere in this project exists to avoid.
 *
 * It runs after the tone stages and before the compressor, in that order for a
 * concrete reason. Both stages place their thresholds relative to the programme
 * loudness of the audio handed to them, and this one additionally decides
 * whether to act at all from the programme-to-floor distance it measures.
 * Expanding first leaves those numbers intact: it only attenuates well below
 * the programme, far under the relative gate the loudness measurement applies,
 * so the compressor downstream sees the threshold it would have chosen on its
 * own. Compressing first does not — it pulls the programme down while leaving
 * the floor exactly where it was, narrowing the very distance this stage reads
 * before choosing a threshold, and can flip its skip decision outright.
 */

import { applyDynamics, expanderCurve, type DynamicsTiming } from "../dsp/dynamics";
import { loudnessOfRange, preFilter } from "../dsp/loudness";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export interface ExpandParams {
  /**
   * Threshold, in dB below the programme's integrated loudness.
   *
   * Relative for the same reason the compressor's is: it has to mean the same
   * thing on a quiet recording as on a hot one.
   */
  thresholdBelowProgrammeDb: number;
  /** Expansion ratio below the threshold. */
  ratio: number;
  /** Soft-knee width in dB, centred on the threshold. */
  kneeDb: number;
  /**
   * Hard cap on attenuation, in dB. This is what makes it an expander rather
   * than a gate.
   */
  rangeDb: number;
  /** Time constant while the gain falls — the floor being pushed down, in ms. */
  closeMs: number;
  /** Time constant while the gain recovers — a word starting, in ms. */
  openMs: number;
  /**
   * Programme-to-floor distance, in dB, above which the stage declines.
   *
   * A recording whose floor already sits this far down has nothing left worth
   * pushing, and expanding it would only risk chewing the quiet ends of words
   * for no audible gain.
   */
  cleanFloorDb: number;
}

export const DEFAULT_EXPAND_PARAMS: ExpandParams = {
  thresholdBelowProgrammeDb: 27,
  ratio: 2.8,
  kneeDb: 8,
  rangeDb: 12,
  // Opening fast matters more than closing fast: a slow open clips the front
  // of a word, which is audible, while a slow close merely lets the floor
  // linger a moment longer, which is not.
  closeMs: 150,
  openMs: 5,
  cleanFloorDb: 50,
};

export interface ExpandStageReport {
  /** Detector threshold actually used, in dBFS. */
  thresholdDbfs: number;
  ratio: number;
  /** Measured programme-to-floor distance on the way in, in dB. */
  inputFloorDistanceDb: number;
  /** What the floor in the pauses actually did, in dB (positive = quieter). */
  floorReductionDb: number;
  noiseFloorBeforeLufs: number;
  noiseFloorAfterLufs: number;
  /** Largest attenuation applied, in dB. */
  maxReductionDb: number;
  /** Fraction of the file where the gain differed from unity by more than 0.1 dB. */
  activeFraction: number;
  /** True when the floor was already low enough to leave alone. */
  skipped: boolean;
}

/** Mean loudness across the pause regions — the noise floor, measured. */
function pauseLoudness(signal: Signal, pauses: { start: number; end: number }[]): number {
  if (pauses.length === 0) return -Infinity;
  const filtered = preFilter(signal.channels, signal.sampleRate);

  let totalPower = 0;
  let totalSamples = 0;
  for (const pause of pauses) {
    const length = pause.end - pause.start;
    if (length <= 0) continue;
    const loudness = loudnessOfRange(filtered, pause.start, pause.end);
    if (!Number.isFinite(loudness)) continue;
    totalPower += 10 ** (loudness / 10) * length;
    totalSamples += length;
  }

  return totalSamples > 0 ? 10 * Math.log10(totalPower / totalSamples) : -Infinity;
}

export const expandStage: Stage<ExpandParams, ExpandStageReport> = {
  name: "expand",
  description: "Push the floor down between words, where nothing is masking it",
  defaults: DEFAULT_EXPAND_PARAMS,

  render(signal, params, ctx): StageOutput<ExpandStageReport> {
    const pauses = ctx.analysis.silence().regions.map((r) => ({ start: r.start, end: r.end }));
    const before = pauseLoudness(signal, pauses);
    const programme = ctx.analysis.integratedLufs();
    const distance =
      Number.isFinite(programme) && Number.isFinite(before) ? programme - before : NaN;

    const base: ExpandStageReport = {
      thresholdDbfs: NaN,
      ratio: params.ratio,
      inputFloorDistanceDb: distance,
      floorReductionDb: 0,
      noiseFloorBeforeLufs: before,
      noiseFloorAfterLufs: before,
      maxReductionDb: 0,
      activeFraction: 0,
      skipped: true,
    };

    // Nothing worth pushing: return the signal itself, so a recording with an
    // already-deep floor comes out of this stage bit-identical.
    if (!Number.isFinite(programme) || !Number.isFinite(distance) || distance > params.cleanFloorDb) {
      return { signal, report: base };
    }

    const thresholdDbfs = programme - params.thresholdBelowProgrammeDb;
    const timing: DynamicsTiming = {
      downMs: params.closeMs,
      upMs: params.openMs,
      detectorMs: 15,
    };

    ctx.progress(0.1);
    const result = applyDynamics(
      signal.channels,
      signal.sampleRate,
      expanderCurve({
        thresholdDb: thresholdDbfs,
        ratio: params.ratio,
        kneeDb: params.kneeDb,
        rangeDb: params.rangeDb,
      }),
      timing,
    );
    ctx.progress(0.9);

    const output: Signal = {
      sampleRate: signal.sampleRate,
      channels: result.channels,
      length: signal.length,
    };
    const after = pauseLoudness(output, pauses);

    ctx.progress(1);
    return {
      signal: output,
      report: {
        ...base,
        thresholdDbfs,
        floorReductionDb: Number.isFinite(before) && Number.isFinite(after) ? before - after : 0,
        noiseFloorAfterLufs: after,
        maxReductionDb: result.maxReductionDb,
        activeFraction: result.activeFraction,
        skipped: false,
      },
    };
  },
};
