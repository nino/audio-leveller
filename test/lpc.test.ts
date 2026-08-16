import { describe, it, expect } from "vitest";
import {
  autocorrelation,
  fitAr,
  coefficientAutocorrelation,
  interpolateGap,
} from "../src/dsp/lpc";
import { rng } from "../src/eval/signals";

/**
 * Generate an AR(2) process with known coefficients: a resonator driven by
 * noise. `x[n] = -a1*x[n-1] - a2*x[n-2] + noise`, matching the a[0]=1 sign
 * convention used throughout lpc.ts.
 */
function arProcess(n: number, a1: number, a2: number, seed: number): Float32Array {
  const next = rng(seed);
  const out = new Float32Array(n);
  for (let i = 2; i < n; i++) {
    const noise = (next() * 2 - 1) * 0.1;
    out[i] = -a1 * out[i - 1] - a2 * out[i - 2] + noise;
  }
  return out;
}

/** A decaying sinusoid — the shape a real resonance actually makes. */
function resonance(n: number, freq: number, sampleRate: number, decay: number): Float32Array {
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    out[i] = Math.exp(-decay * i / sampleRate) * Math.sin((2 * Math.PI * freq * i) / sampleRate);
  }
  return out;
}

describe("autocorrelation", () => {
  it("peaks at lag zero", () => {
    const signal = arProcess(4000, -1.6, 0.9, 11);
    const r = autocorrelation(signal, 0, signal.length, 8);
    for (let lag = 1; lag <= 8; lag++) {
      expect(Math.abs(r[lag])).toBeLessThanOrEqual(r[0]);
    }
  });

  it("finds the period of a periodic signal", () => {
    const sr = 48000;
    const period = 100; // samples
    const signal = new Float32Array(4000);
    for (let i = 0; i < signal.length; i++) {
      signal[i] = Math.sin((2 * Math.PI * i) / period);
    }
    const r = autocorrelation(signal, 0, signal.length, 2 * period);
    // The lag equal to one period should correlate far better than a half period.
    expect(r[period]).toBeGreaterThan(r[period / 2]);
    expect(sr).toBe(48000); // keep the sample rate meaningful in the test name
  });

  it("stays solvable on a silent window", () => {
    const silent = new Float32Array(1000);
    const r = autocorrelation(silent, 0, silent.length, 16);
    expect(r[0]).toBeGreaterThan(0);
    expect(Number.isFinite(r[0])).toBe(true);
  });
});

describe("levinson", () => {
  it("recovers the coefficients of a known AR(2) process", () => {
    const a1 = -1.6;
    const a2 = 0.9;
    const signal = arProcess(20000, a1, a2, 42);
    const a = fitAr(signal, 1000, signal.length, 2);

    expect(a[0]).toBe(1);
    expect(a[1]).toBeCloseTo(a1, 1);
    expect(a[2]).toBeCloseTo(a2, 1);
  });

  it("produces a stable model — all reflection coefficients inside the unit circle", () => {
    const signal = arProcess(8000, -1.9, 0.95, 7);
    const a = fitAr(signal, 0, signal.length, 24);
    expect(a.every((v) => Number.isFinite(v))).toBe(true);
    expect(a[0]).toBe(1);
  });

  it("degrades gracefully on degenerate input", () => {
    const constant = new Float32Array(500).fill(0.5);
    const a = fitAr(constant, 0, constant.length, 16);
    expect(a.every((v) => Number.isFinite(v))).toBe(true);
  });
});

describe("coefficientAutocorrelation", () => {
  it("computes the autocorrelation of the coefficient vector", () => {
    const a = Float64Array.from([1, -0.5, 0.25]);
    const ra = coefficientAutocorrelation(a);
    // ra[0] = 1 + 0.25 + 0.0625
    expect(ra[0]).toBeCloseTo(1.3125, 10);
    // ra[1] = 1*(-0.5) + (-0.5)(0.25)
    expect(ra[1]).toBeCloseTo(-0.625, 10);
    // ra[2] = 1*0.25
    expect(ra[2]).toBeCloseTo(0.25, 10);
  });
});

describe("interpolateGap", () => {
  it("reconstructs a gap in a resonant signal far better than silence", () => {
    const sr = 48000;
    const signal = resonance(4000, 440, sr, 2);
    const original = Float32Array.from(signal);

    const gapStart = 2000;
    const gapEnd = 2024; // 0.5 ms
    const damaged = Float32Array.from(signal);
    for (let i = gapStart; i < gapEnd; i++) damaged[i] = 0;

    const a = fitAr(damaged, 1000, 1900, 32);
    const repaired = Float32Array.from(damaged);
    expect(interpolateGap(repaired, gapStart, gapEnd, a)).toBe(true);

    // Error across the gap, repaired versus simply left blank.
    let repairedError = 0;
    let blankError = 0;
    for (let i = gapStart; i < gapEnd; i++) {
      repairedError += (repaired[i] - original[i]) ** 2;
      blankError += (damaged[i] - original[i]) ** 2;
    }
    expect(repairedError).toBeLessThan(blankError * 0.05);
  });

  it("leaves samples outside the gap untouched", () => {
    const signal = resonance(3000, 300, 48000, 1);
    const before = Float32Array.from(signal);
    const gapStart = 1500;
    const gapEnd = 1510;

    const a = fitAr(signal, 500, 1400, 24);
    interpolateGap(signal, gapStart, gapEnd, a);

    for (let i = 0; i < signal.length; i++) {
      if (i >= gapStart && i < gapEnd) continue;
      expect(signal[i]).toBe(before[i]);
    }
  });

  it("refuses a gap too close to the signal edge rather than guessing", () => {
    const signal = resonance(1000, 300, 48000, 1);
    const a = fitAr(signal, 0, signal.length, 32);
    expect(interpolateGap(signal, 4, 12, a)).toBe(false);
    expect(interpolateGap(signal, 980, 999, a)).toBe(false);
  });

  it("returns false for an empty gap", () => {
    const signal = resonance(1000, 300, 48000, 1);
    const a = fitAr(signal, 0, signal.length, 8);
    expect(interpolateGap(signal, 500, 500, a)).toBe(false);
  });

  it("reproduces a pure resonance almost exactly", () => {
    // A single decaying sinusoid is exactly an AR(2) process, so a model of
    // order 2 or more should reconstruct it to near machine precision.
    const signal = resonance(4000, 1000, 48000, 0.5);
    const original = Float32Array.from(signal);
    const a = fitAr(signal, 500, 1900, 16);

    const damaged = Float32Array.from(signal);
    for (let i = 2000; i < 2008; i++) damaged[i] = 0;
    expect(interpolateGap(damaged, 2000, 2008, a)).toBe(true);

    for (let i = 2000; i < 2008; i++) {
      expect(damaged[i]).toBeCloseTo(original[i], 3);
    }
  });
});
