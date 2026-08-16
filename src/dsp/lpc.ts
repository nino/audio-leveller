/**
 * Linear prediction: fitting an all-pole model to a short window of audio,
 * and using it to reconstruct samples that were destroyed.
 *
 * Speech is well described over 20-50 ms by an autoregressive model — each
 * sample is roughly a fixed linear combination of the ones before it, because
 * that is what a resonant vocal tract does. Two things follow, and the
 * de-clicker uses both:
 *
 *   - A sample that badly violates the model is suspicious. That is detection.
 *   - Missing samples can be recovered by choosing the values that make the
 *     signal fit the model best. That is repair.
 *
 * Coefficient convention throughout: `a[0] === 1`, and the prediction error is
 *
 *     e[n] = sum(k = 0..p) a[k] * x[n - k]
 *
 * so the classic "predictor" coefficients are `-a[1..p]`. Keeping the leading 1
 * explicit makes the interpolation maths below much easier to follow.
 */

/**
 * Autocorrelation of a window, lags 0..order.
 * `r[0]` gets a small ridge so a silent or near-constant window still yields a
 * solvable (if meaningless) system rather than dividing by zero.
 */
export function autocorrelation(
  samples: Float32Array,
  start: number,
  end: number,
  order: number,
): Float64Array {
  const r = new Float64Array(order + 1);
  const from = Math.max(0, start);
  const to = Math.min(samples.length, end);

  for (let lag = 0; lag <= order; lag++) {
    let sum = 0;
    for (let i = from + lag; i < to; i++) sum += samples[i] * samples[i - lag];
    r[lag] = sum;
  }

  // White-noise correction: lifts the model off a singular solution and stops
  // it chasing an exact fit to one window's noise.
  r[0] = r[0] * 1.0001 + 1e-12;
  return r;
}

/**
 * Levinson-Durbin recursion: autocorrelation to AR coefficients.
 * Returns `a` of length order+1 with `a[0] === 1`.
 */
export function levinson(r: Float64Array, order: number): Float64Array {
  const a = new Float64Array(order + 1);
  a[0] = 1;
  let error = r[0];
  if (error <= 0) return a;

  const temp = new Float64Array(order + 1);
  for (let i = 1; i <= order; i++) {
    let acc = r[i];
    for (let j = 1; j < i; j++) acc += a[j] * r[i - j];

    const reflection = -acc / error;
    // Reflection coefficients outside (-1, 1) mean the recursion has gone
    // unstable — usually a degenerate window. Stop with what we have.
    if (!Number.isFinite(reflection) || Math.abs(reflection) >= 1) break;

    for (let j = 1; j < i; j++) temp[j] = a[j] + reflection * a[i - j];
    for (let j = 1; j < i; j++) a[j] = temp[j];
    a[i] = reflection;

    error *= 1 - reflection * reflection;
    if (error <= 0) break;
  }

  return a;
}

/** Fit an AR model of the given order to `samples[start, end)`. */
export function fitAr(
  samples: Float32Array,
  start: number,
  end: number,
  order: number,
): Float64Array {
  return levinson(autocorrelation(samples, start, end, order), order);
}

/**
 * Autocorrelation of the coefficient vector itself: `ra[m] = sum_k a[k]a[k+m]`.
 * This is what turns the interpolation normal equations into something with
 * only p+1 distinct values.
 */
export function coefficientAutocorrelation(a: Float64Array): Float64Array {
  const p = a.length - 1;
  const ra = new Float64Array(p + 1);
  for (let m = 0; m <= p; m++) {
    let sum = 0;
    for (let k = 0; k + m <= p; k++) sum += a[k] * a[k + m];
    ra[m] = sum;
  }
  return ra;
}

/** Solve a small dense symmetric system by Gaussian elimination with pivoting. */
function solve(matrix: Float64Array, rhs: Float64Array, n: number): Float64Array | null {
  const m = Float64Array.from(matrix);
  const x = Float64Array.from(rhs);

  for (let col = 0; col < n; col++) {
    // Partial pivot.
    let pivotRow = col;
    let best = Math.abs(m[col * n + col]);
    for (let row = col + 1; row < n; row++) {
      const value = Math.abs(m[row * n + col]);
      if (value > best) {
        best = value;
        pivotRow = row;
      }
    }
    if (best < 1e-18) return null; // singular; caller leaves the gap alone

    if (pivotRow !== col) {
      for (let k = 0; k < n; k++) {
        const swap = m[col * n + k];
        m[col * n + k] = m[pivotRow * n + k];
        m[pivotRow * n + k] = swap;
      }
      const swap = x[col];
      x[col] = x[pivotRow];
      x[pivotRow] = swap;
    }

    const pivot = m[col * n + col];
    for (let row = col + 1; row < n; row++) {
      const factor = m[row * n + col] / pivot;
      if (factor === 0) continue;
      for (let k = col; k < n; k++) m[row * n + k] -= factor * m[col * n + k];
      x[row] -= factor * x[col];
    }
  }

  // Back substitution.
  for (let row = n - 1; row >= 0; row--) {
    let sum = x[row];
    for (let k = row + 1; k < n; k++) sum -= m[row * n + k] * x[k];
    x[row] = sum / m[row * n + row];
  }

  return x;
}

/**
 * Least-squares AR interpolation (Janssen): replace `samples[gapStart, gapEnd)`
 * with the values that minimise the model's total prediction error, given the
 * surrounding samples.
 *
 * Minimising `J = sum_n e[n]^2` over the unknown samples gives, for each
 * unknown index m,
 *
 *     sum_j ra[|m - j|] * x[j] = 0
 *
 * over every j within p of m. Splitting that into unknowns (left-hand side)
 * and known neighbours (right-hand side) is the linear system solved here.
 * Unlike a crude fade or a linear bridge, this reconstructs the resonance that
 * was there: across a few milliseconds of speech the result is inaudible.
 *
 * Writes in place. Returns false and leaves the signal untouched if the system
 * is singular or the gap runs past the model's reach at the signal edges.
 */
export function interpolateGap(
  samples: Float32Array,
  gapStart: number,
  gapEnd: number,
  a: Float64Array,
): boolean {
  const p = a.length - 1;
  const gapLength = gapEnd - gapStart;
  if (gapLength <= 0) return false;

  // The solve needs p known samples of context on each side.
  if (gapStart - p < 0 || gapEnd + p > samples.length) return false;

  const ra = coefficientAutocorrelation(a);
  const matrix = new Float64Array(gapLength * gapLength);
  const rhs = new Float64Array(gapLength);

  for (let i = 0; i < gapLength; i++) {
    const m = gapStart + i;

    for (let j = 0; j < gapLength; j++) {
      const lag = Math.abs(m - (gapStart + j));
      matrix[i * gapLength + j] = lag <= p ? ra[lag] : 0;
    }

    // Known neighbours move to the right-hand side, negated.
    let sum = 0;
    for (let j = m - p; j <= m + p; j++) {
      if (j >= gapStart && j < gapEnd) continue; // unknown, stays on the left
      if (j < 0 || j >= samples.length) continue;
      sum += ra[Math.abs(m - j)] * samples[j];
    }
    rhs[i] = -sum;
  }

  const solution = solve(matrix, rhs, gapLength);
  if (!solution) return false;
  for (let i = 0; i < gapLength; i++) {
    if (!Number.isFinite(solution[i])) return false;
  }

  for (let i = 0; i < gapLength; i++) samples[gapStart + i] = solution[i];
  return true;
}
