import { describe, it, expect } from "vitest";
import { dynamicEq, DEFAULT_DYNEQ_OPTIONS } from "../src/dsp/dyneq";
import { computeLtas, DEFAULT_LTAS_OPTIONS } from "../src/dsp/ltas";
import { applyCascade, peakingEq } from "../src/dsp/biquad";
import { syntheticSpeech, cloneSignal, rng } from "../src/eval/signals";
import { siSdrDb } from "../src/eval/metrics";
import { sine } from "./helpers";
import type { Signal } from "../src/pipeline/types";

const sr = 48000;

const cache = new Map<string, ReturnType<typeof syntheticSpeech>>();
function speech() {
  let built = cache.get("x");
  if (!built) {
    built = syntheticSpeech({
      sampleRate: sr,
      segments: [
        { seconds: 3, levelLufs: -23, f0: 110 },
        { seconds: 3, levelLufs: -23, f0: 130 },
      ],
      pauseSec: 1.2,
      floorDbfs: -75,
      seed: 4242,
    });
    cache.set("x", built);
  }
  return built;
}

/** Level at the grid point nearest `freq`, over the whole signal. */
function levelAt(samples: Float32Array, freq: number): number {
  const ltas = computeLtas(samples, sr, [{ start: 0, end: samples.length }], DEFAULT_LTAS_OPTIONS);
  if (!ltas) throw new Error("expected an LTAS");
  let best = 0;
  for (let i = 0; i < ltas.freqs.length; i++) {
    if (Math.abs(Math.log2(ltas.freqs[i] / freq)) < Math.abs(Math.log2(ltas.freqs[best] / freq))) {
      best = i;
    }
  }
  return ltas.db[best];
}

describe("dynamic eq", () => {
  it("attenuates a ringing tone added on top of speech", () => {
    const built = speech();
    const withTone = cloneSignal(built.signal);
    const tone = sine(2500, built.signal.length / sr, sr, 0.05);
    for (let i = 0; i < withTone.length; i++) withTone.channels[0][i] += tone[i];

    const result = dynamicEq(withTone.channels, sr);

    const before = levelAt(withTone.channels[0], 2500);
    const after = levelAt(result.channels[0], 2500);
    expect(before - after).toBeGreaterThan(3);
  });

  it("leaves the broad shape of the voice alone", () => {
    const built = speech();
    const result = dynamicEq(built.signal.channels, sr);

    // The formant regions are the voice, not resonances to shave.
    for (const freq of [500, 1500, 2600]) {
      const before = levelAt(built.signal.channels[0], freq);
      const after = levelAt(result.channels[0], freq);
      expect(Math.abs(before - after), `${freq} Hz`).toBeLessThan(2);
    }
  });

  it("de-esses: pulls down a sibilance burst harder than the same burst lower down", () => {
    // The sibilance band gets a lower threshold, so the same protrusion is
    // attenuated more there. This pins that weighting down.
    const build = (freq: number): Signal => {
      const built = cloneSignal(speech().signal);
      const tone = sine(freq, built.length / sr, sr, 0.03);
      for (let i = 0; i < built.length; i++) built.channels[0][i] += tone[i];
      return built;
    };

    const sibilant = build(7000);
    const lower = build(3500);

    const sibilantResult = dynamicEq(sibilant.channels, sr);
    const lowerResult = dynamicEq(lower.channels, sr);

    const sibilantCut = levelAt(sibilant.channels[0], 7000) - levelAt(sibilantResult.channels[0], 7000);
    const lowerCut = levelAt(lower.channels[0], 3500) - levelAt(lowerResult.channels[0], 3500);
    expect(sibilantCut).toBeGreaterThan(lowerCut);
  });

  it("does not touch the low end, where the fundamental lives", () => {
    const built = speech();
    const result = dynamicEq(built.signal.channels, sr);
    for (const freq of [110, 200]) {
      expect(Math.abs(levelAt(built.signal.channels[0], freq) - levelAt(result.channels[0], freq)))
        .toBeLessThan(1);
    }
  });

  it("respects the maximum reduction", () => {
    const built = speech();
    const wrecked = cloneSignal(built.signal);
    // A savage resonance, far past what the limit allows.
    const boosted = applyCascade(wrecked.channels[0], [peakingEq(sr, 3000, 24, 8)]);
    wrecked.channels[0] = boosted;

    const result = dynamicEq(wrecked.channels, sr, { maxReductionDb: 6 });
    expect(result.maxReductionDb).toBeLessThanOrEqual(6.001);
  });

  it("is close to transparent on clean speech", () => {
    const built = speech();
    const result = dynamicEq(built.signal.channels, sr);
    const cleaned: Signal = { ...built.signal, channels: result.channels };
    expect(siSdrDb(built.signal, cleaned)).toBeGreaterThan(15);
  });

  it("does almost nothing when the threshold is unreachable", () => {
    const built = speech();
    const result = dynamicEq(built.signal.channels, sr, { thresholdDb: 200 });
    expect(result.activeFraction).toBe(0);
    for (let i = 0; i < built.signal.length; i += 101) {
      expect(result.channels[0][i]).toBeCloseTo(built.signal.channels[0][i], 5);
    }
  });

  it("reacts faster to a resonance arriving than to it leaving", () => {
    // Attack shorter than release is what keeps the gain from fluttering.
    expect(DEFAULT_DYNEQ_OPTIONS.attackMs).toBeLessThan(DEFAULT_DYNEQ_OPTIONS.releaseMs);
  });

  it("produces finite output on silence and on noise", () => {
    expect(dynamicEq([new Float32Array(sr)], sr).channels[0].every(Number.isFinite)).toBe(true);

    const next = rng(5);
    const noise = new Float32Array(sr);
    for (let i = 0; i < noise.length; i++) noise[i] = (next() * 2 - 1) * 0.3;
    expect(dynamicEq([noise], sr).channels[0].every(Number.isFinite)).toBe(true);
  });

  it("is deterministic", () => {
    const built = speech();
    const a = dynamicEq(built.signal.channels, sr);
    const b = dynamicEq(built.signal.channels, sr);
    expect(Array.from(a.channels[0])).toEqual(Array.from(b.channels[0]));
  });
});
