/**
 * Objective metrics for the evaluation harness.
 *
 * The design rule here: every metric must be able to say a stage made things
 * *worse*, not just better. A harness that only measures improvement will
 * happily report that a denoiser which eats consonants is working perfectly.
 *
 * The one to watch as the chain grows is `snrGainDb` — programme-to-floor
 * distance, in minus out. A pure gain change (what the leveller does) moves
 * the programme and the floor together and must leave it at 0. A denoiser has
 * to move it positive. Anything that moves it negative is amplifying noise.
 */

import { Analyzer } from "../pipeline/analysis";
import type { Signal } from "../pipeline/types";
import { integratedLoudness, loudnessOfRange, preFilter } from "../dsp/loudness";
import { computeLtas, broadTarget } from "../dsp/ltas";
import { truePeakDbfs } from "../dsp/truepeak";
import { reverbDecayMs } from "../dsp/reverbtime";

// Re-exported so eval code has one place to import metrics from.
export { reverbDecayMs };
import type { SpeechSegment } from "./signals";

/** Metric keys are free-form so stages can add their own; values are always dB-ish. */
export type Metrics = Record<string, number>;

/** Channels laid end to end, for whole-signal correlations. */
function flatten(signal: Signal): Float32Array {
  if (signal.channels.length === 1) return signal.channels[0];
  const out = new Float32Array(signal.channels.length * signal.length);
  signal.channels.forEach((ch, c) => out.set(ch, c * signal.length));
  return out;
}

function dot(a: Float32Array, b: Float32Array): number {
  let sum = 0;
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) sum += a[i] * b[i];
  return sum;
}

function rms(a: Float32Array): number {
  if (a.length === 0) return 0;
  return Math.sqrt(dot(a, a) / a.length);
}

/**
 * Scale-invariant signal-to-distortion ratio (dB).
 *
 * Scale-invariant is the point: the leveller changes the overall gain by
 * design, and a metric that punished it for that would be measuring the wrong
 * thing. The reference is projected onto the estimate first, so only the
 * *shape* of the difference counts.
 */
export function siSdrDb(reference: Signal, estimate: Signal): number {
  const s = flatten(reference);
  const x = flatten(estimate);
  const energy = dot(s, s);
  if (energy === 0) return -Infinity;

  const alpha = dot(x, s) / energy;
  let targetEnergy = 0;
  let noiseEnergy = 0;
  const n = Math.min(s.length, x.length);
  for (let i = 0; i < n; i++) {
    const target = alpha * s[i];
    targetEnergy += target * target;
    const err = x[i] - target;
    noiseEnergy += err * err;
  }

  if (noiseEnergy === 0) return Infinity;
  if (targetEnergy === 0) return -Infinity;
  return 10 * Math.log10(targetEnergy / noiseEnergy);
}

/**
 * The gain-aligned difference between two signals — what a stage actually
 * changed, with any overall level change divided out.
 */
function residual(reference: Signal, estimate: Signal): Float32Array {
  const s = flatten(reference);
  const x = flatten(estimate);
  const energy = dot(s, s);
  const alpha = energy === 0 ? 1 : dot(x, s) / energy;

  const n = Math.min(s.length, x.length);
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) out[i] = x[i] - alpha * s[i];
  return out;
}

/**
 * How audible the worst surviving click is (dB). Lower is better; anything
 * below about -6 dB sits under the signal around it and cannot be heard as a
 * click.
 *
 * The residual peak at each click site is compared against the *local peak*
 * of the reference (±10 ms), because that is how click audibility works: a
 * spike is a click when it pokes above what is already there. Two earlier
 * versions of this metric were wrong in instructive ways:
 *
 *   - Normalising by whole-file RMS punished good repairs in loud speech
 *     (global RMS is dragged down by the pauses) and forgave bad ones in
 *     silence.
 *   - Normalising by local RMS still had a false floor: AR interpolation
 *     necessarily replaces the *unpredictable* part of the signal (fricative
 *     noise, floor noise) with a different realisation, so in a pause the
 *     residual is the floor noise itself — and a peak measured against an RMS
 *     sits crest-factor above it even when the repair is perfect.
 *
 * Peak against peak is like for like: a residual spike that stays below the
 * peaks already present within ±10 ms cannot be heard as a click. In dead
 * silence the mask is floored at a fraction of the global RMS so the score
 * cannot explode dividing by nothing.
 */
export function impulsiveResidualDb(
  reference: Signal,
  estimate: Signal,
  positions: number[],
  widthSamples = 3,
): number {
  if (positions.length === 0) return -Infinity;

  const s = flatten(reference);
  const x = flatten(estimate);
  const energy = dot(s, s);
  const alpha = energy === 0 ? 1 : dot(x, s) / energy;
  const alphaScale = Math.abs(alpha) > 1e-12 ? Math.abs(alpha) : 1;
  const diff = residual(reference, estimate);
  const globalLevel = rms(s);
  if (globalLevel === 0) return -Infinity;

  const maskHalf = Math.round(0.01 * reference.sampleRate); // ±10 ms
  const guard = 8;
  let worst = -Infinity;

  for (const channelOffset of reference.channels.map((_, c) => c * reference.length)) {
    for (const pos of positions) {
      const from = Math.max(0, channelOffset + pos - guard);
      const to = Math.min(diff.length, channelOffset + pos + widthSamples + guard);
      let peak = 0;
      for (let i = from; i < to; i++) peak = Math.max(peak, Math.abs(diff[i]));
      // The residual lives at the estimate's scale, the mask at the
      // reference's; divide the gain back out so the score is scale-invariant.
      peak /= alphaScale;
      if (peak === 0) continue;

      // Local peak of the reference around the site, floored against silence.
      const maskFrom = Math.max(0, channelOffset + pos - maskHalf);
      const maskTo = Math.min(s.length, channelOffset + pos + maskHalf);
      let local = 0;
      for (let i = maskFrom; i < maskTo; i++) local = Math.max(local, Math.abs(s[i]));
      const mask = Math.max(local, 0.02 * globalLevel);

      worst = Math.max(worst, 20 * Math.log10(peak / mask));
    }
  }

  return worst;
}

/**
 * Worst per-segment deviation from the target loudness, in LU. This is the
 * leveller's actual job, measured on the output over the segment boundaries
 * the generator knows about — not on the boundaries the leveller guessed.
 */
export function segmentLufsError(
  signal: Signal,
  segments: SpeechSegment[],
  targetLufs: number,
): number {
  if (segments.length === 0) return 0;
  const filtered = preFilter(signal.channels, signal.sampleRate);

  let worst = 0;
  for (const seg of segments) {
    // Stay clear of the gain ramps that run across the pauses at each end.
    const pad = Math.round(0.3 * signal.sampleRate);
    const start = Math.min(seg.start + pad, seg.end);
    const end = Math.max(seg.end - pad, start);
    if (end - start < signal.sampleRate * 0.5) continue;

    // Gated integrated loudness, not an ungated mean over the range: that is
    // what "-23 LUFS" means, what the leveller measures to pick its gain, and
    // what the generator normalises to. An ungated mean reads ~2 LU low on
    // syllable-modulated speech, which would show up here as a phantom error.
    const measured = integratedLoudness(filtered, signal.sampleRate, start, end);
    if (!Number.isFinite(measured)) continue;
    worst = Math.max(worst, Math.abs(measured - targetLufs));
  }
  return worst;
}

function median(values: number[]): number {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * Signal-to-noise measured *locally*: each speech segment against the noise in
 * the pause leading into it, median across segments.
 *
 * This is the transparency metric, and it needs to be local. The whole-file
 * `snrGainDb` moves under the leveller for a legitimate reason — boosting a
 * quiet passage boosts its noise floor too, so the distance between programme
 * and floor genuinely shrinks when segments are pulled together. That is worth
 * reporting but it is not a defect.
 *
 * Within one segment the leveller applies a single gain, and the gain ramp has
 * essentially reached that segment's value by the end of the pause before it.
 * So speech-minus-noise measured this way is invariant to levelling, and any
 * movement is a stage genuinely changing the noise relative to the speech —
 * which is exactly what a denoiser must do, and what nothing else may do.
 */
export function segmentSnrDb(signal: Signal, segments: SpeechSegment[]): number {
  if (segments.length === 0) return NaN;
  const sampleRate = signal.sampleRate;
  const filtered = preFilter(signal.channels, sampleRate);
  const pad = Math.round(0.3 * sampleRate);
  const guard = Math.round(0.05 * sampleRate); // stay off the speech onset

  const values: number[] = [];
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    const previousEnd = i === 0 ? 0 : segments[i - 1].end;

    // A short window at the very end of the pause. It has to be short and it
    // has to be *there*: the leveller ramps gain across the whole silence and
    // only reaches this segment's gain at the end of it, so sampling earlier
    // measures noise at a gain the following speech never gets, which reads
    // as a phantom SNR change proportional to the gain difference.
    const pauseEnd = Math.max(previousEnd, seg.start - guard);
    const pauseLength = pauseEnd - previousEnd;
    if (pauseLength < 0.15 * sampleRate) continue;
    const pauseStart = pauseEnd - Math.min(pauseLength, Math.round(0.15 * sampleRate));

    // Ungated: a quiet noise floor fails the -70 LUFS absolute gate outright,
    // and here the quiet part *is* the thing being measured.
    const noise = loudnessOfRange(filtered, pauseStart, pauseEnd);

    const start = Math.min(seg.start + pad, seg.end);
    const end = Math.max(seg.end - pad, start);
    if (end - start < sampleRate * 0.5) continue;
    const speech = integratedLoudness(filtered, sampleRate, start, end);

    if (Number.isFinite(noise) && Number.isFinite(speech)) values.push(speech - noise);
  }

  return median(values);
}

/**
 * Worst deviation of the speech spectrum from its own broadly-smoothed shape,
 * in dB — how coloured the material is. The corrective EQ exists to bring this
 * down; nothing else in the chain should move it much.
 */
export function spectralDeviationDb(
  signal: Signal,
  segments: SpeechSegment[],
  minFreq = 80,
  maxFreq = 12000,
): number {
  const ranges = segments.map((s) => ({ start: s.start, end: s.end }));
  const ltas = computeLtas(signal.channels[0], signal.sampleRate, ranges);
  if (!ltas) return NaN;

  const target = broadTarget(ltas);
  let worst = 0;
  for (let i = 0; i < ltas.freqs.length; i++) {
    if (ltas.freqs[i] < minFreq || ltas.freqs[i] > maxFreq) continue;
    worst = Math.max(worst, Math.abs(ltas.db[i] - target[i]));
  }
  return worst;
}

/** How much a chain changed a signal at all: RMS difference, dB relative to input. */
export function changeDb(before: Signal, after: Signal): number {
  const a = flatten(before);
  const b = flatten(after);
  const n = Math.min(a.length, b.length);
  let diff = 0;
  for (let i = 0; i < n; i++) {
    const d = b[i] - a[i];
    diff += d * d;
  }
  if (diff === 0) return -Infinity; // bit-identical
  const level = rms(a);
  return level > 0 ? 10 * Math.log10(diff / n / (level * level)) : Infinity;
}

export interface References {
  /**
   * The undegraded signal, scored against the raw input — the "before" number.
   */
  forInput: Signal;
  /**
   * The undegraded signal *put through the same chain*, scored against the
   * output. Using the processed reference is what separates "what the
   * degradation did" from "what the chain was asked to do": the leveller
   * changes segment levels on purpose, and comparing against a raw reference
   * would score that intended change as damage.
   */
  forOutput: Signal;
}

export interface MetricInputs {
  input: Signal;
  output: Signal;
  /** Present only for cases built from a known-clean source. */
  reference?: References;
  /** Known speech-segment boundaries from the generator. */
  segments?: SpeechSegment[];
  /** Known click positions, when the case injected any. */
  clickPositions?: number[];
  /** Loudness the chain was asked to hit. */
  targetLufs?: number;
}

/**
 * Everything measurable about one case. Keys are stable — the runner compares
 * them against a stored baseline, so renaming one is a breaking change.
 */
export function computeMetrics(inputs: MetricInputs): Metrics {
  const { input, output, reference, segments, clickPositions, targetLufs } = inputs;

  const inAnalysis = new Analyzer(input);
  const outAnalysis = new Analyzer(output);

  const inLufs = inAnalysis.integratedLufs();
  const outLufs = outAnalysis.integratedLufs();
  const inFloor = inAnalysis.silence().floorLufs;
  const outFloor = outAnalysis.silence().floorLufs;

  // Whole-file programme-to-floor distance. This is a *diagnostic*, not a
  // transparency check: pulling segments toward a common level legitimately
  // shrinks it, because boosting a quiet passage boosts its noise too. It is
  // the honest measure of what levelling costs in noise, and the reason a
  // denoiser belongs before the leveller in the chain.
  const inSnr = inLufs - inFloor;
  const outSnr = outLufs - outFloor;

  const metrics: Metrics = {
    inputLufs: inLufs,
    outputLufs: outLufs,
    inputPeakDbfs: inAnalysis.peakDbfs(),
    outputPeakDbfs: outAnalysis.peakDbfs(),
    inputFloorLufs: inFloor,
    outputFloorLufs: outFloor,
    inputSnrDb: inSnr,
    outputSnrDb: outSnr,
    snrGainDb: outSnr - inSnr,
    changeDb: changeDb(input, output),
    // What a converter or a lossy encoder will actually see, as opposed to the
    // highest sample value. The limiter's ceiling is meaningless without it.
    inputTruePeakDbfs: truePeakDbfs(input.channels),
    outputTruePeakDbfs: truePeakDbfs(output.channels),
  };

  {
    // Reverberation decay, so a dereverb stage can be judged on what it did.
    const before = reverbDecayMs(input);
    const after = reverbDecayMs(output);
    metrics.inputDecayMs = before;
    metrics.outputDecayMs = after;
    metrics.decayShorteningMs = Number.isFinite(before) && Number.isFinite(after) ? before - after : 0;
  }

  if (segments) {
    const inDeviation = spectralDeviationDb(input, segments);
    const outDeviation = spectralDeviationDb(output, segments);
    metrics.inputSpectralDeviationDb = inDeviation;
    metrics.outputSpectralDeviationDb = outDeviation;
    // Positive means the chain flattened the colouration; negative means it
    // added some.
    metrics.spectralFlatteningDb = inDeviation - outDeviation;

    // The local version, which levelling leaves alone — see `segmentSnrDb`.
    const inSegSnr = segmentSnrDb(input, segments);
    const outSegSnr = segmentSnrDb(output, segments);
    metrics.inputSegmentSnrDb = inSegSnr;
    metrics.outputSegmentSnrDb = outSegSnr;
    metrics.segmentSnrGainDb = outSegSnr - inSegSnr;
  }

  if (targetLufs !== undefined) {
    metrics.lufsError = Math.abs(outLufs - targetLufs);
    if (segments) metrics.segmentLufsError = segmentLufsError(output, segments, targetLufs);
  }

  if (reference) {
    const before = siSdrDb(reference.forInput, input);
    const after = siSdrDb(reference.forOutput, output);
    metrics.inputSiSdrDb = before;
    metrics.outputSiSdrDb = after;
    metrics.siSdrGainDb = after - before;

    if (clickPositions?.length) {
      metrics.inputClickResidualDb = impulsiveResidualDb(
        reference.forInput,
        input,
        clickPositions,
      );
      metrics.outputClickResidualDb = impulsiveResidualDb(
        reference.forOutput,
        output,
        clickPositions,
      );
    }
  }

  return metrics;
}

/** Integrated loudness of a bare signal, for callers outside the Analyzer path. */
export function lufsOf(signal: Signal): number {
  return integratedLoudness(preFilter(signal.channels, signal.sampleRate), signal.sampleRate);
}
