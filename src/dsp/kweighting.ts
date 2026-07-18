/**
 * ITU-R BS.1770 "K-weighting" pre-filter.
 *
 * K-weighting is a cascade of two biquad filters applied before the
 * mean-square loudness measurement:
 *   Stage 1 - a high-shelf "head" filter (~+4 dB shelf)
 *   Stage 2 - an RLB high-pass filter (rolls off very low frequencies)
 *
 * The coefficients are derived analytically for an arbitrary sample rate
 * using the same formulation as libebur128, so measurements are correct at
 * 44.1 kHz, 48 kHz, 96 kHz, etc. — not just the 48 kHz values printed in
 * the standard.
 */

export interface BiquadCoefficients {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

/** Stage 1: high-shelf boost centred at ~1681 Hz. */
export function shelvingFilter(sampleRate: number): BiquadCoefficients {
  const f0 = 1681.9744509555319;
  const G = 3.9998438533940324;
  const Q = 0.7071752369554193;

  const K = Math.tan((Math.PI * f0) / sampleRate);
  const Vh = Math.pow(10, G / 20);
  const Vb = Math.pow(Vh, 0.4996667741545416);

  const a0 = 1 + K / Q + K * K;
  return {
    b0: (Vh + (Vb * K) / Q + K * K) / a0,
    b1: (2 * (K * K - Vh)) / a0,
    b2: (Vh - (Vb * K) / Q + K * K) / a0,
    a1: (2 * (K * K - 1)) / a0,
    a2: (1 - K / Q + K * K) / a0,
  };
}

/** Stage 2: RLB high-pass at ~38 Hz. */
export function highPassFilter(sampleRate: number): BiquadCoefficients {
  const f0 = 38.13547087602444;
  const Q = 0.5003270373238773;

  const K = Math.tan((Math.PI * f0) / sampleRate);
  const a0 = 1 + K / Q + K * K;
  return {
    b0: 1,
    b1: -2,
    b2: 1,
    a1: (2 * (K * K - 1)) / a0,
    a2: (1 - K / Q + K * K) / a0,
  };
}

/**
 * Apply a single biquad (Direct Form I) to a signal, returning a new array.
 * The difference equation is:
 *   y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]
 */
export function applyBiquad(input: Float32Array, c: BiquadCoefficients): Float32Array {
  const out = new Float32Array(input.length);
  let x1 = 0;
  let x2 = 0;
  let y1 = 0;
  let y2 = 0;
  for (let n = 0; n < input.length; n++) {
    const x0 = input[n];
    const y0 = c.b0 * x0 + c.b1 * x1 + c.b2 * x2 - c.a1 * y1 - c.a2 * y2;
    out[n] = y0;
    x2 = x1;
    x1 = x0;
    y2 = y1;
    y1 = y0;
  }
  return out;
}

/** Apply the full two-stage K-weighting filter to a single channel. */
export function kWeight(channel: Float32Array, sampleRate: number): Float32Array {
  const stage1 = applyBiquad(channel, shelvingFilter(sampleRate));
  return applyBiquad(stage1, highPassFilter(sampleRate));
}
