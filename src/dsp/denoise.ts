/**
 * Spectral noise reduction.
 *
 * Estimate what the noise looks like, then attenuate each time-frequency cell
 * in proportion to how much of it is noise. The whole difficulty is in not
 * making it worse: naive spectral subtraction leaves *musical noise*, a
 * shimmer of isolated surviving bins that warble in and out and sounds far
 * worse than the steady hiss it replaced. Three things here exist to prevent
 * that, and all three cost some raw reduction to buy it:
 *
 * - **Decision-directed a-priori SNR** (Ephraim & Malah). Estimating a bin's
 *   speech-to-noise ratio from the current frame alone is far too noisy a
 *   statistic; blending it with what the *previous* frame's output actually
 *   contained smooths the gain trajectory enormously. This is the single
 *   biggest difference between "denoised" and "underwater".
 *
 * - **A gain floor.** Never attenuating a bin by more than a set amount leaves
 *   the residual noise as a quiet, natural-sounding version of the original
 *   rather than a field of holes. The floor *is* the reduction target: asking
 *   for 12 dB of noise reduction sets the floor at -12 dB, which is why this
 *   module has no separate wet/dry control. A global blend would dilute the
 *   speech along with the noise; the floor only ever binds where the bin is
 *   noise-dominated.
 *
 * - **Smoothing the gain across neighbouring bins**, so a surviving bin drags
 *   its neighbours up with it instead of standing alone as a tone.
 *
 * Reaching for a bigger number than the source can support is how denoising
 * gets its bad reputation. The default 12 dB is deliberately modest.
 */

import {
  processStft,
  scanStft,
  stftFrameCount,
  binCount,
  magnitudes,
  applyGains,
  type StftFrames,
} from "./stft";
import { DEFAULT_STFT_OPTIONS } from "./stft";
import type { SampleRange } from "./ltas";

export interface DenoiseOptions {
  /**
   * How far the noise floor may be pushed down, in dB. Also the per-bin gain
   * floor — see the note above on why those are the same number.
   */
  reductionDb: number;
  frameSize: number;
  hopSize: number;
  /** Decision-directed blend. Higher is smoother; 0.98 is the usual choice. */
  smoothing: number;
  /**
   * Multiply the noise estimate by this before subtracting. Slightly
   * over-estimating noise trades a little speech dulling for markedly less
   * musical noise, and a profile measured from pauses is a slight
   * under-estimate of what sits under speech anyway.
   */
  overEstimate: number;
  /** Half-width, in bins, of the gain smoothing across frequency. */
  binSmoothing: number;
}

export const DEFAULT_DENOISE_OPTIONS: DenoiseOptions = {
  reductionDb: 12,
  frameSize: DEFAULT_STFT_OPTIONS.frameSize,
  hopSize: DEFAULT_STFT_OPTIONS.hopSize,
  smoothing: 0.98,
  overEstimate: 1.5,
  binSmoothing: 2,
};

export interface NoiseProfile {
  /** Mean noise power per bin. */
  power: Float64Array;
  /** How many frames it was measured over. */
  frames: number;
  /** True when it came from detected pauses rather than minimum statistics. */
  fromPauses: boolean;
}

// `SampleRange` is shared with the LTAS code; both mean "a half-open sample
// range", so there is no reason for two of them.
export type { SampleRange } from "./ltas";

/** Does frame `f` fall wholly inside one of the ranges? */
function frameInRanges(
  f: number,
  frameSize: number,
  hopSize: number,
  ranges: SampleRange[],
): boolean {
  // The transform pads by one frame, so frame f covers [f*hop - frameSize, ...).
  const start = f * hopSize - frameSize;
  const end = start + frameSize;
  return ranges.some((r) => start >= r.start && end <= r.end);
}

/**
 * Frames the minimum-statistics fallback will look at.
 *
 * It wants the quietest fifth of frames per bin, which means holding a value
 * per bin per frame — 513 x 241,219 x 8 bytes, very nearly a gigabyte, on a
 * twenty-minute recording. A noise floor is a stationary property, so it is
 * estimated just as well from a few thousand frames spread across the file as
 * from every one of them, and the count that was used is reported either way.
 * Below the cap nothing changes, which covers everything the eval corpus and
 * the unit suite contain.
 */
const MIN_STATISTICS_FRAME_CAP = 20_000;

function subsampleStride(total: number): number {
  return total <= MIN_STATISTICS_FRAME_CAP ? 1 : Math.ceil(total / MIN_STATISTICS_FRAME_CAP);
}

/**
 * Estimate the noise spectrum.
 *
 * From detected pauses when there are any — that is a direct measurement of
 * the thing we want to remove. Otherwise fall back to minimum statistics: per
 * bin, the mean of the quietest fifth of frames. That is more fragile (in
 * continuous speech the quietest frames still contain speech) which is why it
 * is reported, so the caller can be more conservative.
 */
type FrameScan = (visit: (spectrum: Float64Array, frameIndex: number) => void) => void;

function estimateProfile(
  frameSize: number,
  hopSize: number,
  total: number,
  scan: FrameScan,
  pauses: SampleRange[],
): NoiseProfile {
  const bins = binCount(frameSize);
  const power = new Float64Array(bins);

  let usable = 0;
  for (let f = 0; f < total; f++) {
    if (frameInRanges(f, frameSize, hopSize, pauses)) usable++;
  }

  if (usable >= 4) {
    const scratch = new Float64Array(bins);
    scan((spectrum, f) => {
      if (!frameInRanges(f, frameSize, hopSize, pauses)) return;
      magnitudes(spectrum, scratch);
      for (let k = 0; k < bins; k++) power[k] += scratch[k] * scratch[k];
    });
    for (let k = 0; k < bins; k++) power[k] /= usable;
    return { power, frames: usable, fromPauses: true };
  }

  // Minimum statistics: per bin, the mean of the quietest fifth of frames.
  if (total === 0) return { power, frames: 0, fromPauses: false };

  const stride = subsampleStride(total);
  const kept = Math.ceil(total / stride);
  const perBin: Float64Array[] = [];
  for (let k = 0; k < bins; k++) perBin.push(new Float64Array(kept));
  const scratch = new Float64Array(bins);
  let at = 0;
  scan((spectrum, f) => {
    if (f % stride !== 0 || at >= kept) return;
    magnitudes(spectrum, scratch);
    for (let k = 0; k < bins; k++) perBin[k][at] = scratch[k] * scratch[k];
    at++;
  });

  const take = Math.max(1, Math.floor(at * 0.2));
  for (let k = 0; k < bins; k++) {
    const sorted = perBin[k].subarray(0, at).slice().sort();
    let acc = 0;
    for (let i = 0; i < take; i++) acc += sorted[i];
    power[k] = acc / take;
  }

  return { power, frames: take, fromPauses: false };
}

/**
 * Estimate the noise spectrum from an already-materialised analysis.
 *
 * Kept for callers that have one in hand; the pipeline itself goes through
 * {@link estimateNoiseProfileOf}, which never materialises the frames.
 */
export function estimateNoiseProfile(
  analysis: StftFrames,
  pauses: SampleRange[],
): NoiseProfile {
  return estimateProfile(
    analysis.frameSize,
    analysis.hopSize,
    analysis.frames.length,
    (visit) => analysis.frames.forEach((spectrum, f) => visit(spectrum, f)),
    pauses,
  );
}

/** Estimate the noise spectrum straight from samples, in bounded memory. */
export function estimateNoiseProfileOf(
  samples: Float32Array,
  pauses: SampleRange[],
  frameSize: number,
  hopSize: number,
): NoiseProfile {
  const total = stftFrameCount(samples.length, { frameSize, hopSize });
  return estimateProfile(
    frameSize,
    hopSize,
    total,
    (visit) => {
      scanStft(samples, { frameSize, hopSize }, visit);
    },
    pauses,
  );
}

export interface DenoiseResult {
  channels: Float32Array[];
  profile: NoiseProfile;
  /** Mean gain applied to noise-dominated bins, in dB (negative). */
  meanNoiseGainDb: number;
  /** Mean gain applied to speech-dominated bins, in dB — should be near 0. */
  meanSpeechGainDb: number;
}

/** Denoise one channel in place of its analysis, returning the new samples. */
function denoiseChannel(
  samples: Float32Array,
  pauses: SampleRange[],
  opts: DenoiseOptions,
  stats: { noiseGain: number; noiseCount: number; speechGain: number; speechCount: number },
): { samples: Float32Array; profile: NoiseProfile } {
  const bins = binCount(opts.frameSize);
  const profile = estimateNoiseProfileOf(samples, pauses, opts.frameSize, opts.hopSize);

  const floor = Math.pow(10, -Math.abs(opts.reductionDb) / 20);
  const gains = new Float64Array(bins);
  const smoothed = new Float64Array(bins);
  const mags = new Float64Array(bins);
  // Previous frame's clean power estimate, for the decision-directed rule.
  const previousClean = new Float64Array(bins);

  const out = processStft(samples, { frameSize: opts.frameSize, hopSize: opts.hopSize }, (frame) => {
    magnitudes(frame, mags);

    for (let k = 0; k < bins; k++) {
      const noise = profile.power[k] * opts.overEstimate + 1e-20;
      const observed = mags[k] * mags[k];

      // Posterior SNR: how much louder this bin is than noise alone.
      const posterior = observed / noise;
      // A-priori SNR, blending the previous frame's clean estimate with this
      // frame's instantaneous reading. The blend is what stops the gain
      // flickering bin to bin and turning residual noise musical.
      const instantaneous = Math.max(posterior - 1, 0);
      const prior =
        opts.smoothing * (previousClean[k] / noise) + (1 - opts.smoothing) * instantaneous;

      // Wiener gain, floored so noise is attenuated but never erased.
      const wiener = prior / (1 + prior);
      gains[k] = Math.max(floor, wiener);
    }

    // Smooth across frequency: an isolated surviving bin is exactly what
    // musical noise sounds like, so let its neighbours pull it back down.
    const half = opts.binSmoothing;
    for (let k = 0; k < bins; k++) {
      let acc = 0;
      let count = 0;
      for (let j = Math.max(0, k - half); j <= Math.min(bins - 1, k + half); j++) {
        acc += gains[j];
        count++;
      }
      smoothed[k] = acc / count;
    }

    for (let k = 0; k < bins; k++) {
      const observed = mags[k] * mags[k];
      const noise = profile.power[k] * opts.overEstimate + 1e-20;
      previousClean[k] = smoothed[k] * smoothed[k] * observed;

      // Book-keeping: how hard are we pushing noise versus speech?
      const gainDb = 20 * Math.log10(Math.max(smoothed[k], 1e-12));
      if (observed < 4 * noise) {
        stats.noiseGain += gainDb;
        stats.noiseCount++;
      } else if (observed > 100 * noise) {
        stats.speechGain += gainDb;
        stats.speechCount++;
      }
    }

    applyGains(frame, smoothed);
  });

  return { samples: out, profile };
}

/**
 * Attenuate steady noise across every channel.
 *
 * `pauses` are sample ranges believed to hold no speech; the silence analysis
 * already knows them. Channels are processed independently so a noise profile
 * measured on one side is never imposed on the other.
 */
export function denoise(
  channels: Float32Array[],
  pauses: SampleRange[],
  options: Partial<DenoiseOptions> = {},
): DenoiseResult {
  const opts = { ...DEFAULT_DENOISE_OPTIONS, ...options };
  const stats = { noiseGain: 0, noiseCount: 0, speechGain: 0, speechCount: 0 };

  let profile: NoiseProfile = { power: new Float64Array(0), frames: 0, fromPauses: false };
  const out = channels.map((ch) => {
    const result = denoiseChannel(ch, pauses, opts, stats);
    profile = result.profile;
    return result.samples;
  });

  return {
    channels: out,
    profile,
    meanNoiseGainDb: stats.noiseCount > 0 ? stats.noiseGain / stats.noiseCount : 0,
    meanSpeechGainDb: stats.speechCount > 0 ? stats.speechGain / stats.speechCount : 0,
  };
}
