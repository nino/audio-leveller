/**
 * Core pipeline vocabulary.
 *
 * A {@link Signal} is bare audio – the currency every stage deals in. The
 * bit depth and sample format of the file on disk live in the pipeline
 * result instead, so stages never have to think about encoding.
 */

import type { Analyzer } from "./analysis";

export interface Signal {
  sampleRate: number;
  /** One Float32Array per channel, each of length `length`. */
  channels: Float32Array[];
  /** Number of sample frames (per channel). */
  length: number;
}

/** A loudness snapshot of a signal, reported before and after the chain. */
export interface Measurement {
  integratedLufs: number;
  peakDbfs: number;
}

export interface StageContext {
  /**
   * Measurements of the signal *as it enters this stage*. Stages must use
   * this rather than measuring the original file: an earlier stage may have
   * changed the spectrum or the noise floor out from under them.
   */
  analysis: Analyzer;
  /**
   * The untouched decoded signal (at the pipeline's working rate), for the
   * stages that genuinely need original audio – room-tone harvesting has to
   * happen before a denoiser deletes the room tone.
   */
  source: Signal;
  /** Measurements of {@link source}. */
  sourceAnalysis: Analyzer;
  /** Report progress within this stage, as a fraction in [0, 1]. */
  progress(fraction: number): void;
}

export interface StageOutput<R = unknown> {
  signal: Signal;
  /** Whatever the stage decided and did, JSON-serialisable. */
  report: R;
  /**
   * Additional signals the stage produced alongside the main output, keyed by
   * a short name (the leveller emits `roomtone`). Written out as sibling
   * files. Keys must be unique across the whole chain.
   */
  extras?: Record<string, Signal>;
}

/**
 * A processing stage. Stages are pure: same signal plus same params gives the
 * same output, which is what makes per-stage caching and golden-file tests
 * possible later.
 */
export interface Stage<P extends object = object, R = unknown> {
  readonly name: string;
  /** One-line description, shown by `--list-stages`. */
  readonly description: string;
  /** Parameter defaults. Also defines the parameter shape for callers. */
  readonly defaults: P;
  /**
   * Sample rate this stage must run at. When any enabled stage sets this, the
   * pipeline converts once on the way in and once on the way out. Stages that
   * work at any rate (the leveller derives its filter coefficients
   * analytically) leave it undefined and cost nothing.
   */
  readonly requiredSampleRate?: number;
  render(signal: Signal, params: P, ctx: StageContext): StageOutput<R>;
}

/** A stage as requested by a caller: which one, on or off, with what params. */
export interface StageSpec {
  name: string;
  /** Defaults to true. A disabled stage still appears in the report. */
  enabled?: boolean;
  params?: Record<string, unknown>;
}

export interface StageReport {
  name: string;
  enabled: boolean;
  /** Fully resolved params (defaults merged with overrides). */
  params: Record<string, unknown>;
  elapsedMs: number;
  /** Stage-specific detail; null when the stage was bypassed. */
  report: unknown;
}

export interface PipelineReport {
  /** Rate of the file on disk. */
  sourceSampleRate: number;
  /** Rate the chain actually ran at (differs only when a stage demands it). */
  workingSampleRate: number;
  channels: number;
  durationSec: number;
  /** Whether sample-rate conversion was performed. */
  resampled: boolean;
  input: Measurement;
  output: Measurement;
  stages: StageReport[];
  /** Names of the extra signals produced (e.g. `roomtone`). */
  extras: string[];
}

export interface PipelineProgress {
  /** Name of the stage currently running. */
  stage: string;
  /** Zero-based index of that stage among the enabled ones. */
  index: number;
  /** How many stages will run. */
  total: number;
  /** Overall completion in [0, 1]. */
  overall: number;
}

export interface PipelineResult {
  signal: Signal;
  extras: Record<string, Signal>;
  report: PipelineReport;
}
