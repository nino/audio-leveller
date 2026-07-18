import { describe, it, expect } from "vitest";
import { buildRoomTone, weightedMeanGainDb } from "../src/dsp/roomtone";
import type { SilenceRegion } from "../src/dsp/silence";
import { silence, breath, concat } from "./helpers";

const sr = 48000;

function region(startSec: number, endSec: number): SilenceRegion {
  const start = Math.round(startSec * sr);
  const end = Math.round(endSec * sr);
  return { start, end, mid: Math.floor((start + end) / 2) };
}

function stats(ch: Float32Array) {
  let peak = 0;
  let maxJump = 0;
  let nan = 0;
  for (let i = 0; i < ch.length; i++) {
    if (!Number.isFinite(ch[i])) nan++;
    peak = Math.max(peak, Math.abs(ch[i]));
    if (i > 0) maxJump = Math.max(maxJump, Math.abs(ch[i] - ch[i - 1]));
  }
  return { peak, maxJump, nan };
}

describe("weightedMeanGainDb", () => {
  it("weights speech-segment gains by their length and ignores non-speech", () => {
    const segs = [
      { start: 0, end: 100, gainDb: -6, isSpeech: true }, // len 100
      { start: 100, end: 400, gainDb: +2, isSpeech: true }, // len 300
      { start: 400, end: 900, gainDb: +30, isSpeech: false }, // ignored
    ];
    // (-6*100 + 2*300) / 400 = 0
    expect(weightedMeanGainDb(segs)).toBeCloseTo(0, 6);
  });

  it("returns 0 when there is no speech", () => {
    expect(weightedMeanGainDb([{ start: 0, end: 100, gainDb: 5, isSpeech: false }])).toBe(0);
  });
});

describe("buildRoomTone", () => {
  it("loops to reach at least the target duration", () => {
    // A single 2s silence, asked for >=10s of bed.
    const sig = silence(3, sr);
    const rt = buildRoomTone([sig], sr, [region(0.2, 2.5)], 0, { minDurationSec: 10 });
    expect(rt.length / sr).toBeGreaterThanOrEqual(10);
    expect(rt.instances).toBeGreaterThan(rt.clips.length); // it actually looped
  });

  it("applies the given gain in dB", () => {
    const sig = silence(4, sr, 0.01); // steady low-level noise
    const unity = buildRoomTone([sig], sr, [region(0.2, 3.8)], 0, { minDurationSec: 5 });
    const boosted = buildRoomTone([sig], sr, [region(0.2, 3.8)], 6, { minDurationSec: 5 });
    const rms = (ch: Float32Array) =>
      Math.sqrt(ch.reduce((a, b) => a + b * b, 0) / ch.length);
    // +6 dB ~= ×1.995 in amplitude.
    expect(rms(boosted.channels[0]) / rms(unity.channels[0])).toBeCloseTo(1.995, 1);
  });

  it("avoids a subtle click when choosing the cleanest window", () => {
    // A low-level click (~4× the floor) — the kind that a plain crest-factor
    // check over a 1 s window misses because it barely moves the RMS.
    const sig = silence(4, sr, 0.005);
    const clickAt = Math.round(1.0 * sr);
    sig[clickAt] = 0.02;
    sig[clickAt + 1] = -0.018;
    const rt = buildRoomTone([sig], sr, [region(0.2, 3.8)], 0, {
      maxClipSec: 0.5,
      minDurationSec: 0.5,
    });
    expect(rt.clips.length).toBe(1);
    const c = rt.clips[0];
    // The chosen source window must not contain the click.
    const contains = clickAt >= c.start && clickAt < c.end;
    expect(contains).toBe(false);
  });

  it("avoids a breath (louder swell) within a silence", () => {
    // Quiet floor throughout, with a louder breath from 1.0s to 1.8s.
    const sig = silence(4, sr, 0.003);
    const b = breath(0.8, sr, 0.03);
    const off = Math.round(1.0 * sr);
    for (let i = 0; i < b.length; i++) sig[off + i] += b[i];

    const rt = buildRoomTone([sig], sr, [region(0, 4)], 0, {
      maxClipSec: 0.8,
      minDurationSec: 0.8,
    });
    const c = rt.clips[0];
    const overlapsBreath = c.start < off + b.length && c.end > off;
    expect(overlapsBreath).toBe(false);
  });

  it("drops a louder silence in favour of a quieter one", () => {
    const quiet = silence(3, sr, 0.003); // ~-50 dBFS floor
    const loud = silence(3, sr, 0.03); // ~20 dB louder
    const sig = concat(quiet, silence(1, sr, 0.003), loud);

    const rt = buildRoomTone([sig], sr, [region(0.2, 2.8), region(4.2, 6.8)], 0, {
      minDurationSec: 0.5,
    });
    // Every kept clip must come from the quiet silence.
    expect(rt.clips.length).toBe(1);
    expect(rt.clips[0].end).toBeLessThanOrEqual(Math.round(2.8 * sr));
  });

  it("produces a click-free, level-safe bed that starts and ends at zero", () => {
    const sig = silence(3, sr, 0.01);
    const rt = buildRoomTone([sig], sr, [region(0.2, 2.8)], 0, { minDurationSec: 10 });
    const s = stats(rt.channels[0]);
    expect(s.nan).toBe(0);
    expect(s.peak).toBeLessThan(0.5); // low-level noise, no clipping
    expect(s.maxJump).toBeLessThan(0.1); // no discontinuities at crossfades
    expect(Math.abs(rt.channels[0][0])).toBeLessThan(0.002); // fade-in from zero
    expect(Math.abs(rt.channels[0][rt.length - 1])).toBeLessThan(0.002); // fade-out to zero
  });

  it("returns an empty bed when there is no usable silence", () => {
    const rt = buildRoomTone([silence(2, sr)], sr, [], 0);
    expect(rt.length).toBe(0);
    expect(rt.clips.length).toBe(0);
    expect(rt.channels[0].length).toBe(0);
  });

  it("skips silences whose usable core is too short", () => {
    // A 0.4s silence, trimmed 0.15s each end => 0.1s core < 0.3s minClip.
    const rt = buildRoomTone([silence(1, sr)], sr, [region(0.1, 0.5)], 0);
    expect(rt.clips.length).toBe(0);
  });
});
