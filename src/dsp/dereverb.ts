/**
 * Dereverberation by weighted prediction error (WPE).
 *
 * Reverberation is the room convolving the voice with its own impulse
 * response. The *late* part of that response — everything past the first few
 * tens of milliseconds — is what makes a recording sound distant, and it has a
 * property worth exploiting: it is a linear function of the signal's own past.
 * So for each frequency bin, predict the current frame from frames a short
 * delay back, and subtract what the prediction accounts for. What is left is
 * the direct sound plus early reflections.
 *
 * The weighting is the idea in the name. Ordinary least squares would be
 * dominated by the loudest frames, which are exactly the ones where the direct
 * sound is strongest and reverberation matters least. Weighting each frame by
 * the inverse of the *desired* signal's power there concentrates the fit on the
 * frames that are mostly tail. That power is not known in advance, so it is
 * estimated from the current output and the whole thing iterated a few times.
 *
 * Why this rather than a trained enhancer: WPE cannot invent speech. It only
 * ever subtracts a linear prediction, so its failure mode is leaving
 * reverberation behind, not fabricating detail that was never spoken. For a
 * podcast where the speaker's actual voice is the product, that is the right
 * trade — and it needs no weights, so it runs everywhere.
 *
 * The prediction delay is what protects the direct sound: without it the filter
 * would happily predict (and therefore cancel) the speech itself.
 */

import { stft, istft, binCount, type StftFrames } from "./stft";

export interface DereverbOptions {
  /** Prediction filter length, in frames. Longer reaches further into the tail. */
  taps: number;
  /**
   * Frames to skip before the prediction starts. This is what keeps the direct
   * sound and early reflections intact — they carry the intelligibility, and a
   * filter allowed to predict them would cancel the voice along with the room.
   */
  delay: number;
  /** How many times to re-estimate the desired-signal power and refit. */
  iterations: number;
  frameSize: number;
  hopSize: number;
  /** Diagonal loading, relative to the mean power, for a stable solve. */
  regularisation: number;
}

/**
 * Defaults chosen by sweeping frame size, delay and tap count against both a
 * reverberant recording and a dry one — because the failure that matters is
 * damage to material that never needed treating.
 *
 * The frame size is four times the pipeline's usual 1024, and that is the
 * whole story. WPE assumes the *desired* signal is uncorrelated across the
 * prediction delay, and speech violates that badly at short delays: at 1024/256
 * a delay of two frames reaches back 10 ms, about one pitch period for a male
 * voice, so the filter predicts the voice's own harmonic structure and
 * subtracts it. Measured: dry speech came back at 1 dB SI-SDR — destroyed. At
 * 4096/1024 with a delay of three frames the filter reaches back 64 ms, past
 * the pitch period and past the vocal tract's ringing, and only the room's tail
 * is left to predict. The same dry recording comes back at 20 dB.
 */
export const DEFAULT_DEREVERB_OPTIONS: DereverbOptions = {
  taps: 20,
  delay: 3,
  iterations: 3,
  frameSize: 4096,
  hopSize: 1024,
  regularisation: 1e-4,
};

/**
 * Solve a small Hermitian system `A g = b` in complex arithmetic by Gaussian
 * elimination with partial pivoting. Matrices are stored row-major as
 * interleaved [re, im].
 */
function solveComplex(a: Float64Array, b: Float64Array, n: number): Float64Array | null {
  const m = Float64Array.from(a);
  const x = Float64Array.from(b);

  const at = (row: number, col: number): number => 2 * (row * n + col);

  for (let col = 0; col < n; col++) {
    let pivotRow = col;
    let best = 0;
    for (let row = col; row < n; row++) {
      const re = m[at(row, col)];
      const im = m[at(row, col) + 1];
      const magnitude = re * re + im * im;
      if (magnitude > best) {
        best = magnitude;
        pivotRow = row;
      }
    }
    if (best < 1e-30) return null;

    if (pivotRow !== col) {
      for (let k = 0; k < n; k++) {
        const i = at(col, k);
        const j = at(pivotRow, k);
        let t = m[i];
        m[i] = m[j];
        m[j] = t;
        t = m[i + 1];
        m[i + 1] = m[j + 1];
        m[j + 1] = t;
      }
      let t = x[2 * col];
      x[2 * col] = x[2 * pivotRow];
      x[2 * pivotRow] = t;
      t = x[2 * col + 1];
      x[2 * col + 1] = x[2 * pivotRow + 1];
      x[2 * pivotRow + 1] = t;
    }

    const pre = m[at(col, col)];
    const pim = m[at(col, col) + 1];
    const pmag = pre * pre + pim * pim;

    for (let row = col + 1; row < n; row++) {
      const rre = m[at(row, col)];
      const rim = m[at(row, col) + 1];
      // factor = m[row][col] / m[col][col]
      const fre = (rre * pre + rim * pim) / pmag;
      const fim = (rim * pre - rre * pim) / pmag;
      if (fre === 0 && fim === 0) continue;

      for (let k = col; k < n; k++) {
        const cre = m[at(col, k)];
        const cim = m[at(col, k) + 1];
        m[at(row, k)] -= fre * cre - fim * cim;
        m[at(row, k) + 1] -= fre * cim + fim * cre;
      }
      const bre = x[2 * col];
      const bim = x[2 * col + 1];
      x[2 * row] -= fre * bre - fim * bim;
      x[2 * row + 1] -= fre * bim + fim * bre;
    }
  }

  // Back substitution.
  for (let row = n - 1; row >= 0; row--) {
    let sre = x[2 * row];
    let sim = x[2 * row + 1];
    for (let k = row + 1; k < n; k++) {
      const mre = m[at(row, k)];
      const mim = m[at(row, k) + 1];
      const xre = x[2 * k];
      const xim = x[2 * k + 1];
      sre -= mre * xre - mim * xim;
      sim -= mre * xim + mim * xre;
    }
    const dre = m[at(row, row)];
    const dim = m[at(row, row) + 1];
    const dmag = dre * dre + dim * dim;
    if (dmag < 1e-30) return null;
    x[2 * row] = (sre * dre + sim * dim) / dmag;
    x[2 * row + 1] = (sim * dre - sre * dim) / dmag;
  }

  return x;
}

/** Run WPE over one bin of an STFT, writing the dereverberated result back. */
function dereverbBin(
  frames: Float64Array[],
  bin: number,
  opts: DereverbOptions,
): void {
  const frameCount = frames.length;
  const { taps, delay } = opts;
  if (frameCount <= taps + delay + 2) return;

  const re = (t: number): number => frames[t][2 * bin];
  const im = (t: number): number => frames[t][2 * bin + 1];

  // Desired-signal power per frame; starts as the observation's own power.
  const power = new Float64Array(frameCount);
  let meanPower = 0;
  for (let t = 0; t < frameCount; t++) {
    power[t] = re(t) * re(t) + im(t) * im(t);
    meanPower += power[t];
  }
  meanPower /= Math.max(1, frameCount);
  if (meanPower <= 0) return;

  // Floor the weighting power well above zero. The 1/power weighting is the
  // whole idea, but near-silent frames would otherwise carry effectively
  // infinite weight and drag the fit to a filter that subtracts far more than
  // the room put in — and because each iteration re-estimates power from its
  // own output, that runs away rather than settling.
  const floor = meanPower * 1e-3;
  const matrix = new Float64Array(2 * taps * taps);
  const vector = new Float64Array(2 * taps);
  const historyRe = new Float64Array(taps);
  const historyIm = new Float64Array(taps);
  const outRe = new Float64Array(frameCount);
  const outIm = new Float64Array(frameCount);

  for (let iteration = 0; iteration < opts.iterations; iteration++) {
    matrix.fill(0);
    vector.fill(0);

    // Weighted correlation of the delayed history with itself, and with the
    // current frame. The 1/power weighting is what focuses the fit on frames
    // that are mostly tail rather than mostly direct sound.
    for (let t = delay + taps; t < frameCount; t++) {
      const weight = 1 / Math.max(power[t], floor);

      // The tap history, read once. The double loop below is 20x20, and
      // reading `re(t - delay - j)` inside it meant 840 accessor calls per
      // frame — each one an array-of-arrays indirection through a closure — to
      // fetch the same twenty complex numbers over and over.
      for (let i = 0; i < taps; i++) {
        const ti = t - delay - i;
        historyRe[i] = re(ti);
        historyIm[i] = im(ti);
      }
      const yRe = re(t);
      const yIm = im(t);

      for (let i = 0; i < taps; i++) {
        const ire = historyRe[i];
        const iim = historyIm[i];

        // R is Hermitian — R[j][i] is the conjugate of R[i][j] — so only the
        // upper triangle is computed and the rest mirrored. That is exact
        // rather than approximate: IEEE multiplication commutes and negation
        // is lossless, so the mirrored entry is the same bits the full loop
        // would have accumulated.
        for (let j = i; j < taps; j++) {
          const jre = historyRe[j];
          const jim = historyIm[j];
          // R[i][j] = x_i * conj(x_j)
          const real = weight * (ire * jre + iim * jim);
          const imag = weight * (iim * jre - ire * jim);
          const idx = 2 * (i * taps + j);
          matrix[idx] += real;
          matrix[idx + 1] += imag;
          if (j !== i) {
            const mirror = 2 * (j * taps + i);
            matrix[mirror] += real;
            matrix[mirror + 1] -= imag;
          }
        }

        // r[i] = x_i * conj(y_t)
        vector[2 * i] += weight * (ire * yRe + iim * yIm);
        vector[2 * i + 1] += weight * (iim * yRe - ire * yIm);
      }
    }

    // Diagonal loading: the system is near-singular wherever the signal is
    // quiet, and an unregularised solve there produces enormous filters that
    // subtract far more than the room put in.
    const load = meanPower * opts.regularisation * frameCount;
    for (let i = 0; i < taps; i++) matrix[2 * (i * taps + i)] += load;

    const filter = solveComplex(matrix, vector, taps);
    if (!filter) return;

    // Subtract the predicted late reverberation.
    for (let t = 0; t < frameCount; t++) {
      let predRe = 0;
      let predIm = 0;
      if (t >= delay + taps) {
        for (let i = 0; i < taps; i++) {
          const ti = t - delay - i;
          const xre = re(ti);
          const xim = im(ti);
          const gre = filter[2 * i];
          const gim = filter[2 * i + 1];
          // g^H x  =>  conj(g) * x
          predRe += gre * xre + gim * xim;
          predIm += gre * xim - gim * xre;
        }
      }
      outRe[t] = re(t) - predRe;
      outIm[t] = im(t) - predIm;
    }

    // Re-estimate the desired-signal power for the next pass.
    for (let t = 0; t < frameCount; t++) {
      power[t] = outRe[t] * outRe[t] + outIm[t] * outIm[t];
    }
  }

  for (let t = 0; t < frameCount; t++) {
    frames[t][2 * bin] = outRe[t];
    frames[t][2 * bin + 1] = outIm[t];
  }
}

export interface DereverbResult {
  channels: Float32Array[];
  /** Bins that were actually processed (short files leave some untouched). */
  binsProcessed: number;
}

/** Dereverberate every channel independently. */
export function dereverb(
  channels: Float32Array[],
  options: Partial<DereverbOptions> = {},
): DereverbResult {
  const opts = { ...DEFAULT_DEREVERB_OPTIONS, ...options };
  const bins = binCount(opts.frameSize);
  let binsProcessed = 0;

  const out = channels.map((samples) => {
    const analysis: StftFrames = stft(samples, {
      frameSize: opts.frameSize,
      hopSize: opts.hopSize,
    });

    for (let bin = 0; bin < bins; bin++) {
      dereverbBin(analysis.frames, bin, opts);
      binsProcessed++;
    }

    return istft(analysis);
  });

  return { channels: out, binsProcessed: channels.length > 0 ? binsProcessed / channels.length : 0 };
}
