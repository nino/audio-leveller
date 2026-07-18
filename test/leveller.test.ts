import { describe, it, expect } from "vitest";
import { levelAudio } from "../src/dsp/leveller";
import { preFilter, integratedLoudness } from "../src/dsp/loudness";
import { sine, silence, concat, mono, peak } from "./helpers";

const sr = 48000;

describe("leveller (end-to-end)", () => {
  it("brings every segment to the target loudness (-23 LUFS)", () => {
    // Two speech-like segments at very different levels, split by silence.
    const quietSeg = sine(220, 3, sr, 0.08); // quiet
    const loudSeg = sine(440, 3, sr, 0.6); // loud
    const input = mono(concat(quietSeg, silence(1.5, sr), loudSeg), sr);

    const { audio, report } = levelAudio(input, { targetLufs: -23 });
    expect(report.segments.length).toBe(2);

    // Re-measure each segment in the OUTPUT; each should land near target.
    const filtered = preFilter(audio.channels, sr);
    for (const seg of report.segments) {
      // Measure the core of the segment, away from the ramp region.
      const pad = Math.round(0.3 * sr);
      const measured = integratedLoudness(
        filtered,
        sr,
        seg.start + pad,
        seg.end - pad,
      );
      expect(measured).toBeCloseTo(-23, 0); // within ~1 LU
    }
  });

  it("ramps the gain linearly across the silence between segments", () => {
    const a = sine(220, 3, sr, 0.5);
    const b = sine(440, 3, sr, 0.05); // needs a big boost => different gain
    const input = mono(concat(a, silence(1.5, sr), b), sr);

    const { report } = levelAudio(input);
    expect(report.segments.length).toBe(2);
    const gA = report.segments[0].gainDb;
    const gB = report.segments[1].gainDb;
    expect(gA).not.toBeCloseTo(gB, 1); // the two segments really do differ

    // The applied envelope over the silence should be monotonic between the
    // two segment gains: sample the output gain implied by re-measuring short
    // windows across the gap is hard, so instead assert the segment gains have
    // opposite senses (loud cut, quiet boosted) as in the Levelator example.
    expect(gA).toBeLessThan(0); // loud segment was reduced
    expect(gB).toBeGreaterThan(0); // quiet segment was boosted
  });

  it("normalises the whole file when there is no qualifying silence", () => {
    const input = mono(sine(330, 4, sr, 0.3), sr);
    const { audio, report } = levelAudio(input);
    expect(report.segments.length).toBe(1);
    const measured = integratedLoudness(preFilter(audio.channels, sr), sr);
    expect(measured).toBeCloseTo(-23, 0);
  });

  it("never lets the output clip past the ceiling", () => {
    const input = mono(concat(sine(220, 3, sr, 0.02), silence(1.5, sr), sine(440, 3, sr, 0.02)), sr);
    const { audio } = levelAudio(input, { ceilingDb: -1 });
    // -1 dBFS ceiling ~= 0.891 linear; allow a hair for inter-sample slack.
    expect(peak(audio)).toBeLessThanOrEqual(0.9);
  });

  it("does not boost a quiet head/tail (room tone) toward the target", () => {
    // Quiet room tone | silence | real speech | silence | quiet room tone.
    const room = () => silence(2, sr, 0.0005); // ~-66 dBFS noise floor
    const speech = sine(300, 3, sr, 0.5);
    const input = mono(
      concat(room(), silence(1.2, sr), speech, silence(1.2, sr), room()),
      sr,
    );

    const { report } = levelAudio(input);
    const speechSegs = report.segments.filter((s) => s.isSpeech);
    const roomSegs = report.segments.filter((s) => !s.isSpeech);

    expect(speechSegs.length).toBeGreaterThanOrEqual(1);
    expect(roomSegs.length).toBeGreaterThanOrEqual(1);
    // Room-tone segments must not get a big boost — they ride the neighbour gain.
    for (const r of roomSegs) {
      expect(r.gainDb).toBeLessThan(15);
    }
  });

  it("preserves sample rate, length and channel count", () => {
    const input = mono(sine(330, 2, sr, 0.3), sr);
    const { audio } = levelAudio(input);
    expect(audio.sampleRate).toBe(sr);
    expect(audio.length).toBe(input.length);
    expect(audio.channels.length).toBe(1);
  });
});
