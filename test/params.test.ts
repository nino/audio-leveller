/**
 * The UI's parameter schema is written by hand; the stages own the actual
 * parameters. These tests are the seam between the two: every exposed key has
 * to exist on its stage, every default has to sit inside the range the slider
 * offers, and every preset override has to be a parameter that exists.
 */

import { describe, it, expect } from "vitest";
import {
  PRESETS,
  DEFAULT_PRESET,
  presetByName,
  presetSettings,
  stageParamDefaults,
  stageParamGroups,
  pipelineSchema,
} from "../src/params";
import { DEFAULT_CHAIN } from "../src/stages";
import { getStage } from "../src/pipeline/registry";
import { buildChain } from "../src/stages";

describe("parameter schema", () => {
  it("only exposes parameters the stages actually have", () => {
    for (const group of stageParamGroups()) {
      expect(DEFAULT_CHAIN).toContain(group.stage);
      const defaults = getStage(group.stage).defaults as Record<string, unknown>;
      for (const param of group.params) {
        expect(Object.keys(defaults)).toContain(param.key);
      }
    }
  });

  it("gives every numeric parameter a range that contains its default", () => {
    const defaults = stageParamDefaults();
    for (const group of stageParamGroups()) {
      for (const param of group.params) {
        if (param.kind !== "number") continue;
        const value = defaults[group.stage][param.key] as number;
        expect(typeof value).toBe("number");
        expect(value).toBeGreaterThanOrEqual(param.min);
        expect(value).toBeLessThanOrEqual(param.max);
      }
    }
  });

  it("offers every choice parameter's default among its options", () => {
    const defaults = stageParamDefaults();
    for (const group of stageParamGroups()) {
      for (const param of group.params) {
        if (param.kind !== "choice") continue;
        const value = defaults[group.stage][param.key];
        expect(param.options.map((o) => o.value)).toContain(value);
      }
    }
  });

  it("describes the whole chain to the UI", () => {
    const schema = pipelineSchema();
    expect(schema.chain).toEqual(DEFAULT_CHAIN);
    expect(schema.presets.map((p) => p.name)).toContain(schema.defaultPreset);
  });
});

describe("presets", () => {
  it("makes Nino the default, and the defaults", () => {
    expect(DEFAULT_PRESET).toBe("Nino");
    expect(presetByName("Nino").params).toEqual({});
    expect(presetByName("Nino").bypass).toEqual([]);
    expect(presetSettings("Nino")).toEqual(stageParamDefaults());
  });

  it("resolves by name, case-insensitively, and rejects unknown names", () => {
    expect(presetByName("acx").name).toBe("ACX");
    expect(() => presetByName("mastering")).toThrow(/Unknown preset/);
  });

  it("only overrides parameters that exist on their stage", () => {
    for (const preset of PRESETS) {
      for (const [stage, overrides] of Object.entries(preset.params)) {
        const defaults = getStage(stage).defaults as Record<string, unknown>;
        for (const key of Object.keys(overrides)) {
          expect(Object.keys(defaults)).toContain(key);
        }
      }
      // A preset's bypass list has to name real stages, or the chain throws.
      expect(() => buildChain({ bypass: preset.bypass, params: preset.params })).not.toThrow();
    }
  });

  it("aims ACX at Audible's window: -20 LUFS, peaks under -3 dBFS", () => {
    const acx = presetSettings("ACX");
    expect(acx.level.targetLufs).toBe(-20);
    expect(acx.level.ceilingDb as number).toBeLessThanOrEqual(-3);
    // Tone is a taste, and a submission is not the place for it.
    expect(acx.eq.voicing).toBe("neutral");
    // Everything the preset does not mention still comes from the stage.
    expect(acx.declick.thresholdSigma).toBe(stageParamDefaults().declick.thresholdSigma);
  });
});
