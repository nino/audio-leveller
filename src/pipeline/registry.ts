/**
 * Stage registry. Stages register themselves by name so a chain can be
 * described as data – a list of names and params from a CLI flag, a config
 * file or the UI – rather than as imports wired up in code.
 */

import type { Stage } from "./types";

const stages = new Map<string, Stage<object, unknown>>();

export function registerStage(stage: Stage<object, unknown>): void {
  if (stages.has(stage.name)) {
    throw new Error(`Stage "${stage.name}" is already registered`);
  }
  stages.set(stage.name, stage);
}

export function getStage(name: string): Stage<object, unknown> {
  const stage = stages.get(name);
  if (!stage) {
    const known = [...stages.keys()].join(", ") || "none";
    throw new Error(`Unknown stage "${name}" (registered: ${known})`);
  }
  return stage;
}

export function hasStage(name: string): boolean {
  return stages.has(name);
}

export function registeredStages(): Stage<object, unknown>[] {
  return [...stages.values()];
}
