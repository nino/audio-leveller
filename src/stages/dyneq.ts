/**
 * Dynamic EQ as a pipeline stage.
 *
 * Runs after the static EQ – that one has already removed whatever colouration
 * is constant, so what reaches here is the part that comes and goes: ringing
 * vowels, sibilance, the occasional boomy plosive. And before the leveller, so
 * the loudness target is measured on what actually gets written.
 *
 * This is also the de-esser. Sibilance is not a different problem from
 * resonance, only a more common one in a known band, so it is handled by the
 * same mechanism with more sensitivity applied there.
 */

import { dynamicEq, DEFAULT_DYNEQ_OPTIONS, type DynEqOptions } from "../dsp/dyneq";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export type DynEqParams = DynEqOptions;

export interface DynEqStageReport {
  /** Worst attenuation applied to any single bin, in dB. */
  maxReductionDb: number;
  /** Mean attenuation across the cells that were touched, in dB. */
  meanReductionDb: number;
  /**
   * Fraction of time-frequency cells attenuated at all. Worth watching: a
   * value near zero means the stage found nothing, and a large one means it is
   * reshaping the whole recording rather than catching resonances.
   */
  activeFraction: number;
}

export const dyneqStage: Stage<DynEqParams, DynEqStageReport> = {
  name: "dyneq",
  description: "Suppress resonances and sibilance while they occur (dynamic EQ / de-esser)",
  defaults: DEFAULT_DYNEQ_OPTIONS,

  render(signal, params, ctx): StageOutput<DynEqStageReport> {
    const result = dynamicEq(signal.channels, signal.sampleRate, params);
    ctx.progress(1);

    const output: Signal = {
      sampleRate: signal.sampleRate,
      channels: result.channels,
      length: signal.length,
    };

    return {
      signal: output,
      report: {
        maxReductionDb: result.maxReductionDb,
        meanReductionDb: result.meanReductionDb,
        activeFraction: result.activeFraction,
      },
    };
  },
};
