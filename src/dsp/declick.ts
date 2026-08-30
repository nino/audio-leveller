/**
 * De-clicker: find impulsive damage and rebuild it from its surroundings.
 *
 * Detection runs on the *linear-prediction residual*. A short window of speech
 * is well described by an AR model (see `lpc.ts`), so a click – which owes
 * nothing to the samples around it – leaves a spike in the residual far larger
 * than anything speech produces.
 *
 * The awkward part of that idea, and the reason naive AR de-clickers smear
 * damage across a whole model order, is that one corrupt sample at m pollutes
 * the forward residual for the next p samples: the predictor keeps feeding on
 * the bad value. Thresholding the forward residual alone therefore flags
 * [m, m+p] and "repairs" p samples of perfectly good audio.
 *
 * So this detector requires the *backward* residual to agree. Running the same
 * model in reverse, a corrupt sample at m pollutes [m-p, m]. Where both are
 * large is exactly where the damage is:
 *
 *     forward large:   [m1, m2 + p]
 *     backward large:  [m1 - p, m2]
 *     both:            [m1, m2]        <- the actual burst
 *
 * The threshold is set from the median absolute deviation of the residual
 * rather than its standard deviation, because the clicks themselves would
 * inflate a standard deviation and hide behind it.
 *
 * Repair is least-squares AR interpolation, which reconstructs the resonance
 * that was there rather than bridging or fading across the hole.
 */

import { fitAr, interpolateGap } from "./lpc";

export interface DeclickOptions {
  /** AR model order. ~32 captures speech formants at 44.1-48 kHz. */
  order: number;
  /**
   * Analysis block length. The model is re-fitted and the detection threshold
   * re-estimated per block.
   *
   * Short matters more than it looks. Voiced speech is driven by a glottal
   * pulse every pitch period – impulsive excitation, five to fifteen
   * milliseconds apart, which is exactly what an impulsive-outlier detector
   * fires on. Over a 50 ms block those pulses are a minority of the samples and
   * sit above the median, so the threshold lets them through as "clicks" and
   * the repair replaces real glottal pulses with interpolated mush. Over a
   * block comparable to one pitch period they dominate their own statistics and
   * lift the threshold above themselves. Measured on the eval corpus: 50 ms
   * blocks give ~2 false positives per second of clean speech, 10 ms blocks
   * give none, and both still catch every injected click.
   */
  blockSec: number;
  /** Detection threshold, in robust standard deviations of the residual. */
  thresholdSigma: number;
  /** Longest burst to treat as a click. Longer events are more likely to be
   *  real transients, and are left alone. */
  maxBurstSec: number;
  /** Bursts closer together than this are merged into one repair. */
  mergeGapSamples: number;
  /** Widen each burst by this much before repairing. Real clicks decay: the
   *  first sample or two tower over the threshold and the tail ducks under it,
   *  so repairing only the detected run leaves the tail behind. Two samples
   *  covers the tail of a typical short click; the cost is four extra
   *  interpolated samples per burst, in material the model fits well. */
  dilateSamples: number;
  /** Discard a whole analysis block's detections when they cover more than
   *  this fraction of it. Clicks are rare by definition – a block where the
   *  residual is over threshold this often is a transient the model doesn't
   *  fit (a door slam, dense crackle), and interpolating chunks of it would
   *  rewrite real audio. */
  maxBlockDensity: number;
  /** Abort the whole stage if detection covers more than this fraction of the
   *  file – that means the threshold is wrong for this material, and doing
   *  nothing is much safer than rewriting it. */
  maxRepairFraction: number;
  /**
   * Pulse-train veto. Voiced speech is excited by a glottal pulse every pitch
   * period, and in the LPC residual those pulses *are* impulsive outliers –
   * a robust per-block threshold sits between them and flags them, and on
   * real speech that comes to ~20 "clicks" a second, every one of them a
   * repair that replaces a real pulse with interpolated mush. Short blocks
   * (see `blockSec`) reduce this but do not cure it.
   *
   * What separates a click from a pulse is not its size against the residual
   * floor but its size against the *neighbouring pulses*: a pulse has
   * comparable neighbours one pitch period away, a click towers over them.
   * So a candidate is vetoed when the residual within `pulseLagMinSec` to
   * `pulseLagMaxSec` on either side (one to two pitch periods, 50–400 Hz)
   * reaches `pulseVetoRatio` of its own peak. At 0.3 a click has to be ~10 dB
   * more impulsive than the voice's own excitation around it; one that is
   * not is masked by that excitation anyway.
   *
   * Measured on a real 44.1 kHz podcast recording (clean, no clicks): the
   * detector alone repaired ~780 "clicks" per minute of speech, all glottal
   * pulses; with the veto, ~20. Injected clicks at 0.3× the waveform peak
   * were still caught 28/30, at 0.5× 30/30. Judge this on real recordings:
   * the synthetic eval voice is more impulsive than a real one even after
   * its glottal return phase was added, so there the margin is thinner.
   */
  pulseVetoRatio: number;
  pulseLagMinSec: number;
  pulseLagMaxSec: number;
}

export const DEFAULT_DECLICK_OPTIONS: DeclickOptions = {
  order: 32,
  blockSec: 0.01,
  thresholdSigma: 6,
  maxBurstSec: 0.002,
  mergeGapSamples: 4,
  dilateSamples: 2,
  maxBlockDensity: 0.05,
  maxRepairFraction: 0.02,
  pulseVetoRatio: 0.3,
  pulseLagMinSec: 0.0025,
  pulseLagMaxSec: 0.02,
};

export interface ClickBurst {
  channel: number;
  start: number;
  end: number;
  /** How far above the threshold the worst sample sat, in robust sigmas. */
  peakSigma: number;
  repaired: boolean;
}

export interface DeclickResult {
  channels: Float32Array[];
  bursts: ClickBurst[];
  /** Bursts that were detected and successfully rebuilt. */
  repaired: number;
  samplesRepaired: number;
  repairedFraction: number;
  /**
   * True when detection covered more than `maxRepairFraction` and the stage
   * declined to touch the audio at all.
   */
  aborted: boolean;
  /** Candidates dropped by the pulse-train veto (see `pulseVetoRatio`). */
  vetoed: number;
}

/** Median of a copy; the input is left alone. */
function median(values: Float64Array, length: number): number {
  if (length === 0) return 0;
  const sorted = Float64Array.prototype.slice.call(values, 0, length).sort();
  const mid = length >> 1;
  return length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Residual magnitude below which a block is treated as having no usable scale. */
const MIN_SIGMA = 1e-7;

interface Detection {
  start: number;
  end: number;
  peakSigma: number;
  /** Absolute peak of the forward residual over the burst. */
  peakAbs: number;
  /** Index of the analysis block this burst sits in, for the repair model. */
  block: number;
}

/**
 * Find click bursts in one channel. Returns the detections plus the per-block
 * AR models, so repair can reuse the model that did the detecting.
 */
function detect(
  samples: Float32Array,
  sampleRate: number,
  options: DeclickOptions,
): { detections: Detection[]; models: Float64Array[]; vetoed: number } {
  const { order, thresholdSigma } = options;
  const blockSamples = Math.max(order * 4, Math.round(options.blockSec * sampleRate));
  const maxBurst = Math.max(1, Math.round(options.maxBurstSec * sampleRate));
  const length = samples.length;

  const models: Float64Array[] = [];
  const detections: Detection[] = [];

  // Coarse envelope of |forward residual| (max per ~1 ms bin) plus which block
  // each bin came from, for the pulse-train veto below.
  const binSamples = Math.max(1, Math.round(0.001 * sampleRate));
  const envelope = new Float32Array(Math.ceil(length / binSamples));
  const envelopeBlock = new Int32Array(envelope.length).fill(-1);

  // The residuals need `order` samples of context each side, so the first and
  // last `order` samples can't be examined. Clicks that land there are rare
  // and repairing them without context would be guesswork.
  const scratch = new Float64Array(blockSamples);

  for (let blockStart = 0, block = 0; blockStart < length; blockStart += blockSamples, block++) {
    const blockEnd = Math.min(length, blockStart + blockSamples);

    // Fit on the block plus a margin, so the model isn't starved at the edges.
    const a = fitAr(
      samples,
      Math.max(0, blockStart - order),
      Math.min(length, blockEnd + order),
      order,
    );
    models.push(a);

    const from = Math.max(order, blockStart);
    const to = Math.min(length - order, blockEnd);
    if (to <= from) continue;

    // Forward and backward prediction error over the block.
    const forward = new Float64Array(to - from);
    const backward = new Float64Array(to - from);
    for (let n = from; n < to; n++) {
      let f = 0;
      let b = 0;
      for (let k = 0; k <= order; k++) {
        f += a[k] * samples[n - k];
        b += a[k] * samples[n + k];
      }
      forward[n - from] = f;
      backward[n - from] = b;
      const af = Math.abs(f);
      const bin = Math.floor(n / binSamples);
      if (af > envelope[bin]) {
        envelope[bin] = af;
        envelopeBlock[bin] = block;
      }
    }

    // Robust scale from the forward residual. Median absolute deviation, not
    // standard deviation: the clicks would inflate the latter and hide in it.
    const count = to - from;
    for (let i = 0; i < count; i++) scratch[i] = Math.abs(forward[i]);
    const sigma = 1.4826 * median(scratch, count);
    if (sigma < MIN_SIGMA) continue;

    const threshold = thresholdSigma * sigma;

    // Where forward and backward residuals *both* exceed the threshold.
    const blockDetections: Detection[] = [];
    let runStart = -1;
    let runPeak = 0;
    for (let i = 0; i <= count; i++) {
      const hit =
        i < count && Math.abs(forward[i]) > threshold && Math.abs(backward[i]) > threshold;

      if (hit) {
        if (runStart < 0) {
          runStart = i;
          runPeak = 0;
        }
        runPeak = Math.max(runPeak, Math.abs(forward[i]));
      } else if (runStart >= 0) {
        const start = from + runStart;
        const end = from + i;
        // Long events are more likely to be real transients than damage.
        if (end - start <= maxBurst) {
          blockDetections.push({ start, end, peakSigma: runPeak / sigma, peakAbs: runPeak, block });
        }
        runStart = -1;
      }
    }

    // Density guard: clicks are rare. A block peppered with over-threshold
    // runs is a transient the model doesn't fit, not a cluster of clicks –
    // drop all of it rather than interpolating chunks of real audio.
    const flagged = blockDetections.reduce((sum, d) => sum + (d.end - d.start), 0);
    if (flagged <= options.maxBlockDensity * (to - from)) {
      for (const d of blockDetections) detections.push(d);
    }
  }

  // Pulse-train veto: a candidate with a comparable residual peak one pitch
  // period away on either side is a glottal pulse, not a click. Two sources
  // of "neighbour", because each alone has a blind spot:
  //
  //  - other candidates (runs where forward and backward residual agree),
  //    which glottal pulses are – but only the cycles that crossed threshold;
  //  - the raw residual envelope, which sees every cycle – but a click
  //    corrupts the AR model of its own block and inflates that block's
  //    residual everywhere, so bins from the candidate's own block are
  //    ignored, otherwise the click would hide behind its own pollution.
  const lagMin = Math.round(options.pulseLagMinSec * sampleRate);
  const lagMax = Math.round(options.pulseLagMaxSec * sampleRate);
  const envelopeMax = (fromSample: number, toSample: number, ownBlock: number): number => {
    const b0 = Math.max(0, Math.ceil(fromSample / binSamples));
    const b1 = Math.min(envelope.length, Math.floor(toSample / binSamples));
    let m = 0;
    for (let b = b0; b < b1; b++) {
      if (envelopeBlock[b] !== ownBlock && envelope[b] > m) m = envelope[b];
    }
    return m;
  };
  const kept: Detection[] = [];
  let vetoed = 0;
  let lo = 0;
  for (let i = 0; i < detections.length; i++) {
    const d = detections[i];
    const bar = options.pulseVetoRatio * d.peakAbs;
    let comparable =
      envelopeMax(d.start - lagMax, d.start - lagMin, d.block) >= bar ||
      envelopeMax(d.end + lagMin, d.end + lagMax, d.block) >= bar;
    while (lo < i && detections[lo].end + lagMax < d.start) lo++;
    for (let j = lo; !comparable && j < detections.length; j++) {
      if (j === i) continue;
      const o = detections[j];
      if (o.start > d.end + lagMax) break;
      const gap = o.start >= d.end ? o.start - d.end : d.start - o.end;
      if (gap >= lagMin && gap <= lagMax && o.peakAbs >= bar) comparable = true;
    }
    if (comparable) vetoed++;
    else kept.push(d);
  }

  return { detections: kept, models, vetoed };
}

/**
 * Merge detections that are close enough to be one event, then drop merged
 * events that are too long to be clicks.
 *
 * The order matters. Merging *with* a length cap would carve a long dense
 * event (a door slam, a burst of crackle-like noise) into many maximum-length
 * "clicks" and repair them all – rewriting something that was never a click.
 * Merging first lets the event show its real extent, and the length filter
 * then discards it whole.
 */
function mergeDetections(detections: Detection[], gap: number, maxBurst: number): Detection[] {
  if (detections.length === 0) return [];
  const merged: Detection[] = [{ ...detections[0] }];

  for (let i = 1; i < detections.length; i++) {
    const current = detections[i];
    const last = merged[merged.length - 1];
    if (current.start - last.end <= gap) {
      last.end = current.end;
      last.peakSigma = Math.max(last.peakSigma, current.peakSigma);
    } else {
      merged.push({ ...current });
    }
  }

  return merged.filter((d) => d.end - d.start <= maxBurst);
}

/**
 * Detect and repair clicks across every channel. Channels are handled
 * independently: damage often hits one side only, and repairing the intact
 * channel to match would be inventing signal.
 */
export function declick(
  channels: Float32Array[],
  sampleRate: number,
  options: Partial<DeclickOptions> = {},
): DeclickResult {
  const opts = { ...DEFAULT_DECLICK_OPTIONS, ...options };
  const maxBurst = Math.max(1, Math.round(opts.maxBurstSec * sampleRate));
  const length = channels[0]?.length ?? 0;

  const bursts: ClickBurst[] = [];
  const perChannel: { detections: Detection[]; models: Float64Array[] }[] = [];
  let detectedSamples = 0;
  let vetoed = 0;

  for (const samples of channels) {
    const found = detect(samples, sampleRate, opts);
    vetoed += found.vetoed;
    const merged = mergeDetections(found.detections, opts.mergeGapSamples, maxBurst);
    perChannel.push({ detections: merged, models: found.models });
    for (const d of merged) detectedSamples += d.end - d.start;
  }

  const totalSamples = length * Math.max(1, channels.length);
  const detectedFraction = totalSamples > 0 ? detectedSamples / totalSamples : 0;

  // Too much of the file flagged means the threshold is wrong for this
  // material. Rewriting 2% of every channel on a bad hypothesis is far worse
  // than leaving the clicks in, so decline and say so.
  if (detectedFraction > opts.maxRepairFraction) {
    return {
      channels: channels.map((ch) => Float32Array.from(ch)),
      bursts: [],
      repaired: 0,
      samplesRepaired: 0,
      repairedFraction: 0,
      aborted: true,
      vetoed,
    };
  }

  const out = channels.map((ch) => Float32Array.from(ch));
  let repaired = 0;
  let samplesRepaired = 0;
  const fitHalf = Math.max(opts.order * 4, Math.round((opts.blockSec * sampleRate) / 2));

  perChannel.forEach(({ detections, models }, channel) => {
    const samples = out[channel];
    for (const detection of detections) {
      const start = Math.max(0, detection.start - opts.dilateSamples);
      const end = Math.min(length, detection.end + opts.dilateSamples);
      const model = models[detection.block] ?? models[models.length - 1];

      let ok = model ? interpolateGap(samples, start, end, model) : false;

      // Refinement pass. The first repair used the model that did the
      // detecting, which was fitted on a block *containing the click* – in
      // quiet audio the click dominates that block's autocorrelation utterly
      // (three samples at click amplitude carry orders of magnitude more
      // energy than 50 ms of noise floor), so the model describes the click,
      // not the audio, and the repair inherits the error. With the click now
      // gone, refit on the repaired neighbourhood and interpolate again using
      // a model of the actual signal.
      if (ok) {
        const refit = fitAr(
          samples,
          Math.max(0, start - fitHalf),
          Math.min(length, end + fitHalf),
          opts.order,
        );
        ok = interpolateGap(samples, start, end, refit) || ok;
        repaired++;
        samplesRepaired += end - start;
      }
      bursts.push({
        channel,
        start,
        end,
        peakSigma: detection.peakSigma,
        repaired: ok,
      });
    }
  });

  return {
    channels: out,
    bursts,
    repaired,
    samplesRepaired,
    repairedFraction: totalSamples > 0 ? samplesRepaired / totalSamples : 0,
    aborted: false,
    vetoed,
  };
}
