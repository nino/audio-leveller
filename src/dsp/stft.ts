/**
 * Short-time Fourier transform with weighted overlap-add (WOLA).
 *
 * Any stage that changes a signal's spectrum frame by frame — the denoiser
 * now, the dynamic EQ later — needs to get back to a waveform without seams.
 * The requirement is *perfect reconstruction*: with the spectrum left alone,
 * analysis followed by synthesis must return the input sample for sample, so
 * that any difference heard afterwards is the processing and not the transform.
 *
 * The arrangement here is the standard one for modifying magnitudes: a
 * square-root Hann window on both analysis and synthesis, hopping a quarter of
 * the frame. Their product is a Hann window, and Hann at 75 % overlap sums to a
 * constant — so the overlap-add divides that constant out exactly. Windowing
 * only on analysis would be simpler and would ring: a modified frame no longer
 * tapers to zero at its edges, and the discontinuity lands in the output.
 */

import { fftComplex, ifftComplex } from "./fft";

export interface StftOptions {
  /** Frame length in samples. Must be a power of two. */
  frameSize: number;
  /** Hop between frames. `frameSize / 4` gives the 75 % overlap WOLA wants. */
  hopSize: number;
}

export const DEFAULT_STFT_OPTIONS: StftOptions = {
  frameSize: 1024,
  hopSize: 256,
};

/** Square-root of a periodic Hann window. */
export function sqrtHannWindow(size: number): Float64Array {
  const w = new Float64Array(size);
  for (let i = 0; i < size; i++) {
    w[i] = Math.sqrt(0.5 - 0.5 * Math.cos((2 * Math.PI * i) / size));
  }
  return w;
}

export interface StftFrames {
  /** Per frame, interleaved [re, im] for bins 0..frameSize/2. */
  frames: Float64Array[];
  frameSize: number;
  hopSize: number;
  /** Length of the signal that produced these frames. */
  length: number;
}

/** Number of bins a frame carries (0 .. Nyquist inclusive). */
export const binCount = (frameSize: number): number => frameSize / 2 + 1;

/**
 * Analyse a channel into overlapping windowed spectra.
 *
 * The signal is padded by one frame at each end so the first and last samples
 * get the same overlap treatment as the middle — without it, a de-noiser's
 * first 20 ms would be attenuated by the window taper alone.
 */
export function stft(
  samples: Float32Array,
  options: Partial<StftOptions> = {},
): StftFrames {
  const { frameSize, hopSize } = { ...DEFAULT_STFT_OPTIONS, ...options };
  if ((frameSize & (frameSize - 1)) !== 0) throw new Error("frameSize must be a power of two");
  if (frameSize % hopSize !== 0) throw new Error("frameSize must be a multiple of hopSize");

  const window = sqrtHannWindow(frameSize);
  const bins = binCount(frameSize);
  const frames: Float64Array[] = [];

  const padded = samples.length + 2 * frameSize;
  const buffer = new Float64Array(2 * frameSize);

  for (let start = 0; start + frameSize <= padded; start += hopSize) {
    for (let i = 0; i < frameSize; i++) {
      // `start` runs over the padded timeline; shift back to the real signal.
      const at = start + i - frameSize;
      const value = at >= 0 && at < samples.length ? samples[at] : 0;
      buffer[2 * i] = value * window[i];
      buffer[2 * i + 1] = 0;
    }

    fftComplex(buffer);

    const spectrum = new Float64Array(2 * bins);
    for (let k = 0; k < bins; k++) {
      spectrum[2 * k] = buffer[2 * k];
      spectrum[2 * k + 1] = buffer[2 * k + 1];
    }
    frames.push(spectrum);
  }

  return { frames, frameSize, hopSize, length: samples.length };
}

/**
 * Rebuild a channel from (possibly modified) spectra.
 *
 * The normalisation is the sum of the squared window across overlapping hops,
 * accumulated per output sample rather than assumed constant — so the taper at
 * the very start and end is divided out correctly too, instead of leaving the
 * first and last frames quiet.
 */
export function istft(analysis: StftFrames): Float32Array {
  const { frames, frameSize, hopSize, length } = analysis;
  const window = sqrtHannWindow(frameSize);
  const bins = binCount(frameSize);

  const padded = length + 2 * frameSize;
  const accumulator = new Float64Array(padded);
  const normalisation = new Float64Array(padded);
  const buffer = new Float64Array(2 * frameSize);

  frames.forEach((spectrum, frameIndex) => {
    // Rebuild the full spectrum from the half kept by `stft`, using the
    // conjugate symmetry a real signal guarantees.
    for (let k = 0; k < bins; k++) {
      buffer[2 * k] = spectrum[2 * k];
      buffer[2 * k + 1] = spectrum[2 * k + 1];
    }
    for (let k = bins; k < frameSize; k++) {
      const mirror = frameSize - k;
      buffer[2 * k] = spectrum[2 * mirror];
      buffer[2 * k + 1] = -spectrum[2 * mirror + 1];
    }

    ifftComplex(buffer);

    const start = frameIndex * hopSize;
    for (let i = 0; i < frameSize; i++) {
      const at = start + i;
      if (at >= padded) break;
      accumulator[at] += buffer[2 * i] * window[i];
      normalisation[at] += window[i] * window[i];
    }
  });

  const out = new Float32Array(length);
  for (let i = 0; i < length; i++) {
    const at = i + frameSize; // undo the analysis padding
    const norm = normalisation[at];
    out[i] = norm > 1e-9 ? accumulator[at] / norm : 0;
  }
  return out;
}

/** Magnitudes of one frame's bins. */
export function magnitudes(spectrum: Float64Array, out?: Float64Array): Float64Array {
  const bins = spectrum.length / 2;
  const result = out ?? new Float64Array(bins);
  for (let k = 0; k < bins; k++) {
    const re = spectrum[2 * k];
    const im = spectrum[2 * k + 1];
    result[k] = Math.sqrt(re * re + im * im);
  }
  return result;
}

/** Scale a frame's bins by per-bin real gains, in place. */
export function applyGains(spectrum: Float64Array, gains: Float64Array): void {
  for (let k = 0; k < gains.length; k++) {
    spectrum[2 * k] *= gains[k];
    spectrum[2 * k + 1] *= gains[k];
  }
}

/**
 * Number of analysis frames {@link stft} would produce for a signal.
 *
 * Worth knowing before allocating anything: at the pipeline's usual 1024/256
 * a twenty-minute recording has a quarter of a million frames.
 */
export function stftFrameCount(length: number, options: Partial<StftOptions> = {}): number {
  const { frameSize, hopSize } = { ...DEFAULT_STFT_OPTIONS, ...options };
  const padded = length + 2 * frameSize;
  return Math.floor((padded - frameSize) / hopSize) + 1;
}

/**
 * Analyse, modify and resynthesise one frame at a time.
 *
 * {@link stft} materialises every frame and hands back the lot, which is
 * convenient and costs memory proportional to the file: at 1024/256 each frame
 * is 8 KB, so a twenty-minute recording needs two gigabytes for the frames
 * alone, another for the inverse transform's accumulators, and the machine
 * spends the render swapping rather than computing. This does the same work
 * with a fixed-size overlap-add buffer, so memory is O(frameSize) no matter how
 * long the file is.
 *
 * `transform` is handed each frame's spectrum to modify in place, exactly as a
 * caller would modify `stft(...).frames[i]`. The output is bit-identical to
 * `istft` on the same modified frames: the accumulation order per output sample
 * is unchanged, because frames still arrive in increasing order and each sample
 * is finished — and emitted — as soon as the last frame overlapping it has been
 * added.
 */
export function processStft(
  samples: Float32Array,
  options: Partial<StftOptions>,
  transform: (spectrum: Float64Array, frameIndex: number) => void,
): Float32Array {
  const { frameSize, hopSize } = { ...DEFAULT_STFT_OPTIONS, ...options };
  if ((frameSize & (frameSize - 1)) !== 0) throw new Error("frameSize must be a power of two");
  if (frameSize % hopSize !== 0) throw new Error("frameSize must be a multiple of hopSize");

  const window = sqrtHannWindow(frameSize);
  const bins = binCount(frameSize);
  const length = samples.length;
  const frames = stftFrameCount(length, { frameSize, hopSize });

  const buffer = new Float64Array(2 * frameSize);
  const spectrum = new Float64Array(2 * bins);
  // Overlap-add state, indexed modulo frameSize. A sample stops receiving
  // contributions once the frame starting at it has been added, so exactly
  // `hopSize` slots retire per frame and are immediately reusable.
  const acc = new Float64Array(frameSize);
  const nrm = new Float64Array(frameSize);
  const out = new Float32Array(length);

  for (let f = 0; f < frames; f++) {
    const start = f * hopSize;

    for (let i = 0; i < frameSize; i++) {
      // `start` runs over the padded timeline; shift back to the real signal.
      const at = start + i - frameSize;
      const value = at >= 0 && at < length ? samples[at] : 0;
      buffer[2 * i] = value * window[i];
      buffer[2 * i + 1] = 0;
    }

    fftComplex(buffer);
    for (let k = 0; k < bins; k++) {
      spectrum[2 * k] = buffer[2 * k];
      spectrum[2 * k + 1] = buffer[2 * k + 1];
    }

    transform(spectrum, f);

    // Rebuild the full spectrum from the half kept, using the conjugate
    // symmetry a real signal guarantees.
    for (let k = 0; k < bins; k++) {
      buffer[2 * k] = spectrum[2 * k];
      buffer[2 * k + 1] = spectrum[2 * k + 1];
    }
    for (let k = bins; k < frameSize; k++) {
      const mirror = frameSize - k;
      buffer[2 * k] = spectrum[2 * mirror];
      buffer[2 * k + 1] = -spectrum[2 * mirror + 1];
    }
    ifftComplex(buffer);

    for (let i = 0; i < frameSize; i++) {
      const slot = (start + i) % frameSize;
      acc[slot] += buffer[2 * i] * window[i];
      nrm[slot] += window[i] * window[i];
    }

    // Everything below the next frame's start is now complete.
    for (let at = start; at < start + hopSize; at++) {
      const slot = at % frameSize;
      const index = at - frameSize; // undo the analysis padding
      if (index >= 0 && index < length) {
        out[index] = nrm[slot] > 1e-9 ? acc[slot] / nrm[slot] : 0;
      }
      acc[slot] = 0;
      nrm[slot] = 0;
    }
  }

  return out;
}

/**
 * Walk a signal's spectra without resynthesising, for a pass that only
 * measures — a noise profile, a long-term average. Same bounded memory.
 */
export function scanStft(
  samples: Float32Array,
  options: Partial<StftOptions>,
  visit: (spectrum: Float64Array, frameIndex: number) => void,
): number {
  const { frameSize, hopSize } = { ...DEFAULT_STFT_OPTIONS, ...options };
  const window = sqrtHannWindow(frameSize);
  const bins = binCount(frameSize);
  const length = samples.length;
  const frames = stftFrameCount(length, { frameSize, hopSize });

  const buffer = new Float64Array(2 * frameSize);
  const spectrum = new Float64Array(2 * bins);

  for (let f = 0; f < frames; f++) {
    const start = f * hopSize;
    for (let i = 0; i < frameSize; i++) {
      const at = start + i - frameSize;
      const value = at >= 0 && at < length ? samples[at] : 0;
      buffer[2 * i] = value * window[i];
      buffer[2 * i + 1] = 0;
    }
    fftComplex(buffer);
    for (let k = 0; k < bins; k++) {
      spectrum[2 * k] = buffer[2 * k];
      spectrum[2 * k + 1] = buffer[2 * k + 1];
    }
    visit(spectrum, f);
  }

  return frames;
}
