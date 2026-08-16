import { describe, it, expect } from "vitest";
import { fftComplex, ifftComplex } from "../src/dsp/fft";
import {
  stft,
  istft,
  processStft,
  scanStft,
  stftFrameCount,
  magnitudes,
  applyGains,
  binCount,
  sqrtHannWindow,
} from "../src/dsp/stft";
import { sine } from "./helpers";
import { rng } from "../src/eval/signals";

const sr = 48000;

/** Worst absolute difference between two channels. */
function maxDiff(a: Float32Array, b: Float32Array): number {
  let worst = 0;
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    worst = Math.max(worst, Math.abs(a[i] - b[i]));
  }
  return worst;
}

describe("inverse fft", () => {
  it("round-trips a random spectrum to machine precision", () => {
    const next = rng(11);
    const n = 256;
    const data = new Float64Array(2 * n);
    for (let i = 0; i < 2 * n; i++) data[i] = next() * 2 - 1;
    const original = Float64Array.from(data);

    fftComplex(data);
    ifftComplex(data);

    for (let i = 0; i < 2 * n; i++) expect(data[i]).toBeCloseTo(original[i], 10);
  });
});

describe("stft / istft", () => {
  it("reconstructs a signal perfectly when nothing is modified", () => {
    // The property everything else rests on: any audible difference after a
    // spectral stage must be the processing, never the transform.
    const signal = sine(440, 0.25, sr, 0.5);
    const rebuilt = istft(stft(signal));

    expect(rebuilt.length).toBe(signal.length);
    expect(maxDiff(signal, rebuilt)).toBeLessThan(1e-6);
  });

  it("reconstructs the very first and last samples, not just the middle", () => {
    // A naive overlap-add leaves the edges attenuated by the window taper.
    const next = rng(3);
    const signal = new Float32Array(8000);
    for (let i = 0; i < signal.length; i++) signal[i] = (next() * 2 - 1) * 0.4;

    const rebuilt = istft(stft(signal));
    const edge = 32;
    let worstEdge = 0;
    for (let i = 0; i < edge; i++) {
      worstEdge = Math.max(worstEdge, Math.abs(signal[i] - rebuilt[i]));
      const j = signal.length - 1 - i;
      worstEdge = Math.max(worstEdge, Math.abs(signal[j] - rebuilt[j]));
    }
    expect(worstEdge).toBeLessThan(1e-6);
  });

  it("reconstructs broadband noise, which has no forgiving structure", () => {
    const next = rng(7);
    const noise = new Float32Array(20000);
    for (let i = 0; i < noise.length; i++) noise[i] = (next() * 2 - 1) * 0.5;

    expect(maxDiff(noise, istft(stft(noise)))).toBeLessThan(1e-6);
  });

  it("works at other frame sizes and hops", () => {
    const signal = sine(1000, 0.2, sr, 0.4);
    for (const [frameSize, hopSize] of [
      [512, 128],
      [2048, 512],
      [1024, 512],
    ]) {
      const rebuilt = istft(stft(signal, { frameSize, hopSize }));
      expect(maxDiff(signal, rebuilt), `${frameSize}/${hopSize}`).toBeLessThan(1e-6);
    }
  });

  it("rejects frame sizes that are not powers of two", () => {
    expect(() => stft(new Float32Array(1000), { frameSize: 1000, hopSize: 250 })).toThrow(
      /power of two/,
    );
  });

  it("puts a tone's energy in the expected bin", () => {
    const frameSize = 1024;
    const binHz = sr / frameSize;
    const tone = sine(binHz * 100, 0.2, sr, 0.5);
    const analysis = stft(tone, { frameSize, hopSize: 256 });

    // A frame from the middle, clear of the padding.
    const mid = analysis.frames[Math.floor(analysis.frames.length / 2)];
    const mags = magnitudes(mid);
    let peak = 0;
    for (let k = 1; k < mags.length; k++) if (mags[k] > mags[peak]) peak = k;
    expect(peak).toBe(100);
  });

  it("applies per-bin gains and the change survives resynthesis", () => {
    const signal = sine(1000, 0.2, sr, 0.5);
    const analysis = stft(signal);
    const bins = binCount(analysis.frameSize);

    // Silence everything: the output must be silent, not merely quieter.
    const zeros = new Float64Array(bins);
    for (const frame of analysis.frames) applyGains(frame, zeros);

    const rebuilt = istft(analysis);
    let peak = 0;
    for (let i = 0; i < rebuilt.length; i++) peak = Math.max(peak, Math.abs(rebuilt[i]));
    expect(peak).toBeLessThan(1e-9);
  });

  it("halves amplitude when every bin is halved", () => {
    const signal = sine(700, 0.2, sr, 0.5);
    const analysis = stft(signal);
    const half = new Float64Array(binCount(analysis.frameSize)).fill(0.5);
    for (const frame of analysis.frames) applyGains(frame, half);

    const rebuilt = istft(analysis);
    const mid = Math.floor(signal.length / 2);
    for (let i = mid; i < mid + 200; i++) {
      expect(rebuilt[i]).toBeCloseTo(signal[i] * 0.5, 5);
    }
  });
});

describe("sqrtHannWindow", () => {
  it("squares to a Hann window that sums to a constant at 75% overlap", () => {
    const n = 256;
    const w = sqrtHannWindow(n);
    const hop = n / 4;

    for (let i = 0; i < hop; i++) {
      let sum = 0;
      for (let k = 0; k * hop + i < n; k++) {
        const at = i + k * hop;
        sum += w[at] * w[at];
      }
      expect(sum).toBeCloseTo(2, 6);
    }
  });
});

describe("streaming stft", () => {
  const next = rng(4242);
  const samples = new Float32Array(sr * 3);
  for (let i = 0; i < samples.length; i++) samples[i] = (next() * 2 - 1) * 0.4;

  it("agrees with stft on how many frames there are", () => {
    for (const options of [{}, { frameSize: 2048, hopSize: 512 }, { frameSize: 512, hopSize: 128 }]) {
      expect(stftFrameCount(samples.length, options)).toBe(stft(samples, options).frames.length);
    }
  });

  it("reconstructs bit-identically to stft followed by istft", () => {
    // The whole point of the streaming path is that it is the same computation
    // with bounded memory, so anything less than bit-identical is a bug.
    for (const options of [{}, { frameSize: 2048, hopSize: 512 }, { frameSize: 512, hopSize: 128 }]) {
      const reference = istft(stft(samples, options));
      const streamed = processStft(samples, options, () => {});
      expect(Array.from(streamed)).toEqual(Array.from(reference));
    }
  });

  it("applies the same modification as editing materialised frames", () => {
    const gains = (spectrum: Float64Array): void => {
      // Halve everything above the midpoint, an arbitrary but frame-dependent edit.
      for (let k = spectrum.length / 4; k < spectrum.length / 2; k++) {
        spectrum[2 * k] *= 0.5;
        spectrum[2 * k + 1] *= 0.5;
      }
    };

    const analysis = stft(samples);
    for (const frame of analysis.frames) gains(frame);
    const reference = istft(analysis);
    const streamed = processStft(samples, {}, gains);

    expect(Array.from(streamed)).toEqual(Array.from(reference));
  });

  it("visits every frame in order when only scanning", () => {
    const seen: number[] = [];
    const count = scanStft(samples, {}, (_spectrum, f) => seen.push(f));
    expect(count).toBe(stftFrameCount(samples.length));
    expect(seen).toEqual([...Array(count).keys()]);
  });

  it("holds memory that does not grow with the file", () => {
    // The regression this exists for: a twenty-minute recording needed about
    // two gigabytes for the frame array alone, and the render spent its time
    // swapping. Ten times the audio must not mean ten times the footprint.
    const long = new Float32Array(sr * 30);
    for (let i = 0; i < long.length; i++) long[i] = (next() * 2 - 1) * 0.2;

    global.gc?.();
    const before = process.memoryUsage().heapUsed;
    processStft(long, {}, () => {});
    const growth = process.memoryUsage().heapUsed - before;

    // The materialised path would need ~57 MB of frames for this signal.
    expect(growth).toBeLessThan(20e6);
  });
});
