/**
 * Level-domain dynamics: one gain, computed from the signal's own envelope and
 * smoothed over time.
 *
 * A compressor and a downward expander are the same machine pointed in
 * opposite directions – measure the level, look up a gain on a static curve,
 * smooth that gain, apply it – so both live here and differ only in the curve
 * and the two time constants. Keeping the machine shared means the envelope
 * detector, the smoother and the multi-channel behaviour are written and
 * tested once.
 *
 * Three decisions carry the quality:
 *
 * - **The gain is smoothed, not the envelope.** Smoothing the detector first
 *   and then looking up a gain makes the effective time constant depend on how
 *   far up the curve you are, which is why some compressors breathe. Measured
 *   against the reference this project was fitted to, smoothing the gain is
 *   also what that processor does: its gain trajectory fits a first-order
 *   smoother on the *gain* to within a few hundredths.
 *
 * - **One gain for every channel.** Detecting per channel and applying per
 *   channel would let a loud left channel duck while the right stayed put,
 *   which moves the stereo image in time with the speech. The detector sums
 *   channel power, so the image holds still.
 *
 * - **No make-up gain.** A compressor normally needs one, because pulling the
 *   peaks down makes everything quieter. Here the level stage runs afterwards
 *   and normalises to an exact loudness target, so make-up gain would be a
 *   number the leveller immediately takes back out. Leaving it off keeps the
 *   compressor's report honest: the gain it shows is the gain it applied.
 */

export interface DynamicsCurve {
  /** Gain to apply, in dB, for a detected level in dBFS. */
  (levelDb: number): number;
}

export interface DynamicsTiming {
  /** Time constant while the gain is *falling*, in ms. */
  downMs: number;
  /** Time constant while the gain is *rising*, in ms. */
  upMs: number;
  /**
   * Envelope detector window, in ms. Short enough to follow syllables, long
   * enough not to track individual pitch periods – at 100 Hz a window under
   * 10 ms would ripple at the fundamental and modulate the gain with it.
   */
  detectorMs: number;
}

export const DEFAULT_TIMING: DynamicsTiming = {
  downMs: 33,
  upMs: 168,
  detectorMs: 15,
};

export interface DynamicsResult {
  channels: Float32Array[];
  /** Gain actually applied, per sample, in dB. */
  gainDb: Float32Array;
  /** Largest attenuation applied, in dB (positive number). */
  maxReductionDb: number;
  /** Mean gain over samples where the detector saw signal, in dB. */
  meanGainDb: number;
  /** Fraction of those samples where the gain differed from unity by >0.1 dB. */
  activeFraction: number;
}

/** Level in dBFS of a sliding-RMS detector, one value per sample. */
function detectorLevels(
  channels: Float32Array[],
  sampleRate: number,
  detectorMs: number,
): Float64Array {
  const length = channels[0]?.length ?? 0;
  const window = Math.max(1, Math.round((detectorMs / 1000) * sampleRate));
  const levels = new Float64Array(length);

  // Running sum of summed-channel power. A boxcar rather than a one-pole so
  // the detector has a definite memory: a syllable leaves the window entirely
  // rather than trailing an exponential tail into the next one.
  let sum = 0;
  const history = new Float64Array(window);
  let at = 0;
  for (let i = 0; i < length; i++) {
    let power = 0;
    for (const ch of channels) power += ch[i] * ch[i];
    power /= channels.length;

    sum += power - history[at];
    history[at] = power;
    at = (at + 1) % window;

    const filled = Math.min(i + 1, window);
    const rms = Math.sqrt(Math.max(0, sum) / filled);
    levels[i] = rms > 1e-12 ? 20 * Math.log10(rms) : -240;
  }
  return levels;
}

/**
 * Apply a static gain curve with attack/release smoothing.
 *
 * The detector is delay-free with respect to the output, so a transient
 * arrives before the gain has finished moving – that is what an attack time
 * means, and pre-delaying the signal to hide it would turn the compressor into
 * a limiter. The true-peak limiter in the level stage is the thing that
 * catches what gets through, and it runs last for exactly this reason.
 */
export function applyDynamics(
  channels: Float32Array[],
  sampleRate: number,
  curve: DynamicsCurve,
  timing: DynamicsTiming = DEFAULT_TIMING,
): DynamicsResult {
  const length = channels[0]?.length ?? 0;
  const levels = detectorLevels(channels, sampleRate, timing.detectorMs);

  const coeff = (ms: number): number =>
    ms <= 0 ? 0 : Math.exp(-1 / ((ms / 1000) * sampleRate));
  const down = coeff(timing.downMs);
  const up = coeff(timing.upMs);

  const gainDb = new Float32Array(length);
  let gain = 0;
  let maxReductionDb = 0;
  let sumGain = 0;
  let counted = 0;
  let active = 0;

  for (let i = 0; i < length; i++) {
    const target = curve(levels[i]);
    // Falling gain uses the attack constant, rising gain the release one.
    const a = target < gain ? down : up;
    gain = a * gain + (1 - a) * target;
    gainDb[i] = gain;

    if (levels[i] > -120) {
      sumGain += gain;
      counted++;
      if (Math.abs(gain) > 0.1) active++;
    }
    if (-gain > maxReductionDb) maxReductionDb = -gain;
  }

  const out = channels.map((ch) => {
    const dst = new Float32Array(length);
    for (let i = 0; i < length; i++) dst[i] = ch[i] * 10 ** (gainDb[i] / 20);
    return dst;
  });

  return {
    channels: out,
    gainDb,
    maxReductionDb,
    meanGainDb: counted > 0 ? sumGain / counted : 0,
    activeFraction: counted > 0 ? active / counted : 0,
  };
}

export interface CompressorCurveOptions {
  /** Level above which gain reduction begins, in dBFS. */
  thresholdDb: number;
  /** Compression ratio. 1 is no compression. */
  ratio: number;
  /**
   * Width of the soft knee, in dB, centred on the threshold.
   *
   * A hard knee makes the gain a kinked function of level, and speech spends
   * most of its time near the threshold – so the kink is audible as the gain
   * flicking on and off across it. The reference processor's own curve bends
   * over roughly 10 dB rather than turning a corner.
   */
  kneeDb: number;
  /** Never attenuate by more than this, in dB. */
  maxReductionDb: number;
}

/** Downward compression above a threshold, with a quadratic soft knee. */
export function compressorCurve(options: CompressorCurveOptions): DynamicsCurve {
  const { thresholdDb, ratio, kneeDb, maxReductionDb } = options;
  const slope = 1 - 1 / Math.max(1e-6, ratio);
  const half = kneeDb / 2;

  return (levelDb: number): number => {
    const over = levelDb - thresholdDb;
    let reduction: number;
    if (kneeDb > 0 && over > -half && over < half) {
      // Quadratic interpolation across the knee: continuous in value and slope
      // at both ends, which is what stops the gain flickering at the threshold.
      const x = over + half;
      reduction = (slope * x * x) / (2 * kneeDb);
    } else {
      reduction = over <= 0 ? 0 : slope * over;
    }
    return -Math.min(reduction, maxReductionDb);
  };
}

export interface ExpanderCurveOptions {
  /** Level below which gain reduction begins, in dBFS. */
  thresholdDb: number;
  /** Expansion ratio. 1 is no expansion. */
  ratio: number;
  /** Width of the soft knee, in dB, centred on the threshold. */
  kneeDb: number;
  /** Never attenuate by more than this, in dB. */
  rangeDb: number;
}

/**
 * Downward expansion below a threshold – the quiet gets quieter.
 *
 * `rangeDb` is what keeps this from being a gate. An expander with unlimited
 * range drives the gaps between words to digital silence, and a recording
 * scrubbed to silence between words sounds broken in a way the noise never
 * did: the room disappears and reappears with every syllable. Bounding the
 * attenuation keeps the room present, just further down.
 */
export function expanderCurve(options: ExpanderCurveOptions): DynamicsCurve {
  const { thresholdDb, ratio, kneeDb, rangeDb } = options;
  const slope = Math.max(0, ratio - 1);
  const half = kneeDb / 2;

  return (levelDb: number): number => {
    const under = thresholdDb - levelDb;
    let reduction: number;
    if (kneeDb > 0 && under > -half && under < half) {
      const x = under + half;
      reduction = (slope * x * x) / (2 * kneeDb);
    } else {
      reduction = under <= 0 ? 0 : slope * under;
    }
    return -Math.min(reduction, rangeDb);
  };
}
