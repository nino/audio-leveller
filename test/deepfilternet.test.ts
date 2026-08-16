import { describe, it, expect } from "vitest";
import { createFftPlan } from "../src/dsp/fft";
import {
  analyse,
  applyModel,
  computeFeatures,
  DFN3_CONFIG,
  erbWidths,
  frameCount,
  normAlpha,
  runChunked,
  stagesFor,
  synthesise,
  vorbisWindow,
  type Features,
  type ModelOutput,
} from "../src/models/deepfilternet";
import { rng } from "../src/eval/signals";

const cfg = DFN3_CONFIG;
const bins = cfg.fftSize / 2 + 1;

/** Direct DFT, to check the mixed-radix transform against something obvious. */
function dft(data: Float64Array): Float64Array {
  const n = data.length / 2;
  const out = new Float64Array(2 * n);
  for (let k = 0; k < n; k++) {
    let re = 0;
    let im = 0;
    for (let t = 0; t < n; t++) {
      const a = (-2 * Math.PI * k * t) / n;
      const c = Math.cos(a);
      const s = Math.sin(a);
      re += data[2 * t] * c - data[2 * t + 1] * s;
      im += data[2 * t] * s + data[2 * t + 1] * c;
    }
    out[2 * k] = re;
    out[2 * k + 1] = im;
  }
  return out;
}

function noise(n: number, seed = 7): Float32Array {
  const next = rng(seed);
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) out[i] = (next() * 2 - 1) * 0.3;
  return out;
}

describe("mixed-radix fft", () => {
  // 960 is the size DeepFilterNet3 needs and 2^6·3·5, so every radix the plan
  // knows about is exercised by the sizes below.
  it.each([15, 45, 64, 100, 240, 960])("matches a direct DFT at n=%i", (n) => {
    const next = rng(n);
    const data = new Float64Array(2 * n);
    for (let i = 0; i < 2 * n; i++) data[i] = next() * 2 - 1;

    const expected = dft(data.slice());
    createFftPlan(n).forward(data);

    let worst = 0;
    for (let i = 0; i < 2 * n; i++) worst = Math.max(worst, Math.abs(data[i] - expected[i]));
    expect(worst).toBeLessThan(1e-9);
  });

  it("round-trips through its own inverse", () => {
    const n = 960;
    const next = rng(3);
    const original = new Float64Array(2 * n);
    for (let i = 0; i < 2 * n; i++) original[i] = next() * 2 - 1;

    const data = original.slice();
    const plan = createFftPlan(n);
    plan.forward(data);
    plan.inverse(data);

    let worst = 0;
    for (let i = 0; i < 2 * n; i++) worst = Math.max(worst, Math.abs(data[i] - original[i]));
    expect(worst).toBeLessThan(1e-12);
  });

  it("refuses a size it cannot factor rather than computing the wrong thing", () => {
    expect(() => createFftPlan(7 * 64)).toThrow(/prime factor/);
  });
});

describe("deepfilternet feature frontend", () => {
  it("lays out ERB bands covering exactly the spectrum", () => {
    const widths = erbWidths(cfg);
    expect(widths).toHaveLength(cfg.nbErb);
    expect(widths.reduce((a, b) => a + b, 0)).toBe(bins);
    // The min-width floor binds at the bottom, where an ERB is narrower than
    // a bin; bands must never be narrower than it.
    expect(Math.min(...widths)).toBe(cfg.minNbErbFreqs);
    // Widths are non-decreasing: ERB bandwidth grows with frequency.
    for (let i = 1; i < widths.length; i++) expect(widths[i]).toBeGreaterThanOrEqual(widths[i - 1]);
  });

  it("uses a window that sums to unity at 50 % overlap", () => {
    // Princen–Bradley: w[n]² + w[n + N/2]² = 1. This is what lets the same
    // window be applied on analysis and synthesis without a normalisation pass.
    const w = vorbisWindow(cfg.fftSize);
    const half = cfg.fftSize / 2;
    for (let i = 0; i < half; i++) {
      expect(w[i] * w[i] + w[i + half] * w[i + half]).toBeCloseTo(1, 12);
    }
  });

  it("reproduces upstream's rounded normalisation coefficient", () => {
    // exp(-0.01) is 0.990050, and libDF rounds it to three decimals. The whole
    // feature sequence is conditioned on this, so the rounding is copied
    // rather than corrected.
    expect(normAlpha(cfg)).toBe(0.99);
  });

  it("reconstructs a signal exactly through analysis and synthesis", () => {
    const samples = noise(cfg.sampleRate);
    const frames = frameCount(samples.length, cfg.hopSize);
    const back = synthesise(analyse(samples, frames, cfg).frames, samples.length, cfg);

    let worst = 0;
    for (let i = 0; i < samples.length; i++) worst = Math.max(worst, Math.abs(back[i] - samples[i]));
    expect(worst).toBeLessThan(1e-6);
  });

  it("produces features in the shapes and layout the graphs expect", () => {
    const samples = noise(cfg.sampleRate / 4);
    const frames = frameCount(samples.length, cfg.hopSize);
    const features = computeFeatures(analyse(samples, frames, cfg), cfg);

    expect(features.frames).toBe(frames);
    expect(features.erb).toHaveLength(frames * cfg.nbErb);
    // [1, 2, frames, nbDf] — real block then imaginary block, not interleaved.
    expect(features.spec).toHaveLength(2 * frames * cfg.nbDf);
    expect(features.erb.every(Number.isFinite)).toBe(true);
    expect(features.spec.every(Number.isFinite)).toBe(true);
  });
});

describe("chunked inference", () => {
  /**
   * A stand-in for the graphs that is causal and stateless, so a chunked run
   * must reproduce a whole-file one exactly. Its outputs are derived from the
   * features it was handed, which is the point: a slicing error — the wrong
   * ERB range, or the complex feature's two blocks assembled wrongly — changes
   * the values rather than merely their position, and the test catches it.
   */
  function fakeRunner(erb: Float32Array, spec: Float32Array, frames: number): ModelOutput {
    const { nbErb, nbDf, dfOrder } = cfg;
    const gains = new Float32Array(frames * nbErb);
    const coefs = new Float32Array(frames * nbDf * dfOrder * 2);
    const lsnr = new Float32Array(frames);
    for (let t = 0; t < frames; t++) {
      for (let b = 0; b < nbErb; b++) gains[t * nbErb + b] = erb[t * nbErb + b] * 2;
      for (let k = 0; k < nbDf; k++) {
        const re = spec[t * nbDf + k];
        const im = spec[frames * nbDf + t * nbDf + k];
        for (let o = 0; o < dfOrder; o++) {
          const at = 2 * ((t * nbDf + k) * dfOrder + o);
          coefs[at] = re + o;
          coefs[at + 1] = im - o;
        }
      }
      lsnr[t] = erb[t * nbErb] + spec[t * nbDf];
    }
    return { gains, coefs, lsnr };
  }

  function features(seconds: number): Features {
    const samples = noise(Math.round(seconds * cfg.sampleRate), 21);
    const frames = frameCount(samples.length, cfg.hopSize);
    return computeFeatures(analyse(samples, frames, cfg), cfg);
  }

  it("stitches chunks back into the same result as one pass", () => {
    const feats = features(12);
    expect(feats.frames).toBeGreaterThan(400);

    const whole = runChunked(feats, cfg, { chunkFrames: feats.frames, warmupFrames: 0 }, fakeRunner);
    const chunked = runChunked(feats, cfg, { chunkFrames: 137, warmupFrames: 61 }, fakeRunner);

    expect(chunked.gains).toEqual(whole.gains);
    expect(chunked.coefs).toEqual(whole.coefs);
    expect(chunked.lsnr).toEqual(whole.lsnr);
  });

  it("reports progress to completion", () => {
    const feats = features(12);
    const seen: number[] = [];
    runChunked(feats, cfg, { chunkFrames: 200, warmupFrames: 50 }, fakeRunner, (f) => seen.push(f));
    expect(seen.at(-1)).toBe(1);
  });
});

describe("applying the model", () => {
  const widths = erbWidths(cfg);

  /** A flat spectrogram, so any change is the model's doing. */
  function flat(frames: number): Float64Array[] {
    return Array.from({ length: frames }, () => {
      const f = new Float64Array(2 * bins);
      for (let k = 0; k < bins; k++) f[2 * k] = 1;
      return f;
    });
  }

  function model(frames: number, gain: number, lsnr: number): ModelOutput {
    return {
      gains: new Float32Array(frames * cfg.nbErb).fill(gain),
      // Identity-ish filter: only the tap aligned with the frame itself.
      coefs: new Float32Array(frames * cfg.nbDf * cfg.dfOrder * 2),
      lsnr: new Float32Array(frames).fill(lsnr),
    };
  }

  it("leaves bins below nbDf out of the ERB mask", () => {
    // The training graph keeps the masked spectrum only above nbDf, so the
    // network never learned useful gains below it. Applying them there is the
    // bug that turns this model into a speech destroyer.
    const frames = 8;
    const noisy = flat(frames);
    // lsnr above maxDbDfThresh: gains run, the deep filter does not.
    const out = applyModel(noisy, model(frames + 4, 0.25, 25), cfg, widths, 0);

    const frame = out.frames[3];
    for (let k = 0; k < cfg.nbDf; k++) expect(frame[2 * k]).toBeCloseTo(1, 9);
    for (let k = cfg.nbDf; k < bins; k++) expect(frame[2 * k]).toBeCloseTo(0.25, 6);
  });

  it("passes a frame through untouched when the model calls it clean", () => {
    const frames = 8;
    const out = applyModel(flat(frames), model(frames + 4, 0.25, 40), cfg, widths, 0);
    expect(out.framesLeftAlone).toBe(frames);
    for (let k = 0; k < bins; k++) expect(out.frames[2][2 * k]).toBeCloseTo(1, 12);
  });

  it("never attenuates a bin by more than the requested reduction", () => {
    // The contract the stage depends on: `reductionDb` is a target, and the
    // wet/dry mix is what bounds it. Here the model zeroes everything (local
    // SNR below minDbThresh), so every bin sits at exactly the limit.
    const frames = 8;
    const out = applyModel(flat(frames), model(frames + 4, 0, -30), cfg, widths, 12);

    for (const frame of out.frames) {
      for (let k = 0; k < bins; k++) {
        expect(20 * Math.log10(frame[2 * k])).toBeCloseTo(-12, 6);
      }
    }
  });

  it("gates on local SNR the way upstream does", () => {
    expect(stagesFor(-20, cfg)).toEqual({ gains: false, zeros: true, deepFilter: false });
    expect(stagesFor(0, cfg)).toEqual({ gains: true, zeros: false, deepFilter: true });
    expect(stagesFor(25, cfg)).toEqual({ gains: true, zeros: false, deepFilter: false });
    expect(stagesFor(40, cfg)).toEqual({ gains: false, zeros: false, deepFilter: false });
  });
});
