import { describe, it, expect } from "vitest";
import { convolve } from "../src/dsp/convolve";
import { dereverb } from "../src/dsp/dereverb";
import { syntheticSpeech, syntheticRir, addReverb, cloneSignal } from "../src/eval/signals";
import { siSdrDb, reverbDecayMs } from "../src/eval/metrics";
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
      pauseSec: 1.4,
      floorDbfs: -75,
      seed: 4242,
    });
    cache.set("x", built);
  }
  return built;
}

describe("convolve", () => {
  it("reproduces the signal when convolved with a unit impulse", () => {
    const signal = sine(440, 0.1, sr, 0.5);
    const impulse = new Float32Array(64);
    impulse[0] = 1;

    const out = convolve(signal, impulse);
    for (let i = 0; i < signal.length; i++) expect(out[i]).toBeCloseTo(signal[i], 5);
  });

  it("delays by a shifted impulse", () => {
    const signal = sine(300, 0.05, sr, 0.5);
    const impulse = new Float32Array(64);
    impulse[10] = 1;

    const out = convolve(signal, impulse);
    for (let i = 10; i < signal.length; i++) expect(out[i]).toBeCloseTo(signal[i - 10], 5);
  });

  it("scales by a scaled impulse", () => {
    const signal = sine(300, 0.05, sr, 0.5);
    const impulse = new Float32Array(8);
    impulse[0] = 0.5;
    const out = convolve(signal, impulse);
    for (let i = 0; i < signal.length; i++) expect(out[i]).toBeCloseTo(signal[i] * 0.5, 5);
  });

  it("matches direct convolution on a short case", () => {
    const signal = new Float32Array([1, -2, 3, 0.5, -1, 2, 0, 1]);
    const impulse = new Float32Array([0.5, -0.25, 0.125]);

    const fast = convolve(signal, impulse);
    for (let n = 0; n < signal.length; n++) {
      let expected = 0;
      for (let k = 0; k < impulse.length; k++) {
        if (n - k >= 0) expected += signal[n - k] * impulse[k];
      }
      expect(fast[n]).toBeCloseTo(expected, 6);
    }
  });
});

describe("synthetic room impulse response", () => {
  it("starts with the direct sound and decays", () => {
    const rir = syntheticRir(sr, { rt60Sec: 0.5, directToReverbDb: 6 });
    expect(rir[0]).toBe(1);

    const energyIn = (from: number, to: number): number => {
      let acc = 0;
      for (let i = from; i < to; i++) acc += rir[i] * rir[i];
      return acc;
    };
    const early = energyIn(Math.round(0.02 * sr), Math.round(0.1 * sr));
    const late = energyIn(Math.round(0.3 * sr), Math.round(0.4 * sr));
    expect(early).toBeGreaterThan(late * 5);
  });

  it("honours the requested direct-to-reverberant ratio", () => {
    for (const directToReverbDb of [3, 10]) {
      const rir = syntheticRir(sr, { rt60Sec: 0.5, directToReverbDb });
      let tail = 0;
      for (let i = 1; i < rir.length; i++) tail += rir[i] * rir[i];
      // Direct energy is 1 by construction.
      expect(10 * Math.log10(1 / tail)).toBeCloseTo(directToReverbDb, 0);
    }
  });

  it("is deterministic for a seed", () => {
    const a = syntheticRir(sr, { seed: 7 });
    const b = syntheticRir(sr, { seed: 7 });
    expect(Array.from(a)).toEqual(Array.from(b));
  });

  it("lengthens the measured decay when applied to speech", () => {
    const dry = speech().signal;
    const wet = addReverb(dry, { rt60Sec: 0.7, directToReverbDb: 4 });
    expect(reverbDecayMs(wet)).toBeGreaterThan(reverbDecayMs(dry) * 1.5);
  });

  it("preserves loudness so comparisons stay about reverberation", () => {
    const dry = speech().signal;
    const wet = addReverb(dry, { rt60Sec: 0.6 });
    // addReverb renormalises; SI-SDR should reflect reverb, not a level change.
    let dryPeak = 0;
    let wetPeak = 0;
    for (let i = 0; i < dry.length; i++) {
      dryPeak = Math.max(dryPeak, Math.abs(dry.channels[0][i]));
      wetPeak = Math.max(wetPeak, Math.abs(wet.channels[0][i]));
    }
    expect(wetPeak).toBeLessThan(dryPeak * 3);
  });
});

describe("dereverb (WPE)", () => {
  it("shortens the measured decay of a reverberant recording", () => {
    const dry = speech().signal;
    const wet = addReverb(dry, { rt60Sec: 0.7, directToReverbDb: 4 });

    const result = dereverb(wet.channels);
    const cleaned: Signal = { ...wet, channels: result.channels };

    expect(reverbDecayMs(cleaned)).toBeLessThan(reverbDecayMs(wet));
  });

  it("moves the signal closer to the dry reference", () => {
    const dry = speech().signal;
    const wet = addReverb(dry, { rt60Sec: 0.6, directToReverbDb: 5 });

    const result = dereverb(wet.channels);
    const cleaned: Signal = { ...wet, channels: result.channels };

    expect(siSdrDb(dry, cleaned)).toBeGreaterThan(siSdrDb(dry, wet));
  });

  it("does not wreck a dry recording", () => {
    // The transparency case: nothing to remove, so the voice must survive.
    const dry = speech().signal;
    const result = dereverb(cloneSignal(dry).channels);
    const cleaned: Signal = { ...dry, channels: result.channels };

    // Measured at 20 dB with the shipped settings. The bound is what makes the
    // frame-size choice load-bearing: at the pipeline's usual 1024 this same
    // recording comes back at 1 dB.
    expect(siSdrDb(dry, cleaned)).toBeGreaterThan(15);
  });

  it("leaves the direct sound alone — a zero delay would cancel the voice", () => {
    // With delay 0 the filter is allowed to predict the current frame from the
    // immediately preceding one, which is mostly the speech itself. This test
    // pins down *why* the delay exists.
    const dry = speech().signal;
    const wet = addReverb(dry, { rt60Sec: 0.6, directToReverbDb: 5 });

    const withDelay = dereverb(wet.channels, { delay: 2 });
    const withoutDelay = dereverb(wet.channels, { delay: 0 });

    const good = siSdrDb(dry, { ...wet, channels: withDelay.channels });
    const bad = siSdrDb(dry, { ...wet, channels: withoutDelay.channels });
    expect(good).toBeGreaterThan(bad);
  });

  it("produces finite output on silence", () => {
    const silent = [new Float32Array(sr)];
    const result = dereverb(silent);
    expect(result.channels[0].every((v) => Number.isFinite(v))).toBe(true);
  });

  it("is deterministic", () => {
    const wet = addReverb(speech().signal, { rt60Sec: 0.5 });
    const a = dereverb(wet.channels);
    const b = dereverb(wet.channels);
    expect(Array.from(a.channels[0])).toEqual(Array.from(b.channels[0]));
  });

  it("handles channels independently", () => {
    const wet = addReverb(speech().signal, { rt60Sec: 0.5 });
    const stereo = cloneSignal(wet);
    stereo.channels.push(Float32Array.from(speech().signal.channels[0]));

    const result = dereverb(stereo.channels);
    expect(result.channels).toHaveLength(2);
    expect(result.channels[0].every((v) => Number.isFinite(v))).toBe(true);
    expect(result.channels[1].every((v) => Number.isFinite(v))).toBe(true);
  });
});

describe("long recordings", () => {
  it("measures decay on a file with more frames than a spread can carry", () => {
    // The regression this exists for: `Math.max(...envelope)` where the
    // envelope holds one entry per 5 ms of audio. It survives every fixture in
    // this suite and dies on a real 21-minute recording, because V8's argument
    // limit sits around 65k and that file produces 257,200 frames. Built at a
    // low rate so the test costs a megabyte rather than a gigabyte.
    const rate = 2000;
    const frames = 150_000;
    const samples = new Float32Array(frames * 2); // 2 samples per 1 ms frame
    let seed = 99;
    for (let i = 0; i < samples.length; i++) {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      samples[i] = ((seed / 0x7fffffff) * 2 - 1) * 0.25;
    }

    const signal = { sampleRate: rate, channels: [samples], length: samples.length };
    expect(() => reverbDecayMs(signal, 1)).not.toThrow();
    expect(Number.isNaN(reverbDecayMs(signal, 1))).toBe(false);
  });
});
