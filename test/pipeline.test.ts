import { describe, it, expect } from "vitest";
import { runPipeline } from "../src/pipeline/pipeline";
import { registerStage } from "../src/pipeline/registry";
import type { PipelineProgress, Signal, Stage } from "../src/pipeline/types";
import { levelAudio } from "../src/dsp/leveller";
import { buildChain, DEFAULT_CHAIN } from "../src/stages";
import { sine, silence, concat, mono } from "./helpers";

const sr = 48000;

const asStage = (stage: unknown): Stage<object, unknown> => stage as Stage<object, unknown>;

/** Multiplies by a constant, so ordering and params are observable. */
const gainStage: Stage<{ gain: number }, { applied: number }> = {
  name: "test:gain",
  description: "Test stage: constant gain",
  defaults: { gain: 1 },
  render(signal, params) {
    const channels = signal.channels.map((ch) => {
      const out = new Float32Array(ch.length);
      for (let i = 0; i < ch.length; i++) out[i] = ch[i] * params.gain;
      return out;
    });
    return { signal: { ...signal, channels }, report: { applied: params.gain } };
  },
};

/** Records the order stages ran in. */
const order: string[] = [];
const markerStage: Stage<{ id: string }, { id: string }> = {
  name: "test:marker",
  description: "Test stage: records execution order",
  defaults: { id: "?" },
  render(signal, params, ctx) {
    order.push(params.id);
    ctx.progress(0.5);
    return { signal, report: { id: params.id } };
  },
};

/** Emits a named extra output alongside the main signal. */
const extraStage: Stage<object, null> = {
  name: "test:extra",
  description: "Test stage: emits an extra output",
  defaults: {},
  render: (signal) => ({
    signal,
    report: null,
    extras: {
      marker: { sampleRate: signal.sampleRate, channels: [new Float32Array(4)], length: 4 },
    },
  }),
};

/** Demands 48 kHz, so the pipeline has to convert around it. */
const fixedRateStage: Stage<object, { sawRate: number }> = {
  name: "test:fixed48",
  description: "Test stage: requires 48 kHz",
  defaults: {},
  requiredSampleRate: 48000,
  render(signal) {
    return { signal, report: { sawRate: signal.sampleRate } };
  },
};

const otherRateStage: Stage<object, null> = {
  name: "test:fixed44",
  description: "Test stage: requires 44.1 kHz",
  defaults: {},
  requiredSampleRate: 44100,
  render: (signal) => ({ signal, report: null }),
};

for (const stage of [gainStage, markerStage, extraStage, fixedRateStage, otherRateStage]) {
  registerStage(asStage(stage));
}

function testSignal(sampleRate = sr): Signal {
  const samples = concat(sine(300, 1, sampleRate, 0.4), silence(0.2, sampleRate));
  return { sampleRate, channels: [samples], length: samples.length };
}

describe("pipeline", () => {
  it("is bit-identical to its input when every stage is bypassed", () => {
    const input = testSignal();
    const { signal, report } = runPipeline(input, {
      stages: [
        { name: "test:gain", enabled: false, params: { gain: 4 } },
        { name: "level", enabled: false },
      ],
    });

    expect(Array.from(signal.channels[0])).toEqual(Array.from(input.channels[0]));
    expect(report.resampled).toBe(false);
    // Bypassed stages still appear, so the report shows the whole chain.
    expect(report.stages.map((s) => s.name)).toEqual(["test:gain", "level"]);
    expect(report.stages.every((s) => !s.enabled)).toBe(true);
    expect(report.stages[0].report).toBeNull();
  });

  it("runs stages in order, feeding each the previous stage's output", () => {
    order.length = 0;
    const input = testSignal();
    const { signal } = runPipeline(input, {
      stages: [
        { name: "test:marker", params: { id: "first" } },
        { name: "test:gain", params: { gain: 2 } },
        { name: "test:marker", params: { id: "second" } },
        { name: "test:gain", params: { gain: 3 } },
      ],
    });

    expect(order).toEqual(["first", "second"]);
    // Both gains applied, in sequence.
    expect(signal.channels[0][100]).toBeCloseTo(input.channels[0][100] * 6, 5);
  });

  it("records fully resolved params, merging defaults with overrides", () => {
    const { report } = runPipeline(testSignal(), {
      stages: [{ name: "test:marker", params: { id: "x" } }, { name: "test:gain" }],
    });
    expect(report.stages[0].params).toEqual({ id: "x" });
    expect(report.stages[1].params).toEqual({ gain: 1 }); // the default
  });

  it("collects extra outputs and rejects duplicate keys", () => {
    const { extras } = runPipeline(testSignal(), { stages: [{ name: "test:extra" }] });
    expect(Object.keys(extras)).toEqual(["marker"]);

    expect(() =>
      runPipeline(testSignal(), {
        stages: [{ name: "test:extra" }, { name: "test:extra" }],
      }),
    ).toThrow(/duplicate extra output/);
  });

  it("reports monotonic progress that ends at 1", () => {
    const seen: PipelineProgress[] = [];
    runPipeline(testSignal(), {
      stages: [
        { name: "test:marker", params: { id: "a" } },
        { name: "test:gain", enabled: false },
        { name: "test:gain", params: { gain: 2 } },
      ],
      onProgress: (p) => seen.push(p),
    });

    expect(seen.length).toBeGreaterThan(0);
    // Bypassed stages don't count toward the total.
    expect(seen.every((p) => p.total === 2)).toBe(true);
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i].overall).toBeGreaterThanOrEqual(seen[i - 1].overall);
    }
    expect(seen[seen.length - 1].overall).toBeCloseTo(1, 6);
  });

  it("converts to a stage's required rate and back again", () => {
    const input = testSignal(44100);
    const { signal, report } = runPipeline(input, { stages: [{ name: "test:fixed48" }] });

    expect((report.stages[0].report as { sawRate: number }).sawRate).toBe(48000);
    expect(report.workingSampleRate).toBe(48000);
    expect(report.resampled).toBe(true);
    // The output goes back to the file's own rate and length.
    expect(signal.sampleRate).toBe(44100);
    expect(signal.length).toBe(input.length);
  });

  it("does not convert when no enabled stage requires a rate", () => {
    const input = testSignal(44100);
    const { report } = runPipeline(input, {
      stages: [{ name: "test:fixed48", enabled: false }, { name: "test:gain" }],
    });
    expect(report.resampled).toBe(false);
    expect(report.workingSampleRate).toBe(44100);
  });

  it("refuses a chain whose stages demand different rates", () => {
    expect(() =>
      runPipeline(testSignal(), {
        stages: [{ name: "test:fixed48" }, { name: "test:fixed44" }],
      }),
    ).toThrow(/disagree on sample rate/);
  });

  it("measures the input and the output", () => {
    const input = testSignal();
    const { report } = runPipeline(input, { stages: [{ name: "test:gain", params: { gain: 0.5 } }] });
    // Halving the amplitude is -6 dB on both loudness and peak.
    expect(report.output.integratedLufs).toBeCloseTo(report.input.integratedLufs - 6.02, 1);
    expect(report.output.peakDbfs).toBeCloseTo(report.input.peakDbfs - 6.02, 1);
  });

  it("produces a JSON-serialisable report", () => {
    const { report } = runPipeline(testSignal(), { stages: buildChain() });
    expect(() => JSON.stringify(report)).not.toThrow();
    expect(JSON.parse(JSON.stringify(report)).stages[0].name).toBe("level");
  });
});

describe("level stage", () => {
  it("matches calling levelAudio directly", () => {
    const input = mono(
      concat(sine(220, 3, sr, 0.08), silence(1.5, sr), sine(440, 3, sr, 0.6)),
      sr,
    );
    const direct = levelAudio(input, {});
    const piped = runPipeline(
      { sampleRate: input.sampleRate, channels: input.channels, length: input.length },
      { stages: buildChain() },
    );

    expect(Array.from(piped.signal.channels[0])).toEqual(Array.from(direct.audio.channels[0]));
    expect(Array.from(piped.extras.roomtone.channels[0])).toEqual(
      Array.from(direct.roomTone.channels[0]),
    );
    expect(piped.report.stages[0].report).toEqual(direct.report);
  });

  it("emits no room-tone extra when there is no usable silence", () => {
    const input = testSignal();
    const { extras } = runPipeline(input, { stages: buildChain() });
    expect(extras.roomtone).toBeUndefined();
  });
});

describe("buildChain", () => {
  it("defaults to the full chain, all enabled", () => {
    expect(buildChain().map((s) => s.name)).toEqual(DEFAULT_CHAIN);
    expect(buildChain().every((s) => s.enabled)).toBe(true);
  });

  it("marks bypassed stages without removing them", () => {
    const chain = buildChain({ bypass: ["level"] });
    expect(chain.map((s) => s.name)).toEqual(DEFAULT_CHAIN);
    expect(chain.find((s) => s.name === "level")?.enabled).toBe(false);
  });

  it("restricts the chain with `only`, keeping default order", () => {
    expect(buildChain({ only: ["level"] }).map((s) => s.name)).toEqual(["level"]);
  });

  it("rejects unknown stage names instead of ignoring them", () => {
    expect(() => buildChain({ bypass: ["levl"] })).toThrow(/Unknown stage "levl"/);
    expect(() => buildChain({ only: ["denoise"] })).toThrow(/Unknown stage "denoise"/);
    expect(() => buildChain({ params: { nope: {} } })).toThrow(/Unknown stage "nope"/);
  });
});
