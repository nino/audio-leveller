/**
 * Dereverberation as a pipeline stage.
 *
 * Sits between the denoiser and the EQ. After denoising because WPE's
 * statistics are cleaner without a noise floor under them, and before the EQ
 * because removing a room changes the spectrum the EQ would otherwise fit to.
 *
 * The stage engages only when the recording is actually reverberant, on the
 * same principle as the denoiser: single-channel WPE is not free, and running
 * it over a dry recording buys nothing while measurably altering the voice.
 * Reverberation is detected blind, by how long the signal takes to decay after
 * speech stops — no reference and no impulse response needed.
 *
 * On expectations: single-channel WPE is a modest tool. Measured on the eval
 * corpus it takes about 12 % off the decay time and gains about 1 dB of SI-SDR
 * against the dry reference. Most of the published results that sound dramatic
 * are multi-channel, where the spatial information does most of the work. What
 * it does offer is that it cannot invent speech — it only subtracts a linear
 * prediction — so it will never put words in someone's mouth, which a
 * generative enhancer can.
 */

import { dereverb, DEFAULT_DEREVERB_OPTIONS, type DereverbOptions } from "../dsp/dereverb";
import { reverbDecayMs } from "../dsp/reverbtime";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export interface DereverbParams extends DereverbOptions {
  /**
   * Engage only when the measured decay exceeds this, in milliseconds.
   *
   * Dry speech measures around 35 ms — that is the talker's own articulation,
   * not a room. A noticeably live room lands past 150 ms. The default sits
   * between them so ordinary recordings pass through untouched.
   */
  minDecayMs: number;
}

export const DEFAULT_DEREVERB_PARAMS: DereverbParams = {
  ...DEFAULT_DEREVERB_OPTIONS,
  minDecayMs: 90,
};

export interface DereverbStageReport {
  /** Measured decay before and after, in milliseconds. */
  decayBeforeMs: number;
  decayAfterMs: number;
  /** True when the recording was dry enough to leave alone. */
  skipped: boolean;
  binsProcessed: number;
}

export const dereverbStage: Stage<DereverbParams, DereverbStageReport> = {
  name: "dereverb",
  description: "Suppress late reverberation by weighted prediction error (no model)",
  defaults: DEFAULT_DEREVERB_PARAMS,

  render(signal, params, ctx): StageOutput<DereverbStageReport> {
    const decayBeforeMs = reverbDecayMs(signal);

    // A dry recording is returned as the same object, so this stage is
    // bit-transparent rather than merely close on material it should not touch.
    if (!Number.isFinite(decayBeforeMs) || decayBeforeMs < params.minDecayMs) {
      return {
        signal,
        report: {
          decayBeforeMs,
          decayAfterMs: decayBeforeMs,
          skipped: true,
          binsProcessed: 0,
        },
      };
    }

    ctx.progress(0.1);
    const result = dereverb(signal.channels, params);
    const output: Signal = {
      sampleRate: signal.sampleRate,
      channels: result.channels,
      length: signal.length,
    };
    ctx.progress(1);

    return {
      signal: output,
      report: {
        decayBeforeMs,
        decayAfterMs: reverbDecayMs(output),
        skipped: false,
        binsProcessed: result.binsProcessed,
      },
    };
  },
};
