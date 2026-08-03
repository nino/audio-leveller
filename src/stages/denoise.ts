/**
 * Noise reduction as a pipeline stage.
 *
 * Sits after de-click (impulses are out-of-distribution for any denoiser —
 * classical or trained — and get smeared rather than removed) and before the
 * EQ, because denoising changes the spectrum the EQ would otherwise be fitting
 * a curve to.
 *
 * The stage measures what it achieved rather than assuming it: the noise floor
 * in the pauses is measured before and after, and that number goes in the
 * report. A denoiser that claims 12 dB and delivers 3 is a bug you cannot see
 * any other way.
 */

import { loudnessOfRange, preFilter } from "../dsp/loudness";
import {
  DEFAULT_BACKEND_PREFERENCE,
  resolveBackend,
  type DenoiseBackend,
} from "../models";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export interface DenoiseParams {
  /**
   * How far to push the noise floor down, in dB.
   *
   * Deliberately modest. Removing all of the noise is both impossible and
   * undesirable — a recording with a natural, quiet floor sounds like a
   * recording, while one scrubbed to digital silence between words sounds
   * broken, and the artefacts needed to get there are worse than the noise.
   */
  reductionDb: number;
  /**
   * Programme-to-floor distance, in dB, at which a recording counts as already
   * clean and the stage backs off to nothing.
   *
   * Denoising is not free: it modifies the signal, and on material that was
   * already quiet the modification is all you get. Measured on the eval corpus,
   * running 12 dB of reduction over a recording whose floor sits 35 dB down
   * *lowered* SI-SDR against the clean reference — the processing was the only
   * thing it changed. So the reduction is scaled by how much noise is actually
   * there: a recording at 20 dB gets the full amount, one at 35 dB gets none,
   * and in between it tapers.
   */
  cleanSnrDb: number;
  /** Backend order to try. First one that can actually run wins. */
  backends: string[];
}

export const DEFAULT_DENOISE_PARAMS: DenoiseParams = {
  reductionDb: 12,
  cleanSnrDb: 35,
  backends: DEFAULT_BACKEND_PREFERENCE,
};

export interface DenoiseStageReport {
  /** Which backend actually ran, or null when none could. */
  backend: string | null;
  /** Backends that were preferred but unavailable, with the reason. */
  skipped: { name: string; reason: string }[];
  /** Backend-specific detail. */
  info: Record<string, unknown>;
  reductionRequestedDb: number;
  /** What was actually asked of the backend after the clean-source taper. */
  reductionAppliedDb: number;
  /** Measured programme-to-floor distance on the way in, in dB. */
  inputSnrDb: number;
  /** What the noise floor in the pauses actually did, in dB (positive = quieter). */
  reductionAchievedDb: number;
  noiseFloorBeforeLufs: number;
  noiseFloorAfterLufs: number;
  /** True when no backend could run and the audio was passed through. */
  skippedEntirely: boolean;
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
    totalPower += Math.pow(10, loudness / 10) * length;
    totalSamples += length;
  }

  return totalSamples > 0 ? 10 * Math.log10(totalPower / totalSamples) : -Infinity;
}

/**
 * Scale the requested reduction by how much noise is actually present. Full
 * strength on a noisy source, nothing on a clean one, tapering between.
 */
function adaptReduction(requestedDb: number, snrDb: number, cleanSnrDb: number): number {
  if (!Number.isFinite(snrDb)) return requestedDb;
  const headroom = Math.max(0, cleanSnrDb - snrDb);
  return Math.max(0, Math.min(requestedDb, headroom));
}

export const denoiseStage: Stage<DenoiseParams, DenoiseStageReport> = {
  name: "denoise",
  description: "Attenuate steady background noise (spectral suppression, or a model when present)",
  defaults: DEFAULT_DENOISE_PARAMS,

  render(signal, params, ctx): StageOutput<DenoiseStageReport> {
    const pauses = ctx.analysis.silence().regions.map((r) => ({ start: r.start, end: r.end }));
    const { backend, skipped } = resolveBackend(params.backends, signal.sampleRate);

    const before = pauseLoudness(signal, pauses);
    const programme = ctx.analysis.integratedLufs();
    const inputSnrDb = Number.isFinite(programme) && Number.isFinite(before) ? programme - before : NaN;
    const applied = adaptReduction(params.reductionDb, inputSnrDb, params.cleanSnrDb);

    const base: DenoiseStageReport = {
      backend: null,
      skipped,
      info: {},
      reductionRequestedDb: params.reductionDb,
      reductionAppliedDb: applied,
      inputSnrDb,
      reductionAchievedDb: 0,
      noiseFloorBeforeLufs: before,
      noiseFloorAfterLufs: before,
      skippedEntirely: true,
    };

    // Nothing worth removing: return the signal itself, so a clean recording
    // comes out of this stage bit-identical rather than merely similar.
    if (!backend || applied <= 0) return { signal, report: base };

    ctx.progress(0.1);
    const response = (backend as DenoiseBackend).process({
      channels: signal.channels,
      sampleRate: signal.sampleRate,
      pauses,
      reductionDb: applied,
    });
    ctx.progress(0.9);

    const output: Signal = {
      sampleRate: signal.sampleRate,
      channels: response.channels,
      length: signal.length,
    };
    const after = pauseLoudness(output, pauses);

    ctx.progress(1);
    return {
      signal: output,
      report: {
        ...base,
        backend: backend.name,
        info: response.info,
        reductionAchievedDb: Number.isFinite(before) && Number.isFinite(after) ? before - after : 0,
        noiseFloorAfterLufs: after,
        skippedEntirely: false,
      },
    };
  },
};
