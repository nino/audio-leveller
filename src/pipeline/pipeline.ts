/**
 * The pipeline runner: takes a signal and an ordered list of stages, and
 * hands each stage the audio produced by the one before it.
 *
 * Two things it does that are easy to get wrong:
 *
 * 1. **Sample-rate conversion is lazy.** Rather than forcing everything to a
 *    canonical 48 kHz, conversion happens only when an enabled stage actually
 *    demands a rate (the AI stages will). A chain of rate-agnostic stages on a
 *    44.1 kHz file therefore does no conversion at all, and a fully bypassed
 *    chain is bit-identical to its input.
 *
 * 2. **Analysis follows the audio.** Each stage gets an Analyzer over the
 *    signal as it arrives, not over the original file, because a stage that
 *    changes the spectrum invalidates earlier measurements. Stages that truly
 *    need the untouched original get it separately via `ctx.source`.
 */

import { resample } from "../dsp/resample";
import { Analyzer } from "./analysis";
import { getStage } from "./registry";
import type {
  PipelineProgress,
  PipelineReport,
  PipelineResult,
  Signal,
  StageContext,
  StageReport,
  StageSpec,
} from "./types";

export interface PipelineOptions {
  /** The chain, in order. Stages may be listed disabled to record a bypass. */
  stages: StageSpec[];
  onProgress?: (progress: PipelineProgress) => void;
}

function resampleSignal(signal: Signal, toRate: number): Signal {
  if (signal.sampleRate === toRate) return signal;
  const channels = resample(signal.channels, signal.sampleRate, toRate);
  return { sampleRate: toRate, channels, length: channels[0]?.length ?? 0 };
}

/**
 * Decide what rate the chain runs at. Undefined means "whatever the source
 * is". Mixed requirements are rejected rather than silently resampled between
 * every stage – no stage needs that yet, and guessing would hide the cost.
 */
function workingRate(specs: StageSpec[], sourceRate: number): number {
  const required = new Set<number>();
  for (const spec of specs) {
    if (spec.enabled === false) continue;
    const rate = getStage(spec.name).requiredSampleRate;
    if (rate !== undefined) required.add(rate);
  }
  if (required.size === 0) return sourceRate;
  if (required.size > 1) {
    throw new Error(
      `Stages disagree on sample rate (${[...required].join(", ")}); ` +
        "mid-chain conversion is not supported",
    );
  }
  return [...required][0];
}

export function runPipeline(input: Signal, options: PipelineOptions): PipelineResult {
  const { stages: specs, onProgress } = options;
  const sourceRate = input.sampleRate;
  const rate = workingRate(specs, sourceRate);

  // Convert once on the way in; everything downstream runs at `rate`.
  const source = resampleSignal(input, rate);
  const sourceAnalysis = new Analyzer(source);

  const enabledCount = specs.filter((s) => s.enabled !== false).length;
  let completed = 0;

  let signal = source;
  const extras: Record<string, Signal> = {};
  const stageReports: StageReport[] = [];

  for (const spec of specs) {
    const stage = getStage(spec.name);
    const params = { ...stage.defaults, ...(spec.params ?? {}) };

    if (spec.enabled === false) {
      stageReports.push({
        name: stage.name,
        enabled: false,
        params,
        elapsedMs: 0,
        report: null,
      });
      continue;
    }

    const index = completed;
    const emit = (fraction: number): void => {
      onProgress?.({
        stage: stage.name,
        index,
        total: enabledCount,
        overall: enabledCount === 0 ? 1 : (index + Math.min(1, Math.max(0, fraction))) / enabledCount,
      });
    };
    emit(0);

    const ctx: StageContext = {
      analysis: new Analyzer(signal),
      source,
      sourceAnalysis,
      progress: emit,
    };

    const started = performance.now();
    const output = stage.render(signal, params, ctx);
    const elapsedMs = performance.now() - started;

    for (const [key, extra] of Object.entries(output.extras ?? {})) {
      if (key in extras) {
        throw new Error(`Stage "${stage.name}" produced a duplicate extra output "${key}"`);
      }
      extras[key] = extra;
    }

    signal = output.signal;
    stageReports.push({
      name: stage.name,
      enabled: true,
      params,
      elapsedMs,
      report: output.report,
    });

    completed++;
    emit(1);
  }

  // Back to the file's own rate, so a 44.1 kHz input stays a 44.1 kHz output.
  const outputSignal = resampleSignal(signal, sourceRate);
  const outputExtras: Record<string, Signal> = {};
  for (const [key, extra] of Object.entries(extras)) {
    outputExtras[key] = resampleSignal(extra, sourceRate);
  }

  const report: PipelineReport = {
    sourceSampleRate: sourceRate,
    workingSampleRate: rate,
    channels: input.channels.length,
    durationSec: sourceRate > 0 ? input.length / sourceRate : 0,
    resampled: rate !== sourceRate,
    input: sourceAnalysis.measure(),
    // A fully bypassed chain hands back the same signal – don't measure twice.
    output: (outputSignal === source ? sourceAnalysis : new Analyzer(outputSignal)).measure(),
    stages: stageReports,
    extras: Object.keys(outputExtras),
  };

  return { signal: outputSignal, extras: outputExtras, report };
}
