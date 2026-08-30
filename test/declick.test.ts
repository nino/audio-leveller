import { describe, it, expect } from "vitest";
import { declick, DEFAULT_DECLICK_OPTIONS } from "../src/dsp/declick";
import { syntheticSpeech, addClicks, rng } from "../src/eval/signals";
import type { Signal } from "../src/pipeline/types";

const sr = 48000;

const speechCache = new Map<string, ReturnType<typeof syntheticSpeech>>();
function speech(seconds = 3, seed = 4242) {
  const key = `${seconds}:${seed}`;
  let built = speechCache.get(key);
  if (!built) {
    built = syntheticSpeech({
      sampleRate: sr,
      segments: [{ seconds, levelLufs: -23, f0: 110 }],
      pauseSec: 0.5,
      floorDbfs: -62,
      seed,
    });
    speechCache.set(key, built);
  }
  return built;
}

/** Peak absolute difference between two channels. */
function maxDiff(a: Float32Array, b: Float32Array): number {
  let worst = 0;
  for (let i = 0; i < a.length; i++) worst = Math.max(worst, Math.abs(a[i] - b[i]));
  return worst;
}

/** RMS of a channel. */
function rms(a: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += a[i] * a[i];
  return Math.sqrt(sum / a.length);
}

/** How many of `positions` have a burst covering them. */
function covered(bursts: { start: number; end: number }[], positions: number[]): number {
  return positions.filter((p) => bursts.some((b) => p >= b.start - 4 && p < b.end + 4)).length;
}

describe("declick detection", () => {
  it("finds essentially every click that was injected", () => {
    const { signal } = speech();
    // 2x the waveform peak and 50 ms apart, as in the eval corpus. The
    // synthetic voice's excitation is far more impulsive than a real one, so
    // only clicks this size stand out from it the way real clicks stand out
    // from a real voice; the pulse-train veto deliberately leaves anything
    // within ~10 dB of the surrounding excitation alone. Real sensitivity is
    // measured on real material in the eval fixture cases.
    const { signal: clicked, positions } = addClicks(signal, {
      count: 30,
      relativeAmplitude: 2,
      minGapSec: 0.05,
      seed: 99,
    });

    const result = declick(clicked.channels, sr);
    expect(result.aborted).toBe(false);
    // Allow a couple to fall in the unexaminable head/tail context region.
    expect(covered(result.bursts, positions)).toBeGreaterThanOrEqual(positions.length - 2);
  });

  it("localises the burst instead of smearing it over the model order", () => {
    // The whole point of requiring forward and backward residuals to agree:
    // a naive forward-only detector would flag ~32 samples per click here.
    const { signal } = speech();
    const { signal: clicked } = addClicks(signal, {
      count: 20,
      relativeAmplitude: 0.6,
      seed: 5,
    });

    const result = declick(clicked.channels, sr);
    for (const burst of result.bursts) {
      // 3-sample clicks, dilated by 1 each side, so anything past ~12 samples
      // means the detector is smearing.
      expect(burst.end - burst.start).toBeLessThan(12);
    }
    expect(result.samplesRepaired).toBeLessThan(DEFAULT_DECLICK_OPTIONS.order * 20);
  });

  it("leaves clean speech essentially untouched", () => {
    // The transparency test. A de-clicker that mangles clean audio is worse
    // than no de-clicker, because most material has no clicks at all.
    const { signal } = speech();
    const result = declick(signal.channels, sr);

    const changed = maxDiff(signal.channels[0], result.channels[0]);
    const level = rms(signal.channels[0]);
    // Any change must be far below the programme level.
    expect(20 * Math.log10((changed || 1e-12) / level)).toBeLessThan(-30);
    expect(result.repairedFraction).toBeLessThan(0.001);
  });

  it("does not fire on a pure tone", () => {
    const n = sr * 2;
    const tone = new Float32Array(n);
    for (let i = 0; i < n; i++) tone[i] = 0.5 * Math.sin((2 * Math.PI * 440 * i) / sr);

    const result = declick([tone], sr);
    expect(result.samplesRepaired).toBe(0);
  });

  it("does not fire on white noise, which has no predictable structure", () => {
    // Noise is the adversarial case: every sample is unpredictable, so the
    // residual is large everywhere. A robust threshold must still see no
    // *outliers*, because nothing stands out from anything else.
    const next = rng(3);
    const noise = new Float32Array(sr * 2);
    for (let i = 0; i < noise.length; i++) noise[i] = (next() * 2 - 1) * 0.2;

    const result = declick([noise], sr);
    expect(result.repairedFraction).toBeLessThan(0.005);
  });

  it("ignores events too long to be clicks", () => {
    const { signal } = speech();
    const damaged = Float32Array.from(signal.channels[0]);
    // A 20 ms burst – a door slam, not a click.
    const start = sr;
    const next = rng(17);
    for (let i = start; i < start + sr * 0.02; i++) damaged[i] += (next() * 2 - 1) * 0.5;

    const result = declick([damaged], sr);
    const insideEvent = result.bursts.filter((b) => b.start >= start && b.end <= start + sr * 0.02);
    // It may nibble at the sharp edges, but must not rewrite the whole event.
    const repairedInside = insideEvent.reduce((sum, b) => sum + (b.end - b.start), 0);
    expect(repairedInside).toBeLessThan(sr * 0.02 * 0.5);
  });
});

describe("declick pulse-train veto", () => {
  /**
   * A bare pulse train through a resonator: the most impulsive "voice"
   * possible, every pulse a textbook outlier in the LPC residual. Without the
   * veto the detector repairs all of them; with it, none – and a click that
   * towers over the pulses is still caught.
   */
  function pulseTrain(seconds: number, f0: number): Float32Array {
    const n = Math.round(seconds * sr);
    const out = new Float32Array(n);
    const period = sr / f0;
    let next = 100;
    const noise = rng(7);
    for (let i = 0; i < n; i++) {
      // A little aspiration noise, so the residual has a scale to threshold
      // against – an exactly-zero floor is treated as "nothing to measure".
      out[i] = (noise() - 0.5) * 0.03;
      if (i >= next) {
        out[i] += 1;
        next += period * (1 + 0.02 * Math.sin(i / 700));
      }
    }
    // Two-pole resonator ~ a formant at 700 Hz.
    const r = 0.985;
    const w = (2 * Math.PI * 700) / sr;
    let y1 = 0;
    let y2 = 0;
    for (let i = 0; i < n; i++) {
      const y = out[i] + 2 * r * Math.cos(w) * y1 - r * r * y2;
      out[i] = y * 0.05;
      y2 = y1;
      y1 = y;
    }
    return out;
  }

  it("leaves a periodic pulse train alone, where the bare detector would not", () => {
    const train = pulseTrain(2, 120);
    const bare = declick([train], sr, { pulseVetoRatio: 1e9 });
    const vetoed = declick([train], sr);
    expect(bare.bursts.length).toBeGreaterThan(50);
    expect(vetoed.bursts.length).toBe(0);
    // `vetoed` counts raw candidates, `bursts` are post-merge – so at least.
    expect(vetoed.vetoed).toBeGreaterThanOrEqual(bare.bursts.length);
  });

  it("still catches a click that towers over the pulses", () => {
    const train = pulseTrain(2, 120);
    const clicked = Float32Array.from(train);
    const at = Math.round(1.0137 * sr);
    let peak = 0;
    for (let i = 0; i < train.length; i++) peak = Math.max(peak, Math.abs(train[i]));
    clicked[at] += peak * 1.5;
    clicked[at + 1] -= peak * 1.2;
    const result = declick([clicked], sr);
    expect(result.bursts.length).toBe(1);
    expect(result.bursts[0].start).toBeLessThanOrEqual(at);
    expect(result.bursts[0].end).toBeGreaterThan(at + 1);
  });
});

describe("declick repair", () => {
  it("removes the click energy it detects", () => {
    const { signal } = speech();
    const { signal: clicked, positions } = addClicks(signal, {
      count: 25,
      relativeAmplitude: 2,
      minGapSec: 0.05,
      seed: 21,
    });

    const result = declick(clicked.channels, sr);

    // Compare the repaired signal against the original clean one at the click
    // sites it reported: the residual there should be far smaller than the
    // clicks were. (Detection coverage is asserted separately above; here the
    // question is whether a repair, once attempted, actually removes the click.)
    const detected = positions.filter((p) => result.bursts.some((b) => p >= b.start - 4 && p < b.end + 4));
    expect(detected.length).toBeGreaterThanOrEqual(Math.floor(positions.length * 0.8));
    let worstBefore = 0;
    let worstAfter = 0;
    for (const p of detected) {
      for (let i = Math.max(0, p - 4); i < Math.min(signal.length, p + 8); i++) {
        worstBefore = Math.max(worstBefore, Math.abs(clicked.channels[0][i] - signal.channels[0][i]));
        worstAfter = Math.max(worstAfter, Math.abs(result.channels[0][i] - signal.channels[0][i]));
      }
    }
    expect(worstAfter).toBeLessThan(worstBefore * 0.2);
  });

  it("does not modify samples outside the bursts it reports", () => {
    const { signal } = speech();
    const { signal: clicked } = addClicks(signal, {
      count: 15,
      relativeAmplitude: 0.5,
      seed: 33,
    });

    const result = declick(clicked.channels, sr);
    const inBurst = (i: number): boolean =>
      result.bursts.some((b) => b.repaired && i >= b.start && i < b.end);

    for (let i = 0; i < signal.length; i++) {
      if (inBurst(i)) continue;
      expect(result.channels[0][i]).toBe(clicked.channels[0][i]);
    }
  });

  it("handles each channel independently", () => {
    const { signal } = speech();
    const stereo: Signal = {
      sampleRate: sr,
      length: signal.length,
      channels: [Float32Array.from(signal.channels[0]), Float32Array.from(signal.channels[0])],
    };
    // Damage the right channel only.
    const at = Math.round(sr * 1.5);
    stereo.channels[1][at] += 0.5;
    stereo.channels[1][at + 1] -= 0.4;

    const result = declick(stereo.channels, sr);
    expect(result.bursts.some((b) => b.channel === 1)).toBe(true);
    // The intact left channel should come back untouched.
    expect(Array.from(result.channels[0])).toEqual(Array.from(stereo.channels[0]));
  });

  it("declines to touch the audio when detection runs away", () => {
    // A signal that is nothing but impulses: the threshold cannot be right, so
    // the safe behaviour is to do nothing and report it.
    const next = rng(9);
    const spiky = new Float32Array(sr);
    for (let i = 0; i < spiky.length; i++) spiky[i] = next() > 0.5 ? 0.8 : -0.8;

    const result = declick([spiky], sr, { maxRepairFraction: 0.001 });
    if (result.aborted) {
      expect(Array.from(result.channels[0])).toEqual(Array.from(spiky));
      expect(result.samplesRepaired).toBe(0);
    } else {
      // If it didn't trip the guard, it must at least have stayed conservative.
      expect(result.repairedFraction).toBeLessThanOrEqual(0.001);
    }
  });

  it("is deterministic", () => {
    const { signal } = speech();
    const { signal: clicked } = addClicks(signal, {
      count: 10,
      relativeAmplitude: 0.5,
      seed: 77,
    });
    const a = declick(clicked.channels, sr);
    const b = declick(clicked.channels, sr);
    expect(Array.from(a.channels[0])).toEqual(Array.from(b.channels[0]));
  });
});
