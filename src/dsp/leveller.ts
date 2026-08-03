/**
 * The leveller: ties silence detection + loudness measurement together to
 * normalise every speech segment to a target loudness, with the gain ramped
 * linearly across each silence so there are no audible jumps.
 */

import type { AudioBuffer } from "./wav";
import { preFilter, integratedLoudness } from "./loudness";
import {
  analyzeSilence,
  type SilenceOptions,
  type SilenceRegion,
} from "./silence";
import {
  buildRoomTone,
  weightedMeanGainDb,
  type RoomToneOptions,
  type RoomToneClip,
} from "./roomtone";
import { truePeakEnvelope } from "./truepeak";

export interface LevellerOptions extends Partial<SilenceOptions> {
  /** Target loudness for every segment, in LUFS. */
  targetLufs: number;
  /** Clamp per-segment gain to ±this many dB (guards against boosting noise). */
  maxGainDb: number;
  /** True-peak-ish ceiling for the output limiter, in dBFS. */
  ceilingDb: number;
  /** Room-tone bed generation options. */
  roomtone: Partial<RoomToneOptions>;
}

export const DEFAULT_LEVELLER_OPTIONS: LevellerOptions = {
  targetLufs: -23,
  maxGainDb: 30,
  ceilingDb: -1,
  minSilenceSec: 1.0,
  roomtone: {},
};

export interface SegmentReport {
  start: number;
  end: number;
  loudnessLufs: number;
  gainDb: number;
  /** False for near-silent head/tail/room-tone segments (not boosted to target). */
  isSpeech: boolean;
}

export interface RoomToneReport {
  clips: RoomToneClip[];
  instances: number;
  gainDb: number;
  length: number;
  durationSec: number;
}

export interface LevellerReport {
  sampleRate: number;
  integratedLufs: number;
  thresholdLufs: number;
  floorLufs: number;
  segments: SegmentReport[];
  silences: SilenceRegion[];
  limiterGainReductionDb: number;
  roomTone: RoomToneReport;
}

export interface LevellerResult {
  audio: AudioBuffer;
  /** The room-tone bed. `length` is 0 when no usable silence was found. */
  roomTone: AudioBuffer;
  report: LevellerReport;
}

const dbToLin = (db: number): number => Math.pow(10, db / 20);
const clampDb = (db: number, max: number): number =>
  Math.max(-max, Math.min(max, db));

/**
 * Give every non-speech segment the gain of its nearest speech neighbour
 * (by segment distance), so quiet head/tail/room-tone regions ride along
 * smoothly instead of being boosted toward the loudness target. If there is
 * no speech segment at all, everything stays at unity gain (0 dB).
 */
function fillNonSpeechGains(speechGain: number[], isSpeech: boolean[]): number[] {
  const n = speechGain.length;
  const out = new Array<number>(n);
  for (let i = 0; i < n; i++) {
    if (isSpeech[i]) {
      out[i] = speechGain[i];
      continue;
    }
    let best = -1;
    let bestDist = Infinity;
    for (let j = 0; j < n; j++) {
      if (!isSpeech[j]) continue;
      const d = Math.abs(j - i);
      if (d < bestDist) {
        bestDist = d;
        best = j;
      }
    }
    out[i] = best >= 0 ? speechGain[best] : 0;
  }
  return out;
}

/**
 * Build the piecewise-linear gain envelope (in dB) as a set of breakpoints
 * over sample index. Gain is constant across each speech body and ramps
 * linearly across each silence from the left segment's gain to the right's.
 */
function buildEnvelope(
  segmentGains: number[],
  silences: SilenceRegion[],
  length: number,
): { at: number; db: number }[] {
  const points: { at: number; db: number }[] = [];
  // Start of file at the first segment's gain.
  points.push({ at: 0, db: segmentGains[0] });

  for (let i = 0; i < silences.length; i++) {
    const s = silences[i];
    // Hold the left segment's gain up to the start of the silence...
    points.push({ at: s.start, db: segmentGains[i] });
    // ...then ramp to the right segment's gain across the silence.
    points.push({ at: s.end, db: segmentGains[i + 1] });
  }

  // Hold the last segment's gain to the end of the file.
  points.push({ at: length, db: segmentGains[segmentGains.length - 1] });
  return points;
}

/** Linear gain (not dB) at a given sample, interpolating between breakpoints. */
function gainAt(points: { at: number; db: number }[], sample: number): number {
  // Points are sorted by `at`. Linear search is fine — envelopes are tiny.
  for (let i = points.length - 1; i >= 0; i--) {
    if (sample >= points[i].at) {
      if (i === points.length - 1) return dbToLin(points[i].db);
      const a = points[i];
      const b = points[i + 1];
      const span = b.at - a.at;
      const t = span > 0 ? (sample - a.at) / span : 0;
      return dbToLin(a.db + t * (b.db - a.db));
    }
  }
  return dbToLin(points[0].db);
}

/**
 * Feed-forward limiter with a single gain-reduction curve applied to every
 * channel (preserving the stereo image), instant attack and a one-pole
 * release. Returns the maximum gain reduction applied, in dB.
 *
 * The detector runs on the **true peak**, not the sample peak. Between two
 * samples the waveform a converter reconstructs can overshoot both of them, so
 * a sample-peak limiter set to -1 dBFS routinely lets through -0.3 dBTP, and
 * lossy encoders then clip on the way out. Detecting on the 4x-oversampled
 * envelope costs one pass and makes the ceiling mean what it says.
 */
function limit(channels: Float32Array[], ceilingDb: number, sampleRate: number): number {
  const ceiling = dbToLin(ceilingDb);
  const length = channels[0]?.length ?? 0;
  const releaseCoeff = Math.exp(-1 / (0.05 * sampleRate)); // ~50 ms release
  const peaks = truePeakEnvelope(channels);
  let env = 1; // current gain-reduction factor (1 = no reduction)
  let maxReduction = 1;

  for (let i = 0; i < length; i++) {
    const peak = peaks[i];
    const required = peak > ceiling ? ceiling / peak : 1;
    // Instant attack: clamp down immediately; release recovers slowly.
    env = required < env ? required : env + (1 - env) * (1 - releaseCoeff);
    if (env < maxReduction) maxReduction = env;
    for (let c = 0; c < channels.length; c++) channels[c][i] *= env;
  }

  return maxReduction < 1 ? -20 * Math.log10(maxReduction) : 0;
}

export function levelAudio(
  input: AudioBuffer,
  options: Partial<LevellerOptions> = {},
): LevellerResult {
  const opts = { ...DEFAULT_LEVELLER_OPTIONS, ...options };
  const { sampleRate, length } = input;

  // Filter once; reuse for every measurement.
  const filtered = preFilter(input.channels, sampleRate);
  const analysis = analyzeSilence(filtered, sampleRate, opts);
  const silences = analysis.regions;

  // Segment boundaries are the file edges plus every silence midpoint.
  const boundaries = [0, ...silences.map((s) => s.mid), length];

  // First pass: measure each segment and decide whether it is real speech.
  // A segment quieter than the silence threshold is room tone / a quiet
  // head or tail — boosting it to target would just amplify the noise floor.
  const loudness: number[] = [];
  let isSpeech: boolean[] = [];
  for (let i = 0; i < boundaries.length - 1; i++) {
    const l = integratedLoudness(filtered, sampleRate, boundaries[i], boundaries[i + 1]);
    loudness.push(l);
    isSpeech.push(Number.isFinite(l) && l > analysis.thresholdLufs);
  }

  // Degenerate case (e.g. one uniform segment, no silence): the threshold can
  // sit at the program loudness and demote everything. Fall back to treating
  // every non-silent segment as speech so the file is still normalised.
  if (!isSpeech.some(Boolean)) {
    isSpeech = loudness.map((l) => Number.isFinite(l));
  }

  // Speech segments get the gain that brings them to target; non-speech
  // segments inherit their nearest speech neighbour's gain, so the envelope
  // stays smooth and no noise is boosted.
  const speechGain = loudness.map((l) => clampDb(opts.targetLufs - l, opts.maxGainDb));
  const segmentGains = fillNonSpeechGains(speechGain, isSpeech);

  const segments: SegmentReport[] = [];
  for (let i = 0; i < boundaries.length - 1; i++) {
    segments.push({
      start: boundaries[i],
      end: boundaries[i + 1],
      loudnessLufs: loudness[i],
      gainDb: segmentGains[i],
      isSpeech: isSpeech[i],
    });
  }

  // Apply the interpolated gain envelope.
  const points = buildEnvelope(segmentGains, silences, length);
  const outChannels = input.channels.map((ch) => {
    const out = new Float32Array(ch.length);
    for (let i = 0; i < ch.length; i++) out[i] = ch[i] * gainAt(points, i);
    return out;
  });

  // Catch any peaks the gain created.
  const limiterGainReductionDb = limit(outChannels, opts.ceilingDb, sampleRate);

  const audio: AudioBuffer = {
    sampleRate,
    length,
    channels: outChannels,
    bitDepth: input.bitDepth,
    format: input.format,
  };

  // Build the room-tone bed from the original silence audio, level-matched to
  // the (length-weighted) mean of the speech-segment gains.
  const meanGainDb = weightedMeanGainDb(segments);
  const rt = buildRoomTone(input.channels, sampleRate, silences, meanGainDb, opts.roomtone);
  const roomTone: AudioBuffer = {
    sampleRate,
    length: rt.length,
    channels: rt.channels,
    bitDepth: input.bitDepth,
    format: input.format,
  };

  return {
    audio,
    roomTone,
    report: {
      sampleRate,
      integratedLufs: analysis.integratedLufs,
      thresholdLufs: analysis.thresholdLufs,
      floorLufs: analysis.floorLufs,
      segments,
      silences,
      limiterGainReductionDb,
      roomTone: {
        clips: rt.clips,
        instances: rt.instances,
        gainDb: rt.gainDb,
        length: rt.length,
        durationSec: rt.length / sampleRate,
      },
    },
  };
}
