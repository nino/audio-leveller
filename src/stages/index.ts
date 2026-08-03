/**
 * Built-in stages, registered in chain order.
 *
 * `DEFAULT_CHAIN` is the order stages run in when a caller doesn't specify
 * one. As the pipeline grows this list encodes the ordering decisions that
 * matter: de-click before the denoiser (impulses are out-of-distribution for
 * the model and get smeared rather than removed), EQ after it (denoising
 * changes the spectrum you'd otherwise be fitting a curve to), and levelling
 * last so the loudness target is exact.
 */

import { registerStage } from "../pipeline/registry";
import type { Stage, StageSpec } from "../pipeline/types";
import { declickStage } from "./declick";
import { denoiseStage } from "./denoise";
import { dereverbStage } from "./dereverb";
import { dyneqStage } from "./dyneq";
import { eqStage } from "./eq";
import { levelStage } from "./level";

const BUILT_IN: Stage<object, unknown>[] = [
  declickStage as Stage<object, unknown>,
  denoiseStage as Stage<object, unknown>,
  dereverbStage as Stage<object, unknown>,
  eqStage as Stage<object, unknown>,
  dyneqStage as Stage<object, unknown>,
  levelStage as Stage<object, unknown>,
];

for (const stage of BUILT_IN) registerStage(stage);

/** Default chain, in order. */
export const DEFAULT_CHAIN: string[] = BUILT_IN.map((s) => s.name);

export interface ChainOptions {
  /** Restrict the chain to these stages (in default order). */
  only?: string[];
  /** Run the chain but bypass these stages. They still appear in the report. */
  bypass?: string[];
  /** Parameter overrides, keyed by stage name. */
  params?: Record<string, Record<string, unknown>>;
}

/**
 * Build a chain spec from the default order. Unknown stage names are rejected
 * here rather than silently ignored, so a typo in `--bypass` is an error
 * instead of a stage that quietly stayed on.
 */
export function buildChain(options: ChainOptions = {}): StageSpec[] {
  const { only, bypass = [], params = {} } = options;

  for (const name of [...(only ?? []), ...bypass, ...Object.keys(params)]) {
    if (!DEFAULT_CHAIN.includes(name)) {
      throw new Error(`Unknown stage "${name}" (available: ${DEFAULT_CHAIN.join(", ")})`);
    }
  }

  const selected = only ? DEFAULT_CHAIN.filter((n) => only.includes(n)) : DEFAULT_CHAIN;
  return selected.map((name) => ({
    name,
    enabled: !bypass.includes(name),
    params: params[name],
  }));
}

export { declickStage, denoiseStage, dereverbStage, dyneqStage, eqStage, levelStage };
export type { DeclickParams } from "./declick";
export type { DenoiseParams } from "./denoise";
export type { DereverbParams } from "./dereverb";
export type { DynEqParams } from "./dyneq";
export type { EqParams } from "./eq";
export type { LevelParams } from "./level";
