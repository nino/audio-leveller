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

import { integratedLoudness, loudnessOfRange, preFilter } from "../dsp/loudness";
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
   * clean and the stage backs off to nothing. A backend may state its own
   * threshold ({@link DenoiseBackend.cleanSnrDb}), and does when it is more
   * transparent than this default assumes.
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
  /**
   * How much gated programme loudness a backend may cost before its output is
   * thrown away and the input returned instead, in dB.
   *
   * A denoiser is supposed to remove what is *around* the speech. Losing a
   * little programme loudness is legitimate — on a genuinely noisy source the
   * noise was contributing to the measurement — but losing a lot means the
   * backend has decided the speech is the noise.
   *
   * This is not hypothetical. DeepFilterNet3 does exactly that to this
   * project's synthetic corpus, which is speech-*shaped* rather than speech:
   * it reads a harmonic stack with formants as noise and pulls the programme
   * down by 10 dB, right up against the limit the requested reduction sets.
   * On real recordings the same model moves programme loudness by under
   * 0.7 dB. So the guard costs nothing on material a model understands and
   * turns a wrecked render into a declined stage on material it does not —
   * which matters more for a trained backend than a classical one, because a
   * trained one fails by confidently rewriting rather than by doing too little.
   */
  maxProgrammeLossDb: number;
  /** Backend order to try. First one that can actually run wins. */
  backends: string[];
}

export const DEFAULT_DENOISE_PARAMS: DenoiseParams = {
  reductionDb: 12,
  cleanSnrDb: 35,
  // Above what noise removal alone can explain: at 6 dB SNR the noise is a
  // fifth of the total power, so removing all of it costs about 1 dB of
  // measured loudness, and 3 dB leaves room for worse sources than that.
  maxProgrammeLossDb: 3,
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
  /** Gated programme loudness the backend cost, in dB (positive = quieter). */
  programmeLossDb: number;
  /**
   * Set when a backend ran and its output was rejected, saying why. The audio
   * returned is then the input, unchanged.
   */
  rejected: string | null;
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
    // A backend that states its own threshold knows better than the stage does
    // how transparent it is on material that barely needs treating.
    const cleanSnrDb = backend?.cleanSnrDb ?? params.cleanSnrDb;
    const applied = adaptReduction(params.reductionDb, inputSnrDb, cleanSnrDb);

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
      programmeLossDb: 0,
      rejected: null,
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

    // What the backend cost the programme itself, measured the same way the
    // leveller measures loudness: gated, so pauses do not drag it down.
    const outputProgramme = integratedLoudness(
      preFilter(output.channels, output.sampleRate),
      output.sampleRate,
    );
    const programmeLossDb =
      Number.isFinite(programme) && Number.isFinite(outputProgramme)
        ? programme - outputProgramme
        : 0;

    const common = {
      ...base,
      backend: backend.name,
      info: response.info,
      programmeLossDb,
      skippedEntirely: false,
    };

    if (programmeLossDb > params.maxProgrammeLossDb) {
      ctx.progress(1);
      return {
        signal,
        report: {
          ...common,
          rejected:
            `the ${backend.name} backend cost ${programmeLossDb.toFixed(1)} dB of programme ` +
            `loudness (limit ${params.maxProgrammeLossDb} dB) — it is removing the speech, ` +
            `not the noise, so its output was discarded`,
        },
      };
    }

    ctx.progress(1);
    return {
      signal: output,
      report: {
        ...common,
        reductionAchievedDb: Number.isFinite(before) && Number.isFinite(after) ? before - after : 0,
        noiseFloorAfterLufs: after,
      },
    };
  },
};
