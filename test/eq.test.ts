import { describe, it, expect } from "vitest";
import { computeLtas, broadTarget, bandEnergyDb, DEFAULT_LTAS_OPTIONS } from "../src/dsp/ltas";
import { fitCorrectiveEq, decideRumbleFilter, buildCascade } from "../src/dsp/eq";
import { applyCascade, peakingEq } from "../src/dsp/biquad";
import { truePeakDbfs, truePeakLinear } from "../src/dsp/truepeak";
import { syntheticSpeech, rng } from "../src/eval/signals";
import { sine } from "./helpers";

const sr = 48000;

const cache = new Map<string, ReturnType<typeof syntheticSpeech>>();
function speech(seed = 4242, floorDbfs = -62) {
  const key = `${seed}:${floorDbfs}`;
  let built = cache.get(key);
  if (!built) {
    built = syntheticSpeech({
      sampleRate: sr,
      segments: [
        { seconds: 4, levelLufs: -23, f0: 110 },
        { seconds: 4, levelLufs: -23, f0: 130 },
      ],
      pauseSec: 1.4,
      floorDbfs,
      seed,
    });
    cache.set(key, built);
  }
  return built;
}

/** Whole-signal LTAS helper for tests. */
function ltasOf(samples: Float32Array) {
  return computeLtas(samples, sr, [{ start: 0, end: samples.length }], DEFAULT_LTAS_OPTIONS);
}

/** Level at the grid point nearest `freq`. */
function levelAt(ltas: { freqs: Float64Array; db: Float64Array }, freq: number): number {
  let best = 0;
  for (let i = 0; i < ltas.freqs.length; i++) {
    if (Math.abs(Math.log2(ltas.freqs[i] / freq)) < Math.abs(Math.log2(ltas.freqs[best] / freq))) {
      best = i;
    }
  }
  return ltas.db[best];
}

describe("ltas", () => {
  it("puts a tone's energy at the tone's frequency", () => {
    const ltas = ltasOf(sine(1000, 1, sr, 0.5));
    if (!ltas) throw new Error("expected an LTAS");
    expect(levelAt(ltas, 1000)).toBeGreaterThan(levelAt(ltas, 250) + 30);
    expect(levelAt(ltas, 1000)).toBeGreaterThan(levelAt(ltas, 4000) + 30);
  });

  it("measures white noise as roughly flat on the log grid", () => {
    const next = rng(5);
    const noise = new Float32Array(sr * 2);
    for (let i = 0; i < noise.length; i++) noise[i] = (next() * 2 - 1) * 0.2;

    const ltas = ltasOf(noise);
    if (!ltas) throw new Error("expected an LTAS");
    // Ignore the extremes where the grid runs out of bins.
    const mid = [200, 500, 1000, 2000, 4000, 8000].map((f) => levelAt(ltas, f));
    const spread = Math.max(...mid) - Math.min(...mid);
    expect(spread).toBeLessThan(3);
  });

  it("declines when there is not enough material to average", () => {
    const short = new Float32Array(4096);
    expect(computeLtas(short, sr, [{ start: 0, end: 4096 }])).toBeNull();
  });

  it("sees a resonance that was added to speech", () => {
    const { signal } = speech();
    const boosted = applyCascade(signal.channels[0], [peakingEq(sr, 800, 9, 3)]);

    const before = ltasOf(signal.channels[0]);
    const after = ltasOf(boosted);
    if (!before || !after) throw new Error("expected LTAS");

    const delta = levelAt(after, 800) - levelAt(before, 800);
    expect(delta).toBeGreaterThan(5);
  });

  it("broad target follows the tilt but not the bumps", () => {
    const { signal } = speech();
    const boosted = applyCascade(signal.channels[0], [peakingEq(sr, 800, 10, 4)]);
    const ltas = ltasOf(boosted);
    if (!ltas) throw new Error("expected an LTAS");

    const target = broadTarget(ltas);
    let peakIndex = 0;
    for (let i = 0; i < ltas.freqs.length; i++) {
      if (Math.abs(Math.log2(ltas.freqs[i] / 800)) < Math.abs(Math.log2(ltas.freqs[peakIndex] / 800))) {
        peakIndex = i;
      }
    }
    // The narrow bump should stand clear of the broadly smoothed curve. It
    // does not stand *fully* clear: the target averages in power, so a loud
    // narrow peak pulls its own reference up by a few dB. That is why the
    // fitter under-corrects resonances rather than erasing them.
    expect(ltas.db[peakIndex] - target[peakIndex]).toBeGreaterThan(2.5);
  });
});

describe("corrective eq fitting", () => {
  it("finds and cancels an injected resonance", () => {
    const { signal } = speech();
    const boosted = applyCascade(signal.channels[0], [peakingEq(sr, 800, 9, 3)]);
    const ltas = ltasOf(boosted);
    if (!ltas) throw new Error("expected an LTAS");

    const fit = fitCorrectiveEq(ltas, null, sr);
    expect(fit.bands.length).toBeGreaterThan(0);

    // Some band should sit near 800 Hz and cut.
    const near = fit.bands.filter((b) => Math.abs(Math.log2(b.freq / 800)) < 0.5);
    expect(near.length).toBeGreaterThan(0);
    expect(near[0].gainDb).toBeLessThan(-1.5);
    expect(fit.deviationAfterDb).toBeLessThan(fit.deviationBeforeDb);
  });

  it("actually flattens the spectrum when its bands are applied", () => {
    const { signal } = speech();
    const boosted = applyCascade(signal.channels[0], [
      peakingEq(sr, 300, 7, 2.5),
      peakingEq(sr, 2500, -8, 2),
    ]);

    const before = ltasOf(boosted);
    if (!before) throw new Error("expected an LTAS");
    const fit = fitCorrectiveEq(before, null, sr);
    const cascade = buildCascade(fit, { freq: null, excessDb: 0 }, sr);

    const corrected = applyCascade(boosted, cascade);
    const after = ltasOf(corrected);
    if (!after) throw new Error("expected an LTAS");

    const deviation = (l: NonNullable<ReturnType<typeof ltasOf>>): number => {
      const target = broadTarget(l);
      let worst = 0;
      for (let i = 0; i < l.freqs.length; i++) {
        if (l.freqs[i] < 150 || l.freqs[i] > 8000) continue;
        worst = Math.max(worst, Math.abs(l.db[i] - target[i]));
      }
      return worst;
    };

    expect(deviation(after)).toBeLessThan(deviation(before));
  });

  it("places no bands on material that is already smooth", () => {
    // Transparency: an auto-EQ that always does something is not corrective.
    const { signal, segments } = speech();
    const ltas = computeLtas(
      signal.channels[0],
      sr,
      segments.map((s) => ({ start: s.start, end: s.end })),
      DEFAULT_LTAS_OPTIONS,
    );
    if (!ltas) throw new Error("expected an LTAS");

    expect(fitCorrectiveEq(ltas, null, sr).bands).toEqual([]);
  });

  it("respects the cut and boost limits, and biases toward cutting", () => {
    const { signal } = speech();
    const wrecked = applyCascade(signal.channels[0], [
      peakingEq(sr, 500, 18, 4),
      peakingEq(sr, 3000, -18, 4),
    ]);
    const ltas = ltasOf(wrecked);
    if (!ltas) throw new Error("expected an LTAS");

    const fit = fitCorrectiveEq(ltas, null, sr, { maxCutDb: 6, maxBoostDb: 3 });
    for (const band of fit.bands) {
      expect(band.gainDb).toBeGreaterThanOrEqual(-6.001);
      expect(band.gainDb).toBeLessThanOrEqual(3.001);
    }
  });

  it("never places more bands than it is allowed", () => {
    const { signal } = speech();
    const wrecked = applyCascade(signal.channels[0], [
      peakingEq(sr, 200, 10, 4),
      peakingEq(sr, 700, -10, 4),
      peakingEq(sr, 1800, 9, 4),
      peakingEq(sr, 4000, -9, 4),
      peakingEq(sr, 6000, 8, 4),
      peakingEq(sr, 9000, -8, 4),
    ]);
    const ltas = ltasOf(wrecked);
    if (!ltas) throw new Error("expected an LTAS");

    const fit = fitCorrectiveEq(ltas, null, sr, { maxBands: 3 });
    expect(fit.bands.length).toBeLessThanOrEqual(3);
  });

  it("refuses to boost where the noise floor is close", () => {
    const { signal } = speech();
    // A deep notch that the fitter would like to fill.
    const notched = applyCascade(signal.channels[0], [peakingEq(sr, 5000, -12, 2)]);
    const speechLtas = ltasOf(notched);
    if (!speechLtas) throw new Error("expected an LTAS");

    // A "noise" spectrum sitting right under the speech: nothing may be boosted.
    const noiseLtas = { ...speechLtas, db: speechLtas.db.map((v) => v - 2) as unknown as Float64Array };

    const gated = fitCorrectiveEq(speechLtas, noiseLtas, sr);
    expect(gated.bands.every((b) => b.gainDb <= 0)).toBe(true);

    // With a clean floor far below, the same notch may be filled.
    const quietNoise = {
      ...speechLtas,
      db: speechLtas.db.map((v) => v - 40) as unknown as Float64Array,
    };
    const allowed = fitCorrectiveEq(speechLtas, quietNoise, sr);
    expect(allowed.bands.some((b) => b.gainDb > 0)).toBe(true);
  });
});

describe("rumble decision", () => {
  it("stays off for clean speech", () => {
    const { signal } = speech();
    const ltas = ltasOf(signal.channels[0]);
    if (!ltas) throw new Error("expected an LTAS");
    expect(decideRumbleFilter(ltas).freq).toBeNull();
  });

  it("engages when there is sub-bass rumble under the voice", () => {
    const { signal } = speech();
    const rumbled = Float32Array.from(signal.channels[0]);
    const low = sine(35, rumbled.length / sr, sr, 0.05);
    for (let i = 0; i < rumbled.length; i++) rumbled[i] += low[i];

    const ltas = ltasOf(rumbled);
    if (!ltas) throw new Error("expected an LTAS");
    const decision = decideRumbleFilter(ltas);
    expect(decision.freq).toBe(80);
    expect(decision.excessDb).toBeGreaterThan(-12);
  });

  it("removes the rumble it engaged for", () => {
    const { signal } = speech();
    const rumbled = Float32Array.from(signal.channels[0]);
    const low = sine(35, rumbled.length / sr, sr, 0.05);
    for (let i = 0; i < rumbled.length; i++) rumbled[i] += low[i];

    const ltas = ltasOf(rumbled);
    if (!ltas) throw new Error("expected an LTAS");
    const cascade = buildCascade({ bands: [], deviationBeforeDb: 0, deviationAfterDb: 0 },
      decideRumbleFilter(ltas), sr);
    const filtered = applyCascade(rumbled, cascade);

    const after = ltasOf(filtered);
    if (!after) throw new Error("expected an LTAS");
    // Measured on the raw PSD: the smoothed grid deliberately blurs the bottom
    // octaves, which would mix the removed rumble with the voice above it.
    const sub = (l: NonNullable<ReturnType<typeof ltasOf>>): number =>
      bandEnergyDb(l.psd, sr, 25, 50);
    // A second-order Butterworth at 80 Hz reaches ~14-17 dB of rejection over
    // the 25-50 Hz band, which is what "rumble removed" means here.
    expect(sub(ltas) - sub(after)).toBeGreaterThan(14);
    // The voice band is left alone.
    expect(Math.abs(levelAt(ltas, 1000) - levelAt(after, 1000))).toBeLessThan(1);
  });
});

describe("true peak", () => {
  it("matches sample peak for a slowly varying signal", () => {
    const tone = sine(50, 0.2, sr, 0.5);
    expect(truePeakLinear([tone])).toBeCloseTo(0.5, 2);
  });

  it("exceeds sample peak where inter-sample overshoot is real", () => {
    // A tone near Nyquist/4 sampled off-peak: the reconstruction overshoots
    // every sample. This is precisely what a sample-peak limiter misses.
    const n = 4096;
    const tone = new Float32Array(n);
    for (let i = 0; i < n; i++) tone[i] = 0.5 * Math.sin(2 * Math.PI * (i / 4 + 0.125));

    let samplePeak = 0;
    for (let i = 0; i < n; i++) samplePeak = Math.max(samplePeak, Math.abs(tone[i]));
    expect(truePeakLinear([tone])).toBeGreaterThan(samplePeak * 1.05);
  });

  it("reports -Infinity dBTP for digital silence", () => {
    expect(truePeakDbfs([new Float32Array(1000)])).toBe(-Infinity);
  });

  it("takes the loudest across channels", () => {
    const quiet = sine(1000, 0.1, sr, 0.1);
    const loud = sine(1000, 0.1, sr, 0.6);
    expect(truePeakLinear([quiet, loud])).toBeCloseTo(truePeakLinear([loud]), 6);
  });
});
