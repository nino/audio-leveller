import { describe, it, expect } from "vitest";
import {
  applyDynamics,
  compressorCurve,
  expanderCurve,
  DEFAULT_TIMING,
} from "../src/dsp/dynamics";
import { integratedLoudness, loudnessRange, preFilter } from "../src/dsp/loudness";
import { compressStage, DEFAULT_COMPRESS_PARAMS } from "../src/stages/compress";
import { expandStage, DEFAULT_EXPAND_PARAMS } from "../src/stages/expand";
import { Analyzer } from "../src/pipeline/analysis";
import { syntheticSpeech, rng } from "../src/eval/signals";
import type { Signal } from "../src/pipeline/types";

const sr = 48000;

function speech(floorDbfs: number, levels: number[]): ReturnType<typeof syntheticSpeech> {
  return syntheticSpeech({
    sampleRate: sr,
    segments: levels.map((levelLufs, i) => ({ seconds: 3, levelLufs, f0: 110 + i * 20 })),
    pauseSec: 1.4,
    floorDbfs,
    seed: 4242,
  });
}

function render<P extends object, R>(
  stage: { render: (s: Signal, p: P, c: never) => { signal: Signal; report: R } },
  signal: Signal,
  params: P,
): { signal: Signal; report: R } {
  const analysis = new Analyzer(signal);
  return stage.render(signal, params, {
    analysis,
    source: signal,
    sourceAnalysis: analysis,
    progress: () => {},
  } as never);
}

describe("compressor curve", () => {
  const curve = compressorCurve({ thresholdDb: -20, ratio: 2, kneeDb: 0, maxReductionDb: 30 });

  it("leaves everything below the threshold alone", () => {
    expect(curve(-40)).toBeCloseTo(0, 12);
    expect(curve(-20)).toBeCloseTo(0, 12);
  });

  it("applies the ratio above the threshold", () => {
    // 2:1 at 10 dB over means 5 dB of reduction.
    expect(curve(-10)).toBeCloseTo(-5, 9);
    expect(curve(0)).toBeCloseTo(-10, 9);
  });

  it("never exceeds the reduction cap", () => {
    const capped = compressorCurve({ thresholdDb: -60, ratio: 8, kneeDb: 0, maxReductionDb: 6 });
    expect(capped(0)).toBeCloseTo(-6, 9);
  });

  it("bends smoothly through a soft knee", () => {
    // Speech sits near the threshold most of the time, so a kink there is
    // audible as the gain flicking on and off. Check the curve and its slope
    // are continuous rather than merely finite.
    const soft = compressorCurve({ thresholdDb: -20, ratio: 4, kneeDb: 10, maxReductionDb: 40 });
    const step = 0.05;
    let previousSlope = 0;
    for (let level = -35; level <= -5; level += step) {
      const slope = (soft(level + step) - soft(level)) / step;
      expect(Math.abs(slope - previousSlope)).toBeLessThan(0.02);
      previousSlope = slope;
    }
    // Ends still agree with the hard-knee behaviour away from the knee.
    expect(soft(-30)).toBeCloseTo(0, 6);
    expect(soft(-5)).toBeCloseTo(-(15 - 15 / 4), 6);
  });
});

describe("expander curve", () => {
  const curve = expanderCurve({ thresholdDb: -40, ratio: 3, kneeDb: 0, rangeDb: 20 });

  it("leaves everything above the threshold alone", () => {
    expect(curve(-20)).toBeCloseTo(0, 12);
    expect(curve(-40)).toBeCloseTo(0, 12);
  });

  it("applies the ratio below the threshold", () => {
    // ratio 3 means 2 dB extra attenuation per dB under.
    expect(curve(-45)).toBeCloseTo(-10, 9);
  });

  it("stops at the range, which is what makes it not a gate", () => {
    // Digital silence between words sounds broken in a way the noise does not;
    // the room has to stay present, just further down.
    expect(curve(-90)).toBeCloseTo(-20, 9);
    expect(curve(-200)).toBeCloseTo(-20, 9);
  });
});

describe("applying dynamics", () => {
  function tone(seconds: number, amp: number): Float32Array {
    const n = Math.round(seconds * sr);
    const out = new Float32Array(n);
    for (let i = 0; i < n; i++) out[i] = amp * Math.sin((2 * Math.PI * 300 * i) / sr);
    return out;
  }

  it("is a no-op when the curve asks for no gain", () => {
    const input = tone(0.5, 0.4);
    const result = applyDynamics([input], sr, () => 0);
    for (let i = 0; i < input.length; i += 37) {
      expect(result.channels[0][i]).toBeCloseTo(input[i], 6);
    }
    expect(result.maxReductionDb).toBeCloseTo(0, 9);
  });

  it("applies one gain to every channel, so the stereo image holds still", () => {
    // Detecting and applying per channel would let a loud left duck while the
    // right stayed put, which walks the image around in time with the speech.
    const left = tone(1, 0.5);
    const right = tone(1, 0.05);
    const result = applyDynamics(
      [left, right],
      sr,
      compressorCurve({ thresholdDb: -30, ratio: 4, kneeDb: 0, maxReductionDb: 30 }),
    );
    for (let i = sr / 2; i < sr; i += 53) {
      if (Math.abs(right[i]) < 1e-4) continue;
      expect(result.channels[0][i] / result.channels[1][i]).toBeCloseTo(left[i] / right[i], 4);
    }
  });

  it("reaches most of the way to the target within one time constant", () => {
    // A step from quiet to loud, with a 50 ms fall constant: after 50 ms the
    // gain should have covered about 63% of the distance.
    const quiet = tone(0.5, 0.01);
    const loud = tone(0.5, 0.5);
    const input = new Float32Array(quiet.length + loud.length);
    input.set(quiet, 0);
    input.set(loud, quiet.length);

    const curve = compressorCurve({ thresholdDb: -30, ratio: 4, kneeDb: 0, maxReductionDb: 40 });
    const result = applyDynamics([input], sr, curve, { downMs: 50, upMs: 400, detectorMs: 5 });

    const settled = result.gainDb[result.gainDb.length - 1];
    const atTau = result.gainDb[quiet.length + Math.round(0.05 * sr)];
    expect(settled).toBeLessThan(-3);
    expect(atTau / settled).toBeGreaterThan(0.5);
    expect(atTau / settled).toBeLessThan(0.85);
  });

  it("only ever attenuates, for both curves", () => {
    const input = tone(1, 0.3);
    for (const curve of [
      compressorCurve({ thresholdDb: -40, ratio: 3, kneeDb: 6, maxReductionDb: 20 }),
      expanderCurve({ thresholdDb: -10, ratio: 3, kneeDb: 6, rangeDb: 20 }),
    ]) {
      const result = applyDynamics([input], sr, curve);
      for (const g of result.gainDb) expect(g).toBeLessThanOrEqual(1e-6);
    }
  });

  it("is deterministic", () => {
    const input = tone(0.5, 0.3);
    const curve = compressorCurve({ thresholdDb: -30, ratio: 2, kneeDb: 6, maxReductionDb: 20 });
    const a = applyDynamics([input], sr, curve, DEFAULT_TIMING);
    const b = applyDynamics([input], sr, curve, DEFAULT_TIMING);
    expect(Array.from(a.channels[0])).toEqual(Array.from(b.channels[0]));
  });
});

describe("loudness range", () => {
  it("is near zero for a programme that never changes level", () => {
    const built = speech(-70, [-23, -23, -23]);
    const value = loudnessRange(preFilter(built.signal.channels, sr), sr);
    expect(value).toBeLessThan(3);
  });

  it("grows with the spread between passages", () => {
    const even = speech(-70, [-23, -23, -23]);
    const uneven = speech(-70, [-35, -18, -30]);
    const a = loudnessRange(preFilter(even.signal.channels, sr), sr);
    const b = loudnessRange(preFilter(uneven.signal.channels, sr), sr);
    expect(b).toBeGreaterThan(a + 3);
  });
});

describe("compress stage", () => {
  it("declines on material that is already even", () => {
    const built = speech(-70, [-23, -23, -23]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { signal: out, report } = render(compressStage, signal, DEFAULT_COMPRESS_PARAMS);

    expect(report.skipped).toBe(true);
    // Declining means returning the input itself, not a copy of it.
    expect(out.channels[0]).toBe(signal.channels[0]);
  });

  it("narrows the loudness range of uneven material", () => {
    const built = speech(-70, [-35, -18, -30]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { report } = render(compressStage, signal, DEFAULT_COMPRESS_PARAMS);

    expect(report.skipped).toBe(false);
    expect(report.loudnessRangeAfterLu).toBeLessThan(report.loudnessRangeBeforeLu);
    expect(report.maxReductionDb).toBeLessThanOrEqual(DEFAULT_COMPRESS_PARAMS.maxReductionDb + 1e-6);
  });

  it("does not add make-up gain — the leveller does that", () => {
    // A compressor that quietly restored the level would hide how much it took.
    const built = speech(-70, [-35, -18, -30]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { signal: out } = render(compressStage, signal, DEFAULT_COMPRESS_PARAMS);

    const before = integratedLoudness(preFilter(signal.channels, sr), sr);
    const after = integratedLoudness(preFilter(out.channels, sr), sr);
    expect(after).toBeLessThan(before);
  });
});

describe("expand stage", () => {
  it("declines when the floor is already deep", () => {
    const built = speech(-95, [-30, -18, -25]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { signal: out, report } = render(expandStage, signal, DEFAULT_EXPAND_PARAMS);

    expect(report.skipped).toBe(true);
    expect(out.channels[0]).toBe(signal.channels[0]);
  });

  it("pushes down an audible floor without exceeding its range", () => {
    const built = speech(-52, [-30, -18, -25]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { report } = render(expandStage, signal, DEFAULT_EXPAND_PARAMS);

    expect(report.skipped).toBe(false);
    expect(report.floorReductionDb).toBeGreaterThan(2);
    expect(report.maxReductionDb).toBeLessThanOrEqual(DEFAULT_EXPAND_PARAMS.rangeDb + 1e-6);
  });

  it("leaves the speech itself where it was", () => {
    // The failure that matters is chewing the quiet ends of words. Programme
    // loudness is gated, so it reads the speech and not the gaps.
    const built = speech(-52, [-30, -18, -25]);
    const signal: Signal = { sampleRate: sr, channels: built.signal.channels, length: built.signal.length };
    const { signal: out } = render(expandStage, signal, DEFAULT_EXPAND_PARAMS);

    const before = integratedLoudness(preFilter(signal.channels, sr), sr);
    const after = integratedLoudness(preFilter(out.channels, sr), sr);
    expect(Math.abs(after - before)).toBeLessThan(1);
  });
});

describe("noise floors are deterministic across levels", () => {
  it("changes only the floor, so a quieter build is a clean reference", () => {
    // `floor-expand` in the eval corpus relies on this: same seed, different
    // floor, therefore sample-identical speech.
    const loud = speech(-52, [-30, -18, -25]).signal.channels[0];
    const quiet = speech(-100, [-30, -18, -25]).signal.channels[0];
    let worst = 0;
    let peak = 0;
    for (let i = 0; i < loud.length; i++) {
      worst = Math.max(worst, Math.abs(loud[i] - quiet[i]));
      peak = Math.max(peak, Math.abs(quiet[i]));
    }
    // The difference is the floor and nothing else.
    expect(20 * Math.log10(worst / peak)).toBeLessThan(-40);
    expect(rng).toBeTypeOf("function");
  });
});
