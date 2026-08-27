/**
 * The waveform peak cache. The interesting contract, established by
 * adversarial review of the Int16-snapshot change: every value peaksFor
 * returns lies in [-1, 1], both branches (pyramid above 256 samples/px, raw
 * snapshot below) agree within one quantisation step, and out-of-range
 * columns are (0, 0) — never a leaked sentinel.
 */

import { describe, expect, it } from "vitest";
import { buildPeaks, peaksFor } from "../listen/src/peaks";

function fakeBuffer(data: Float32Array, sampleRate = 48000) {
  return {
    length: data.length,
    sampleRate,
    numberOfChannels: 1,
    getChannelData: () => data,
  } as unknown as AudioBuffer;
}

const sine = (n: number, amp: number): Float32Array => {
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) out[i] = amp * Math.sin((2 * Math.PI * 220 * i) / 48000);
  return out;
};

describe("peaksFor", () => {
  it("round-trips within one quantisation step on both sides of the seam", () => {
    const cache = buildPeaks(fakeBuffer(sine(48000 * 4, 0.5)));
    for (const spp of [512, 256, 255, 64, 8]) {
      const { min, max } = peaksFor(cache, 0, spp, 200);
      let peak = 0;
      for (let x = 0; x < 200; x++) peak = Math.max(peak, max[x], -min[x]);
      expect(Math.abs(peak - 0.5), `spp ${spp}`).toBeLessThan(2 / 32768);
    }
  });

  it("never leaves [-1, 1], including full-scale negative samples", () => {
    const data = sine(48000, 0.9);
    data[100] = -1; // exactly full scale: -32768 must not decode past -1
    data[200] = 1;
    const cache = buildPeaks(fakeBuffer(data));
    for (const spp of [300, 100, 4]) {
      const { min, max } = peaksFor(cache, 0, spp, 400);
      for (let x = 0; x < 400; x++) {
        expect(min[x]).toBeGreaterThanOrEqual(-1);
        expect(max[x]).toBeLessThanOrEqual(1);
      }
    }
  });

  it("clamps over-unity float material identically in both branches", () => {
    const data = sine(48000 * 2, 0.4);
    data[24000] = 1.5; // float files can exceed full scale
    data[24001] = -1.5;
    const cache = buildPeaks(fakeBuffer(data));
    const pyramid = peaksFor(cache, 0, 512, 200);
    const raw = peaksFor(cache, 0, 100, 960);
    const peakOf = (r: { min: Float32Array; max: Float32Array }): number => {
      let p = 0;
      for (let x = 0; x < r.max.length; x++) p = Math.max(p, r.max[x], -r.min[x]);
      return p;
    };
    // Both say full scale, neither says 1.5 — the diagnostic that compares
    // the branches must not see a phantom mismatch on hot material.
    expect(peakOf(pyramid)).toBe(1);
    expect(peakOf(raw)).toBe(1);
  });

  it("returns (0, 0) for columns past the end, and no sentinel ever escapes", () => {
    const cache = buildPeaks(fakeBuffer(sine(1000, 0.5)));
    const { min, max } = peaksFor(cache, 0, 100, 50); // columns 10+ are past the end
    for (let x = 12; x < 50; x++) {
      expect(min[x]).toBe(0);
      expect(max[x]).toBe(0);
    }
    // Out-of-range start: empty columns must be (0, 0), not ±32768 or ±1.00003.
    const oob = peaksFor(cache, -5000, 10, 20);
    for (let x = 0; x < 20; x++) {
      expect(Math.abs(oob.min[x])).toBeLessThanOrEqual(1);
      expect(Math.abs(oob.max[x])).toBeLessThanOrEqual(1);
    }
  });

  it("introduces no DC offset: a symmetric signal stays symmetric", () => {
    const cache = buildPeaks(fakeBuffer(sine(48000 * 2, 0.3)));
    const { min, max } = peaksFor(cache, 0, 50, 1000);
    let sum = 0;
    for (let x = 0; x < 1000; x++) sum += min[x] + max[x];
    expect(Math.abs(sum / 1000)).toBeLessThan(1e-3);
  });

  it("folds stereo the same way in both branches", () => {
    const a = new Float32Array(48000).fill(0.2);
    const b = new Float32Array(48000).fill(-0.8);
    const buffer = {
      length: 48000,
      sampleRate: 48000,
      numberOfChannels: 2,
      getChannelData: (c: number) => (c === 0 ? a : b),
    } as unknown as AudioBuffer;
    const cache = buildPeaks(buffer);
    const pyramid = peaksFor(cache, 0, 512, 10);
    const raw = peaksFor(cache, 0, 100, 10);
    for (const r of [pyramid, raw]) {
      expect(r.max[0]).toBeCloseTo(0.2, 3);
      expect(r.min[0]).toBeCloseTo(-0.8, 3);
    }
  });
});
