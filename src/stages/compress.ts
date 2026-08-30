/**
 * Compression as a pipeline stage.
 *
 * This is the stage that separates a recording that has been *levelled* from
 * one that sounds *mastered*. The leveller sets one gain per speech segment,
 * which fixes the difference between a passage recorded close and one recorded
 * far away. It does nothing about the difference between the start and the end
 * of a single sentence, and that is where most of the unevenness in speech
 * actually lives.
 *
 * The numbers here were measured rather than chosen. A 24-minute before/after
 * pair from a commercial service was aligned to the sample and differenced:
 * across 14,441 frames its applied gain falls as input level rises above about
 * 2 dB under the programme loudness, at a slope corresponding to roughly
 * 1.7:1, and its gain trajectory fits a first-order smoother with a 33 ms fall
 * and a 168 ms rise. Within one continuous utterance its gain moves a median
 * of 4.5 dB, against 3.3 dB between utterances – so more than half of what it
 * does is inside a phrase, where a segment leveller cannot reach.
 *
 * Placed after the tone-shaping stages, so it compresses the corrected
 * spectrum rather than a resonance the EQ is about to remove, and before the
 * leveller, so the loudness target is measured on the audio that gets written.
 */

import { applyDynamics, compressorCurve, type DynamicsTiming } from "../dsp/dynamics";
import { loudnessRange, preFilter } from "../dsp/loudness";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export interface CompressParams {
  /**
   * Threshold, in dB relative to the programme's integrated loudness.
   *
   * Relative rather than absolute so it means the same thing on a quiet
   * recording as on a hot one – an absolute dBFS threshold would compress a
   * loud file hard and miss a quiet one entirely. The measured reference sits
   * about 2 dB below its own programme loudness, which puts the knee in the
   * middle of ordinary speech rather than only on its peaks.
   */
  thresholdRelativeDb: number;
  /** Compression ratio above the threshold. */
  ratio: number;
  /** Soft-knee width in dB, centred on the threshold. */
  kneeDb: number;
  /** Time constant while the gain falls, in ms. */
  attackMs: number;
  /** Time constant while the gain recovers, in ms. */
  releaseMs: number;
  /** Hard cap on attenuation, in dB. */
  maxReductionDb: number;
  /**
   * Loudness range, in LU, below which the stage declines to act.
   *
   * Material that is already even has nothing for a compressor to even out,
   * and compressing it only costs what little life it has left. Speech that
   * genuinely needs this measures 5 LU and up; a heavily processed source can
   * arrive at 2 and should be left alone.
   */
  minLoudnessRangeLu: number;
}

export const DEFAULT_COMPRESS_PARAMS: CompressParams = {
  thresholdRelativeDb: -2,
  ratio: 1.7,
  kneeDb: 10,
  attackMs: 33,
  releaseMs: 168,
  maxReductionDb: 12,
  minLoudnessRangeLu: 3,
};

export interface CompressStageReport {
  /** Detector threshold actually used, in dBFS. */
  thresholdDbfs: number;
  ratio: number;
  /** Loudness range before and after, in LU – what the stage is for. */
  loudnessRangeBeforeLu: number;
  loudnessRangeAfterLu: number;
  /** Largest attenuation applied, in dB. */
  maxReductionDb: number;
  /** Mean gain across the programme, in dB (negative: it only ever attenuates). */
  meanGainDb: number;
  /** Fraction of the file where the gain differed from unity by more than 0.1 dB. */
  activeFraction: number;
  /** True when the material was already even enough to leave alone. */
  skipped: boolean;
}

export const compressStage: Stage<CompressParams, CompressStageReport> = {
  name: "compress",
  description: "Even out level within a phrase, where segment levelling cannot reach",
  defaults: DEFAULT_COMPRESS_PARAMS,

  render(signal, params, ctx): StageOutput<CompressStageReport> {
    const filtered = ctx.analysis.filtered();
    const rangeBefore = loudnessRange(filtered, signal.sampleRate);
    const programme = ctx.analysis.integratedLufs();

    const base: CompressStageReport = {
      thresholdDbfs: NaN,
      ratio: params.ratio,
      loudnessRangeBeforeLu: rangeBefore,
      loudnessRangeAfterLu: rangeBefore,
      maxReductionDb: 0,
      meanGainDb: 0,
      activeFraction: 0,
      skipped: true,
    };

    if (!Number.isFinite(programme) || rangeBefore < params.minLoudnessRangeLu) {
      return { signal, report: base };
    }

    // The detector measures RMS in dBFS; the programme is measured as gated
    // K-weighted loudness. For speech the two scales differ by little, and
    // what matters is that the threshold tracks the programme rather than the
    // absolute level, so the offset is carried through as it stands.
    const thresholdDbfs = programme + params.thresholdRelativeDb;
    const timing: DynamicsTiming = {
      downMs: params.attackMs,
      upMs: params.releaseMs,
      detectorMs: 15,
    };

    ctx.progress(0.1);
    const result = applyDynamics(
      signal.channels,
      signal.sampleRate,
      compressorCurve({
        thresholdDb: thresholdDbfs,
        ratio: params.ratio,
        kneeDb: params.kneeDb,
        maxReductionDb: params.maxReductionDb,
      }),
      timing,
    );
    ctx.progress(0.9);

    const output: Signal = {
      sampleRate: signal.sampleRate,
      channels: result.channels,
      length: signal.length,
    };

    ctx.progress(1);
    return {
      signal: output,
      report: {
        ...base,
        thresholdDbfs,
        // Measured, not assumed: a compressor that claims a ratio and delivers
        // nothing is a bug that only this number makes visible.
        loudnessRangeAfterLu: loudnessRange(
          preFilter(output.channels, output.sampleRate),
          output.sampleRate,
        ),
        maxReductionDb: result.maxReductionDb,
        meanGainDb: result.meanGainDb,
        activeFraction: result.activeFraction,
        skipped: false,
      },
    };
  },
};
