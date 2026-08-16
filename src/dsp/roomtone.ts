/**
 * Room-tone bed generator.
 *
 * Mirrors the manual technique: look through the silence sections, pick the
 * cleanest slice of each (no clicks, no breaths), then concatenate them with
 * little equal-power crossfades into a seamless bed. A single gain — the
 * length-weighted mean of the gains applied to the speech segments — is
 * applied so the bed sits at the same level as the processed voice.
 */

import type { SilenceRegion } from "./silence";

export interface RoomToneOptions {
  /** Trim this much off each end of a silence (speech tails, breath onsets). */
  edgeTrimSec: number;
  /** Ignore a silence whose usable core is shorter than this. */
  minClipSec: number;
  /** Cap how much a single silence contributes (keeps one gap from dominating). */
  maxClipSec: number;
  /** Crossfade length between concatenated clips. */
  crossfadeSec: number;
  /** Fade in/out at the very start/end of the bed. */
  edgeFadeSec: number;
  /** Step used when scanning a silence for its cleanest window. */
  scanHopSec: number;
  /** Drop clips whose dirtiness score is more than this (in dB) above the best. */
  keepMarginDb: number;
  /** Loop the bed (with crossfades) until it reaches at least this duration. */
  minDurationSec: number;
}

export const DEFAULT_ROOMTONE_OPTIONS: RoomToneOptions = {
  edgeTrimSec: 0.15,
  minClipSec: 0.3,
  maxClipSec: 1.0,
  crossfadeSec: 0.05,
  edgeFadeSec: 0.02,
  scanHopSec: 0.02,
  keepMarginDb: 6,
  minDurationSec: 10,
};

export interface RoomToneClip {
  /** Source sample range in the original file. */
  start: number;
  end: number;
  /** Cleanliness score (lower is cleaner). */
  score: number;
}

export interface RoomToneResult {
  channels: Float32Array[];
  length: number;
  /** The distinct source clips (in file order) that were used. */
  clips: RoomToneClip[];
  /** How many clip instances were laid down (>= clips.length once looped). */
  instances: number;
  /** Gain applied to the whole bed, in dB. */
  gainDb: number;
}

const EPS = 1e-9;

function median(values: number[]): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * Cleanliness of a mono-summed window, in "dirtiness" units (lower is cleaner).
 * Room tone is the *quietest steady* part of a silence, so loudness is the
 * primary term: a breath is simply louder than the noise floor. On top of that:
 *   - clicks: a localised transient shows up as one ~10 ms block whose peak
 *     towers over the *median* block peak (the median ignores the outlier, so
 *     this separates real clicks from noise's natural peakiness);
 *   - breaths/swells: high variation of short-window RMS.
 * Near-digital-silence is treated as maximally clean.
 */
/** Exported for the labelled-fixture evaluation; see `eval/fixtures/README.md`. */
export function scoreRange(
  channels: Float32Array[],
  sampleRate: number,
  start: number,
  end: number,
): number {
  const n = end - start;
  if (n <= 0) return Infinity;
  const nc = channels.length;

  // ~10 ms blocks: collect each block's RMS (for loudness + steadiness) and
  // peak (for click detection).
  const blockLen = Math.max(1, Math.round(0.01 * sampleRate));
  const blockRms: number[] = [];
  const blockPeak: number[] = [];
  let sumSqAll = 0;
  let peakAll = 0;
  for (let s = start; s < end; s += blockLen) {
    const e = Math.min(end, s + blockLen);
    let ss = 0;
    let bpeak = 0;
    for (let i = s; i < e; i++) {
      let m = 0;
      for (let c = 0; c < nc; c++) m += channels[c][i];
      m /= nc;
      ss += m * m;
      const a = Math.abs(m);
      if (a > bpeak) bpeak = a;
    }
    sumSqAll += ss;
    if (bpeak > peakAll) peakAll = bpeak;
    blockRms.push(Math.sqrt(ss / (e - s)));
    blockPeak.push(bpeak);
  }
  const rms = Math.sqrt(sumSqAll / n);

  // Effectively silent — the cleanest possible bed material.
  if (peakAll < 1e-4) return -140;

  const rmsDb = 20 * Math.log10(rms + EPS);

  // Click term: loudest block peak vs the median block peak, in dB.
  const medPeak = median(blockPeak);
  let loudestBlock = 0;
  for (const p of blockPeak) {
    if (p > loudestBlock) loudestBlock = p;
  }
  const clickDb = 20 * Math.log10(loudestBlock / (medPeak + EPS));

  // Swell/breath term: coefficient of variation of block RMS.
  const mean = blockRms.reduce((a, b) => a + b, 0) / blockRms.length;
  const variance =
    blockRms.reduce((a, b) => a + (b - mean) * (b - mean), 0) / blockRms.length;
  const cov = Math.sqrt(variance) / (mean + EPS);

  // Loudness dominates; click (dB over the median) and swell penalties add on.
  return rmsDb + 1.5 * clickDb + 25 * cov;
}

/** Find the cleanest window of length `winLen` within [coreStart, coreEnd). */
function cleanestWindow(
  channels: Float32Array[],
  sampleRate: number,
  coreStart: number,
  coreEnd: number,
  winLen: number,
  hop: number,
): { start: number; end: number; score: number } {
  const span = coreEnd - coreStart;
  if (span <= winLen) {
    return { start: coreStart, end: coreEnd, score: scoreRange(channels, sampleRate, coreStart, coreEnd) };
  }
  let best = { start: coreStart, end: coreStart + winLen, score: Infinity };
  for (let s = coreStart; s + winLen <= coreEnd; s += hop) {
    const score = scoreRange(channels, sampleRate, s, s + winLen);
    if (score < best.score) best = { start: s, end: s + winLen, score };
  }
  return best;
}

/** Length-weighted mean of the speech-segment gains (dB). */
export function weightedMeanGainDb(
  segments: { start: number; end: number; gainDb: number; isSpeech: boolean }[],
): number {
  let num = 0;
  let den = 0;
  for (const s of segments) {
    if (!s.isSpeech) continue;
    const len = s.end - s.start;
    num += s.gainDb * len;
    den += len;
  }
  return den > 0 ? num / den : 0;
}

export function buildRoomTone(
  channels: Float32Array[],
  sampleRate: number,
  silences: SilenceRegion[],
  gainDb: number,
  options: Partial<RoomToneOptions> = {},
): RoomToneResult {
  const opts = { ...DEFAULT_ROOMTONE_OPTIONS, ...options };
  const nc = channels.length;
  const trim = Math.round(opts.edgeTrimSec * sampleRate);
  const minClip = Math.round(opts.minClipSec * sampleRate);
  const maxClip = Math.round(opts.maxClipSec * sampleRate);
  const hop = Math.max(1, Math.round(opts.scanHopSec * sampleRate));

  // 1. From each silence, extract its cleanest window.
  const candidates: RoomToneClip[] = [];
  for (const s of silences) {
    const coreStart = s.start + trim;
    const coreEnd = s.end - trim;
    if (coreEnd - coreStart < minClip) continue;
    const winLen = Math.min(coreEnd - coreStart, maxClip);
    const w = cleanestWindow(channels, sampleRate, coreStart, coreEnd, winLen, hop);
    candidates.push({ start: w.start, end: w.end, score: w.score });
  }

  if (candidates.length === 0) {
    return {
      channels: channels.map(() => new Float32Array(0)),
      length: 0,
      clips: [],
      instances: 0,
      gainDb,
    };
  }

  // 2. Drop clips more than keepMarginDb dirtier than the cleanest (louder
  //    room, breaths, clicks), keeping at least the single best one.
  let bestScore = Infinity;
  for (const c of candidates) {
    if (c.score < bestScore) bestScore = c.score;
  }
  const cutoff = bestScore + opts.keepMarginDb;
  let clips = candidates.filter((c) => c.score <= cutoff);
  if (clips.length === 0) clips = [candidates.reduce((a, b) => (a.score <= b.score ? a : b))];

  // Extract each distinct clip's audio once.
  const clipAudio = clips.map((c) => channels.map((ch) => ch.slice(c.start, c.end)));

  // 3. Build the play sequence, looping through the clips until we reach the
  //    target duration. Every other instance is time-reversed so that repeats
  //    of the same clip stay decorrelated (no coherent level build-up where
  //    identical material crossfades into itself).
  const target = Math.round(opts.minDurationSec * sampleRate);
  const crossfade = Math.round(opts.crossfadeSec * sampleRate);
  const sequence: Float32Array[][] = [];
  let projected = 0;
  for (let p = 0; sequence.length < 1000; p++) {
    const base = clipAudio[p % clips.length];
    const audio = p % 2 === 1 ? base.map(reversed) : base;
    const len = audio[0].length;
    const prevLen = sequence.length > 0 ? sequence[sequence.length - 1][0].length : 0;
    const xf =
      sequence.length === 0
        ? 0
        : Math.min(crossfade, Math.floor(prevLen / 2), Math.floor(len / 2));
    sequence.push(audio);
    projected += len - xf;
    if (projected >= target) break;
  }

  // 4. Concatenate the sequence with equal-power crossfades + edge fades.
  const bed = concatCrossfade(sequence, crossfade, Math.round(opts.edgeFadeSec * sampleRate), nc);
  const gain = Math.pow(10, gainDb / 20);
  for (const ch of bed.channels) {
    for (let i = 0; i < ch.length; i++) ch[i] *= gain;
  }

  return {
    channels: bed.channels,
    length: bed.length,
    clips,
    instances: sequence.length,
    gainDb,
  };
}

function reversed(a: Float32Array): Float32Array {
  const out = new Float32Array(a.length);
  for (let i = 0; i < a.length; i++) out[i] = a[a.length - 1 - i];
  return out;
}

/** Overlap-add a sequence of clips with equal-power crossfades between them. */
function concatCrossfade(
  sequence: Float32Array[][],
  crossfade: number,
  edgeFade: number,
  nc: number,
): { channels: Float32Array[]; length: number } {
  const lengths = sequence.map((clip) => clip[0].length);
  const xf: number[] = sequence.map((_, i) =>
    i === 0 ? 0 : Math.min(crossfade, Math.floor(lengths[i - 1] / 2), Math.floor(lengths[i] / 2)),
  );

  const offsets: number[] = new Array(sequence.length);
  offsets[0] = 0;
  for (let i = 1; i < sequence.length; i++) offsets[i] = offsets[i - 1] + lengths[i - 1] - xf[i];
  const total = offsets[sequence.length - 1] + lengths[sequence.length - 1];

  const out: Float32Array[] = [];
  for (let c = 0; c < nc; c++) out.push(new Float32Array(total));
  const last = sequence.length - 1;

  for (let i = 0; i < sequence.length; i++) {
    const len = lengths[i];
    const xfIn = i > 0 ? xf[i] : 0;
    const xfOut = i < last ? xf[i + 1] : 0;
    const fadeIn = i === 0 ? Math.min(edgeFade, len) : 0;
    const fadeOut = i === last ? Math.min(edgeFade, len) : 0;

    for (let k = 0; k < len; k++) {
      let w = 1;
      if (xfIn > 0 && k < xfIn) w *= Math.sin((Math.PI / 2) * ((k + 0.5) / xfIn));
      if (xfOut > 0 && k >= len - xfOut) {
        w *= Math.cos((Math.PI / 2) * ((k - (len - xfOut) + 0.5) / xfOut));
      }
      if (fadeIn > 0 && k < fadeIn) w *= (k + 0.5) / fadeIn;
      if (fadeOut > 0 && k >= len - fadeOut) w *= (len - k - 0.5) / fadeOut;

      const dst = offsets[i] + k;
      for (let c = 0; c < nc; c++) out[c][dst] += sequence[i][c][k] * w;
    }
  }

  return { channels: out, length: total };
}
