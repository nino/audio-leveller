/**
 * Band-limited sample-rate conversion (Kaiser-windowed sinc).
 *
 * The AI stages of the pipeline run at a fixed rate (48 kHz for
 * DeepFilterNet-class models), so material recorded at 44.1 kHz has to be
 * converted on the way in and back again on the way out. Doing that badly
 * undoes exactly the quality the rest of the chain is trying to protect, so
 * this is a proper windowed-sinc resampler rather than linear interpolation.
 *
 * The filter is precomputed once into a table indexed by distance from the
 * output position (in sinc zero-crossings) and linearly interpolated between
 * table entries. That keeps memory fixed regardless of the rate ratio — a
 * polyphase bank would need `toRate / gcd` phases, which explodes for
 * pathological pairs like 44101 → 48000.
 */

export interface ResampleOptions {
  /** Sinc zero-crossings kept either side of the output position. */
  zeros: number;
  /** Cutoff as a fraction of the lower Nyquist. Below 1 to leave transition room. */
  rolloff: number;
  /** Kaiser window shape. ~12 gives roughly -120 dB stopband attenuation. */
  beta: number;
}

export const DEFAULT_RESAMPLE_OPTIONS: ResampleOptions = {
  zeros: 32,
  rolloff: 0.95,
  beta: 12,
};

/** Table entries per sinc zero-crossing. */
const TABLE_PER_ZERO = 256;

/** Modified Bessel function of the first kind, order 0 (series expansion). */
function besselI0(x: number): number {
  let sum = 1;
  let term = 1;
  const half = x / 2;
  for (let k = 1; k < 64; k++) {
    term *= (half / k) * (half / k);
    sum += term;
    if (term < sum * 1e-16) break;
  }
  return sum;
}

function sinc(x: number): number {
  if (x === 0) return 1;
  const px = Math.PI * x;
  return Math.sin(px) / px;
}

interface FilterTable {
  /** h() sampled every 1 / TABLE_PER_ZERO zero-crossings. */
  table: Float64Array;
  /** Cutoff in cycles per *input* sample. */
  cutoff: number;
  /** Half the filter support, in input samples. */
  halfWidth: number;
}

function buildFilter(ratio: number, opts: ResampleOptions): FilterTable {
  // When downsampling the anti-alias cutoff must follow the *output* Nyquist.
  const cutoff = 0.5 * opts.rolloff * Math.min(1, ratio);
  const halfWidth = opts.zeros / (2 * cutoff);
  const length = opts.zeros * TABLE_PER_ZERO + 2;
  const table = new Float64Array(length);
  const i0beta = besselI0(opts.beta);

  for (let k = 0; k < length; k++) {
    // x is the distance from centre measured in zero-crossings.
    const x = k / TABLE_PER_ZERO;
    const u = x / opts.zeros;
    if (u >= 1) {
      table[k] = 0;
      continue;
    }
    const window = besselI0(opts.beta * Math.sqrt(1 - u * u)) / i0beta;
    table[k] = 2 * cutoff * sinc(x) * window;
  }

  return { table, cutoff, halfWidth };
}

/** Number of frames `resample` will produce for a given input length. */
export function resampledLength(length: number, fromRate: number, toRate: number): number {
  if (fromRate === toRate) return length;
  return Math.max(0, Math.round((length * toRate) / fromRate));
}

/**
 * Resample one channel. Samples outside the input are treated as zero, but
 * the per-output normalisation divides by the *full* tap sum (including those
 * out-of-range taps), so the signal fades naturally at the edges instead of
 * being extrapolated.
 */
function resampleChannel(
  input: Float32Array,
  outLength: number,
  ratio: number,
  filter: FilterTable,
): Float32Array {
  const { table, cutoff, halfWidth } = filter;
  const out = new Float32Array(outLength);
  const scale = 2 * cutoff * TABLE_PER_ZERO; // input samples -> table index
  const maxIndex = table.length - 2;
  const len = input.length;

  for (let m = 0; m < outLength; m++) {
    const centre = m / ratio;
    const first = Math.ceil(centre - halfWidth);
    const last = Math.floor(centre + halfWidth);

    let acc = 0;
    let weight = 0;
    for (let n = first; n <= last; n++) {
      const index = Math.abs(centre - n) * scale;
      const i = index | 0;
      if (i > maxIndex) continue;
      const frac = index - i;
      const h = table[i] + frac * (table[i + 1] - table[i]);
      weight += h;
      if (n >= 0 && n < len) acc += input[n] * h;
    }

    out[m] = weight !== 0 ? acc / weight : 0;
  }

  return out;
}

/**
 * Convert every channel from `fromRate` to `toRate`. Equal rates take a fast
 * path that copies rather than filtering, so a no-op conversion is exact.
 */
export function resample(
  channels: Float32Array[],
  fromRate: number,
  toRate: number,
  options: Partial<ResampleOptions> = {},
): Float32Array[] {
  if (fromRate <= 0 || toRate <= 0) throw new Error("Sample rates must be positive");
  if (fromRate === toRate) return channels.map((ch) => Float32Array.from(ch));

  const opts = { ...DEFAULT_RESAMPLE_OPTIONS, ...options };
  const ratio = toRate / fromRate;
  const filter = buildFilter(ratio, opts);

  return channels.map((ch) =>
    resampleChannel(ch, resampledLength(ch.length, fromRate, toRate), ratio, filter),
  );
}
