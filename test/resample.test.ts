import { describe, it, expect } from "vitest";
import { resample, resampledLength } from "../src/dsp/resample";
import { sine } from "./helpers";

/** Signal-to-noise ratio (dB) of `actual` against `expected`, over a range. */
function snrDb(expected: Float32Array, actual: Float32Array, start: number, end: number): number {
  let signal = 0;
  let noise = 0;
  for (let i = start; i < end; i++) {
    signal += expected[i] * expected[i];
    const err = actual[i] - expected[i];
    noise += err * err;
  }
  if (noise === 0) return Infinity;
  return 10 * Math.log10(signal / noise);
}

describe("resample", () => {
  it("returns an exact copy when the rate is unchanged", () => {
    const input = sine(1000, 0.1, 48000, 0.5);
    const [out] = resample([input], 48000, 48000);
    expect(out).not.toBe(input); // a copy, not the same array
    expect(Array.from(out)).toEqual(Array.from(input));
  });

  it("produces the expected output length", () => {
    const input = new Float32Array(44100);
    const [out] = resample([input], 44100, 48000);
    expect(out.length).toBe(48000);
    expect(resampledLength(44100, 44100, 48000)).toBe(48000);
  });

  it("preserves DC exactly", () => {
    const input = new Float32Array(4096).fill(0.25);
    const [out] = resample([input], 44100, 48000);
    // Check the interior, away from the edge fade.
    for (let i = 500; i < out.length - 500; i++) {
      expect(out[i]).toBeCloseTo(0.25, 5);
    }
  });

  it("round-trips a 1 kHz tone at better than 90 dB SNR", () => {
    const sr = 44100;
    const original = sine(1000, 0.5, sr, 0.5);
    const [up] = resample([original], sr, 48000);
    const [back] = resample([up], 48000, sr);

    expect(back.length).toBe(original.length);
    // Skip the filter's edge transient at both ends.
    const pad = 2000;
    expect(snrDb(original, back, pad, original.length - pad)).toBeGreaterThan(90);
  });

  it("round-trips a high-frequency tone without aliasing it", () => {
    const sr = 48000;
    const original = sine(8000, 0.5, sr, 0.5);
    const [down] = resample([original], sr, 44100);
    const [back] = resample([down], 44100, sr);

    const pad = 2000;
    expect(snrDb(original, back, pad, original.length - pad)).toBeGreaterThan(80);
  });

  it("rejects content above the new Nyquist instead of aliasing it", () => {
    // 20 kHz cannot survive a trip to 32 kHz (16 kHz Nyquist): it must be
    // filtered away, NOT folded back down as an audible alias.
    const sr = 48000;
    const original = sine(20000, 0.3, sr, 0.5);
    const [down] = resample([original], sr, 32000);

    let peak = 0;
    for (let i = 500; i < down.length - 500; i++) peak = Math.max(peak, Math.abs(down[i]));
    expect(peak).toBeLessThan(0.01); // ~-40 dB or better rejection
  });

  it("handles multiple channels independently", () => {
    const a = sine(500, 0.2, 48000, 0.4);
    const b = sine(3000, 0.2, 48000, 0.2);
    const out = resample([a, b], 48000, 96000);
    expect(out.length).toBe(2);
    expect(out[0].length).toBe(a.length * 2);
    expect(Array.from(out[0])).not.toEqual(Array.from(out[1]));
  });

  it("handles an awkward rate pair with no useful common factor", () => {
    const original = sine(1000, 0.3, 44101, 0.5);
    const [out] = resample([original], 44101, 48000);
    expect(out.length).toBe(resampledLength(original.length, 44101, 48000));

    let peak = 0;
    for (let i = 1000; i < out.length - 1000; i++) peak = Math.max(peak, Math.abs(out[i]));
    expect(peak).toBeGreaterThan(0.49);
    expect(peak).toBeLessThan(0.51);
  });
});
