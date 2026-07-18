import { describe, it, expect } from "vitest";
import { preFilter } from "../src/dsp/loudness";
import { analyzeSilence, otsuThreshold } from "../src/dsp/silence";
import { sine, silence, concat } from "./helpers";

const sr = 48000;

describe("silence detection", () => {
  it("finds a long gap between two tones", () => {
    // 2s tone | 1.5s silence | 2s tone
    const signal = concat(
      sine(300, 2, sr, 0.5),
      silence(1.5, sr),
      sine(300, 2, sr, 0.5),
    );
    const analysis = analyzeSilence(preFilter([signal], sr), sr);
    expect(analysis.regions.length).toBe(1);

    const gap = analysis.regions[0];
    // The gap should sit roughly where we put it (2s .. 3.5s).
    expect(gap.start / sr).toBeGreaterThan(1.8);
    expect(gap.end / sr).toBeLessThan(3.7);
    expect(gap.mid / sr).toBeGreaterThan(2.4);
    expect(gap.mid / sr).toBeLessThan(3.1);
  });

  it("ignores gaps shorter than minSilenceSec", () => {
    // A 0.3s gap should not qualify with the default 1.0s minimum.
    const signal = concat(
      sine(300, 2, sr, 0.5),
      silence(0.3, sr),
      sine(300, 2, sr, 0.5),
    );
    const analysis = analyzeSilence(preFilter([signal], sr), sr);
    expect(analysis.regions.length).toBe(0);
  });

  it("puts the threshold between the floor and the integrated loudness", () => {
    const signal = concat(sine(300, 2, sr, 0.5), silence(1.5, sr), sine(300, 2, sr, 0.5));
    const analysis = analyzeSilence(preFilter([signal], sr), sr);
    expect(analysis.thresholdLufs).toBeGreaterThan(analysis.floorLufs);
    expect(analysis.thresholdLufs).toBeLessThan(analysis.integratedLufs);
  });

  it("otsu finds a valley between two clusters of loudness values", () => {
    const values = [
      ...Array(50).fill(-55),
      ...Array(50).fill(-20),
    ];
    const t = otsuThreshold(values);
    expect(t).toBeGreaterThan(-55);
    expect(t).toBeLessThan(-20);
  });

  it("reports no silence when the whole file is loud", () => {
    const signal = sine(300, 4, sr, 0.5);
    const analysis = analyzeSilence(preFilter([signal], sr), sr);
    expect(analysis.regions.length).toBe(0);
  });
});
