import { describe, it, expect } from "vitest";
import { preFilter, loudnessOfRange, integratedLoudness } from "../src/dsp/loudness";
import { sine, silence, concat } from "./helpers";

const sr = 48000;

describe("loudness measurement (BS.1770)", () => {
  it("follows the exact scaling law: ×k => +20·log10(k) LU", () => {
    const base = sine(1000, 1, sr, 0.5);
    const half = base.map((s) => s * 0.5);
    const lBase = loudnessOfRange(preFilter([base], sr), 0, base.length);
    const lHalf = loudnessOfRange(preFilter([half], sr), 0, half.length);
    // Halving amplitude is -6.02 dB.
    expect(lHalf - lBase).toBeCloseTo(-6.0206, 2);
  });

  it("adds ~+3.01 LU when the same signal is duplicated to a second channel", () => {
    const s = sine(1000, 1, sr, 0.5);
    const monoL = loudnessOfRange(preFilter([s], sr), 0, s.length);
    const stereoL = loudnessOfRange(preFilter([s, s], sr), 0, s.length);
    expect(stereoL - monoL).toBeCloseTo(3.0103, 2);
  });

  it("gives a physically sensible absolute value for a full-scale 1 kHz sine", () => {
    // Mono 1 kHz sine at 0 dBFS peak: RMS is -3 dBFS, K-weighting ~unity here,
    // minus the 0.691 offset => roughly -3.7 LUFS.
    const s = sine(1000, 1, sr, 1.0);
    const l = loudnessOfRange(preFilter([s], sr), 0, s.length);
    expect(l).toBeGreaterThan(-5);
    expect(l).toBeLessThan(-2);
  });

  it("gates out trailing silence in integrated loudness", () => {
    const loud = sine(1000, 3, sr, 0.5);
    const quiet = silence(3, sr);
    const filtered = preFilter([concat(loud, quiet)], sr);
    const loudOnly = loudnessOfRange(preFilter([loud], sr), 0, loud.length);
    const integrated = integratedLoudness(filtered, sr, 0, loud.length + quiet.length);
    // Gating should keep integrated close to the loud portion, NOT drag it down
    // toward the -inf silence (a naive mean would be ~3 dB lower).
    expect(integrated).toBeCloseTo(loudOnly, 0); // within ~0.5 LU
    expect(Math.abs(integrated - loudOnly)).toBeLessThan(0.5);
  });

  it("returns -Infinity for pure digital silence", () => {
    const zeros = new Float32Array(sr);
    const l = loudnessOfRange(preFilter([zeros], sr), 0, zeros.length);
    expect(l).toBe(-Infinity);
  });
});
