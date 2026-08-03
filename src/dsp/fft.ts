/**
 * Radix-2 FFT, just enough for spectral measurement.
 *
 * Written for the pipeline's actual needs — power spectra of windowed frames
 * (LTAS now, dynamic EQ later) — not as a general-purpose library. Sizes are
 * powers of two; frames are real-valued and processed as complex with zero
 * imaginary parts, which wastes a factor of two we happily pay for
 * simplicity's sake at analysis (not per-sample) rates.
 */

/** In-place complex FFT of interleaved [re, im, re, im, ...]. */
export function fftComplex(data: Float64Array): void {
  const n = data.length / 2;
  if (n < 2) return;
  if ((n & (n - 1)) !== 0) throw new Error("FFT size must be a power of two");

  // Bit-reversal permutation.
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) {
      const ir = 2 * i;
      const jr = 2 * j;
      let t = data[ir];
      data[ir] = data[jr];
      data[jr] = t;
      t = data[ir + 1];
      data[ir + 1] = data[jr + 1];
      data[jr + 1] = t;
    }
  }

  for (let len = 2; len <= n; len <<= 1) {
    const angle = (-2 * Math.PI) / len;
    const wRe = Math.cos(angle);
    const wIm = Math.sin(angle);
    for (let i = 0; i < n; i += len) {
      let curRe = 1;
      let curIm = 0;
      for (let k = 0; k < len / 2; k++) {
        const even = 2 * (i + k);
        const odd = 2 * (i + k + len / 2);
        const oddRe = data[odd] * curRe - data[odd + 1] * curIm;
        const oddIm = data[odd] * curIm + data[odd + 1] * curRe;
        data[odd] = data[even] - oddRe;
        data[odd + 1] = data[even + 1] - oddIm;
        data[even] += oddRe;
        data[even + 1] += oddIm;
        const nextRe = curRe * wRe - curIm * wIm;
        curIm = curRe * wIm + curIm * wRe;
        curRe = nextRe;
      }
    }
  }
}

/** Periodic Hann window, the right variant for overlapped spectral averaging. */
export function hannWindow(size: number): Float64Array {
  const w = new Float64Array(size);
  for (let i = 0; i < size; i++) w[i] = 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / size);
  return w;
}

/**
 * Power spectrum of one real windowed frame: |X[k]|² for k = 0..n/2,
 * normalised by the window's energy so overlapping frames average cleanly.
 * The frame is multiplied by `window` on the way in.
 */
export function powerSpectrum(
  frame: Float32Array,
  offset: number,
  window: Float64Array,
): Float64Array {
  const n = window.length;
  const buffer = new Float64Array(2 * n);
  let windowEnergy = 0;
  for (let i = 0; i < n; i++) {
    buffer[2 * i] = frame[offset + i] * window[i];
    windowEnergy += window[i] * window[i];
  }

  fftComplex(buffer);

  const out = new Float64Array(n / 2 + 1);
  const scale = 1 / (windowEnergy || 1);
  for (let k = 0; k <= n / 2; k++) {
    const re = buffer[2 * k];
    const im = buffer[2 * k + 1];
    out[k] = (re * re + im * im) * scale;
  }
  return out;
}

/** In-place inverse complex FFT, normalised by 1/n. */
export function ifftComplex(data: Float64Array): void {
  const n = data.length / 2;
  if (n < 1) return;

  // conj -> forward -> conj -> scale is the standard inverse-by-forward trick.
  for (let i = 0; i < n; i++) data[2 * i + 1] = -data[2 * i + 1];
  fftComplex(data);
  const scale = 1 / n;
  for (let i = 0; i < n; i++) {
    data[2 * i] *= scale;
    data[2 * i + 1] = -data[2 * i + 1] * scale;
  }
}
