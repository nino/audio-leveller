/**
 * Radix-2 FFT, just enough for spectral measurement.
 *
 * Written for the pipeline's actual needs — power spectra of windowed frames
 * (LTAS now, dynamic EQ later) — not as a general-purpose library. Sizes are
 * powers of two; frames are real-valued and processed as complex with zero
 * imaginary parts, which wastes a factor of two we happily pay for
 * simplicity's sake at analysis (not per-sample) rates.
 *
 * {@link createFftPlan} lifts the power-of-two restriction, because a trained
 * model does not get to choose its frame size: DeepFilterNet3 was trained on a
 * 960-point transform (20 ms at 48 kHz), and 960 = 2⁶·3·5.
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

/**
 * Factorise into radices this transform can handle. Returns null when `n` has
 * a prime factor above 5 — the caller gets a clear error rather than a plan
 * that silently computes the wrong thing.
 */
function factorise(n: number): number[] | null {
  const factors: number[] = [];
  let rest = n;
  for (const radix of [4, 2, 3, 5]) {
    while (rest % radix === 0) {
      factors.push(radix);
      rest /= radix;
    }
  }
  return rest === 1 ? factors : null;
}

export interface FftPlan {
  readonly size: number;
  /** In-place forward transform of interleaved [re, im, ...] of `size` points. */
  forward(data: Float64Array): void;
  /** In-place inverse, normalised by 1/size. */
  inverse(data: Float64Array): void;
}

/**
 * Mixed-radix complex FFT for any size whose prime factors are 2, 3 and 5.
 *
 * Plain recursive Cooley–Tukey: split the input into `r` interleaved
 * subsequences, transform each, then combine. The twiddle table and the
 * scratch buffers are built once per plan, so a plan reused across every frame
 * of a file allocates nothing per frame.
 *
 * Radix 4 is preferred over two radix-2 passes only because it halves the
 * recursion depth; correctness does not depend on the factor order, which is
 * what `test/fft.test.ts` checks by comparing several sizes against a direct
 * DFT.
 */
export function createFftPlan(size: number): FftPlan {
  if (!Number.isInteger(size) || size < 1) throw new Error("FFT size must be a positive integer");
  const planned = factorise(size);
  if (!planned) throw new Error(`FFT size ${size} has a prime factor above 5`);
  const factors: number[] = planned;

  // W_size^k for k = 0..size-1, shared by every level: a sub-transform of
  // length n uses stride size/n into this same table.
  const twiddle = new Float64Array(2 * size);
  for (let k = 0; k < size; k++) {
    const angle = (-2 * Math.PI * k) / size;
    twiddle[2 * k] = Math.cos(angle);
    twiddle[2 * k + 1] = Math.sin(angle);
  }

  const scratch = new Float64Array(2 * size);
  // One temporary per level, sized to that level's radix. A level's combine
  // step runs only after its children have returned, so levels never share.
  const temp = new Float64Array(2 * Math.max(1, ...factors));

  /**
   * Transform `n` points read from `inp` at `inpOff` with stride `stride`,
   * writing `n` contiguous points to `out` at `outOff`.
   */
  function rec(
    out: Float64Array,
    outOff: number,
    inp: Float64Array,
    inpOff: number,
    stride: number,
    n: number,
    level: number,
  ): void {
    if (n === 1) {
      out[2 * outOff] = inp[2 * inpOff];
      out[2 * outOff + 1] = inp[2 * inpOff + 1];
      return;
    }

    const radix = factors[level];
    const m = n / radix;
    for (let j = 0; j < radix; j++) {
      rec(out, outOff + j * m, inp, inpOff + j * stride, stride * radix, m, level + 1);
    }

    // X[k + q·m] = Σ_j W_n^{jk} · W_radix^{jq} · Y_j[k]
    const step = size / n; // W_n^i == twiddle[i · step]
    for (let k = 0; k < m; k++) {
      for (let j = 0; j < radix; j++) {
        const at = 2 * (outOff + j * m + k);
        const yRe = out[at];
        const yIm = out[at + 1];
        const w = 2 * ((j * k * step) % size);
        const wRe = twiddle[w];
        const wIm = twiddle[w + 1];
        temp[2 * j] = yRe * wRe - yIm * wIm;
        temp[2 * j + 1] = yRe * wIm + yIm * wRe;
      }
      const radixStep = size / radix; // W_radix^i == twiddle[i · radixStep]
      for (let q = 0; q < radix; q++) {
        let sumRe = 0;
        let sumIm = 0;
        for (let j = 0; j < radix; j++) {
          const w = 2 * ((j * q * radixStep) % size);
          const wRe = twiddle[w];
          const wIm = twiddle[w + 1];
          sumRe += temp[2 * j] * wRe - temp[2 * j + 1] * wIm;
          sumIm += temp[2 * j] * wIm + temp[2 * j + 1] * wRe;
        }
        const at = 2 * (outOff + q * m + k);
        out[at] = sumRe;
        out[at + 1] = sumIm;
      }
    }
  }

  function forward(data: Float64Array): void {
    if (data.length !== 2 * size) throw new Error(`plan is for ${size} points`);
    rec(scratch, 0, data, 0, 1, size, 0);
    data.set(scratch);
  }

  return {
    size,
    forward,
    inverse(data: Float64Array): void {
      for (let i = 0; i < size; i++) data[2 * i + 1] = -data[2 * i + 1];
      forward(data);
      const scale = 1 / size;
      for (let i = 0; i < size; i++) {
        data[2 * i] *= scale;
        data[2 * i + 1] = -data[2 * i + 1] * scale;
      }
    },
  };
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
