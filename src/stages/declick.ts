/**
 * De-click as a pipeline stage.
 *
 * Sits first in the chain, and the ordering is earning its keep twice over:
 * clicks are out-of-distribution impulses for the AI denoiser (which smears
 * them into chirps instead of removing them), and – as the eval corpus
 * showed – a click landing in a pause can lift it over the silence threshold
 * and merge two speech segments, so de-clicking also repairs the leveller's
 * segmentation.
 *
 * Rate-agnostic: the AR model order is chosen per sample rate instead of
 * demanding a conversion.
 */

import { declick, DEFAULT_DECLICK_OPTIONS, type DeclickOptions } from "../dsp/declick";
import type { Stage, StageOutput } from "../pipeline/types";

export type DeclickParams = DeclickOptions;

export interface DeclickStageReport {
  /** Bursts detected, per channel then by position. */
  bursts: { channel: number; start: number; end: number; peakSigma: number; repaired: boolean }[];
  detected: number;
  repaired: number;
  samplesRepaired: number;
  repairedFraction: number;
  aborted: boolean;
}

export const declickStage: Stage<DeclickParams, DeclickStageReport> = {
  name: "declick",
  description: "Detect impulsive clicks and rebuild them by AR interpolation",
  defaults: DEFAULT_DECLICK_OPTIONS,

  render(signal, params, ctx): StageOutput<DeclickStageReport> {
    const result = declick(signal.channels, signal.sampleRate, params);
    ctx.progress(1);

    return {
      signal: { sampleRate: signal.sampleRate, channels: result.channels, length: signal.length },
      report: {
        bursts: result.bursts,
        detected: result.bursts.length,
        repaired: result.repaired,
        samplesRepaired: result.samplesRepaired,
        repairedFraction: result.repairedFraction,
        aborted: result.aborted,
      },
    };
  },
};
