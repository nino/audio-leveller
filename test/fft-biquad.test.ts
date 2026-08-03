import { describe, it, expect } from "vitest";
import { fftComplex, hannWindow, powerSpectrum } from "../src/dsp/fft";
import {
  peakingEq,
  lowShelf,
  highShelf,
  butterworthHighPass,
  magnitudeDb,
  applyCascade,
} from "../src/dsp/biquad";
import { sine } from "./helpers";

const sr = 48000;

describe("fft", () => {
  it("transforms an impulse to a flat spectrum", () => {
    const n = 64;
    const data = new Float64Array(2 * n);
    data[0] = 1; // delta at t=0
    fftComplex(data);
    for (let k = 0; k < n; k++) {
      expect(data[2 * k]).toBeCloseTo(1, 10);
      expect(data[2 * k + 1]).toBeCloseTo(0, 10);
    }
  });

  it("puts a full-scale bin at the frequency of a pure tone", () => {
    const n = 1024;
    const bin = 100;
    const frame = new Float32Array(n);
    for (let i = 0; i < n; i++) frame[i] = Math.cos((2 * Math.PI * bin * i) / n);

    const rect = new Float64Array(n).fill(1);
    const psd = powerSpectrum(frame, 0, rect);

    let maxBin = 0;
    for (let k = 1; k <= n / 2; k++) if (psd[k] > psd[maxBin]) maxBin = k;
    expect(maxBin).toBe(bin);
    // Everything away from the tone should be essentially zero.
    expect(psd[bin] / (psd[bin - 10] + 1e-30)).toBeGreaterThan(1e10);
  });

  it("rejects non-power-of-two sizes", () => {
    expect(() => fftComplex(new Float64Array(2 * 100))).toThrow(/power of two/);
  });

  it("windows sum correctly for overlapped averaging", () => {
    // Periodic Hann at 50% overlap sums to a constant — the property Welch
    // averaging relies on.
    const n = 256;
    const w = hannWindow(n);
    for (let i = 0; i < n / 2; i++) {
      expect(w[i] + w[i + n / 2]).toBeCloseTo(1, 10);
    }
  });
});

describe("biquad designs", () => {
  it("peaking EQ hits its gain at centre and ~0 dB far away", () => {
    for (const gain of [-6, -2.5, 4]) {
      const c = peakingEq(sr, 1000, gain, 1.4);
      expect(magnitudeDb(c, 1000, sr)).toBeCloseTo(gain, 1);
      expect(magnitudeDb(c, 60, sr)).toBeCloseTo(0, 1);
      expect(magnitudeDb(c, 16000, sr)).toBeCloseTo(0, 1);
    }
  });

  it("shelves hit their gain in the shelf region and 0 dB opposite", () => {
    const low = lowShelf(sr, 200, -5);
    expect(magnitudeDb(low, 30, sr)).toBeCloseTo(-5, 1);
    expect(magnitudeDb(low, 5000, sr)).toBeCloseTo(0, 1);

    const high = highShelf(sr, 6000, 3);
    expect(magnitudeDb(high, 16000, sr)).toBeCloseTo(3, 1);
    expect(magnitudeDb(high, 300, sr)).toBeCloseTo(0, 1);
  });

  it("butterworth high-pass is -3 dB at its corner and steep below", () => {
    const c = butterworthHighPass(sr, 80);
    expect(magnitudeDb(c, 80, sr)).toBeCloseTo(-3, 0);
    expect(magnitudeDb(c, 20, sr)).toBeLessThan(-20);
    expect(magnitudeDb(c, 1000, sr)).toBeCloseTo(0, 1);
  });

  it("analytic response matches what the filter actually does to a tone", () => {
    const c = peakingEq(sr, 500, -6, 2);
    const tone = sine(500, 0.5, sr, 0.5);
    const filtered = applyCascade(tone, [c]);

    // Steady-state RMS ratio (skip the transient head).
    let inRms = 0;
    let outRms = 0;
    const from = Math.round(0.1 * sr);
    for (let i = from; i < tone.length; i++) {
      inRms += tone[i] * tone[i];
      outRms += filtered[i] * filtered[i];
    }
    const measuredDb = 10 * Math.log10(outRms / inRms);
    expect(measuredDb).toBeCloseTo(magnitudeDb(c, 500, sr), 1);
  });
});
