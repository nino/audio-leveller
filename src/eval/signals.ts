/**
 * Deterministic synthetic material for the evaluation harness.
 *
 * These are not speech — they are speech-*shaped*: a glottal-ish harmonic
 * stack with formant resonances, a syllable-rate envelope and pauses between
 * talk spurts. That is enough structure for the things the pipeline actually
 * measures (loudness gating, silence detection, noise floor, impulses) while
 * staying reproducible, which real recordings are not.
 *
 * Everything is seeded. No `Math.random`, so a run today and a run next month
 * produce identical numbers and a metric change always means a code change.
 */

import { preFilter, integratedLoudness } from "../dsp/loudness";
import type { Signal } from "../pipeline/types";

/** Small, fast, seedable PRNG (mulberry32). Uniform in [0, 1). */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Uniform noise in [-1, 1). */
function noiseArray(length: number, seed: number): Float32Array {
  const next = rng(seed);
  const out = new Float32Array(length);
  for (let i = 0; i < length; i++) out[i] = next() * 2 - 1;
  return out;
}

/** Integrated loudness of a bare channel, in LUFS. */
function loudnessOf(samples: Float32Array, sampleRate: number): number {
  return integratedLoudness(preFilter([samples], sampleRate), sampleRate);
}

/** Scale a channel so its integrated loudness lands on `targetLufs`. */
function normaliseTo(samples: Float32Array, sampleRate: number, targetLufs: number): Float32Array {
  const measured = loudnessOf(samples, sampleRate);
  if (!Number.isFinite(measured)) return samples;
  const gain = Math.pow(10, (targetLufs - measured) / 20);
  const out = new Float32Array(samples.length);
  for (let i = 0; i < samples.length; i++) out[i] = samples[i] * gain;
  return out;
}

/** Crude formant colouring: three resonances over a falling harmonic tilt. */
function formantGain(freq: number): number {
  const peaks = [
    { f: 500, bw: 90, gain: 1.0 },
    { f: 1500, bw: 130, gain: 0.55 },
    { f: 2600, bw: 180, gain: 0.3 },
  ];
  let g = 0.04; // a little energy everywhere, so it isn't three pure tones
  for (const p of peaks) {
    const d = (freq - p.f) / p.bw;
    g += p.gain / (1 + d * d);
  }
  return g;
}

/** One talk spurt at unit scale: harmonic stack under a syllable envelope. */
function talkSpurt(seconds: number, sampleRate: number, f0: number, seed: number): Float32Array {
  const n = Math.round(seconds * sampleRate);
  const out = new Float32Array(n);
  const next = rng(seed);
  const syllableRate = 3.8 + next() * 1.2;
  const syllablePhase = next() * Math.PI * 2;
  const fricative = noiseArray(n, seed + 1);

  let phase = 0;
  for (let i = 0; i < n; i++) {
    const t = i / sampleRate;
    // Slow pitch glide, as in real intonation.
    const pitch = f0 * (1 + 0.06 * Math.sin(2 * Math.PI * 0.7 * t + syllablePhase));
    phase += (2 * Math.PI * pitch) / sampleRate;

    let s = 0;
    for (let k = 1; k <= 16; k++) {
      const harmonic = k * pitch;
      if (harmonic > sampleRate / 2) break;
      s += (formantGain(harmonic) / Math.pow(k, 0.9)) * Math.sin(k * phase);
    }

    // Syllable-rate amplitude modulation, never quite reaching zero.
    const syllable = 0.25 + 0.75 * Math.pow(
      0.5 - 0.5 * Math.cos(2 * Math.PI * syllableRate * t + syllablePhase),
      0.6,
    );
    // Gentle fade at the spurt edges so onsets aren't clicks themselves.
    const edge = Math.min(1, Math.min(i, n - 1 - i) / (0.02 * sampleRate));

    out[i] = (s * 0.25 + fricative[i] * 0.012 * syllable) * syllable * edge;
  }

  return out;
}

export interface SpeechSegment {
  /** Sample range of the talk spurt (not including the pauses around it). */
  start: number;
  end: number;
  /** The loudness this spurt was generated at, in LUFS. */
  levelLufs: number;
}

export interface SpeechOptions {
  sampleRate: number;
  /** One entry per talk spurt. */
  segments: { seconds: number; levelLufs: number; f0?: number }[];
  /** Pause length between spurts, and before/after the first/last. */
  pauseSec: number;
  /** Steady noise floor across the whole file, in dBFS RMS. */
  floorDbfs: number;
  seed: number;
  /** Number of channels. Stereo duplicates the mono programme with slight decorrelation. */
  channels?: number;
}

export interface SyntheticSpeech {
  signal: Signal;
  segments: SpeechSegment[];
}

/**
 * Build a multi-segment "recording": pause, spurt, pause, spurt, ... Each
 * spurt is normalised to its requested loudness *before* the noise floor is
 * added, so the segment levels are exact and known.
 */
export function syntheticSpeech(options: SpeechOptions): SyntheticSpeech {
  const { sampleRate, segments, pauseSec, floorDbfs, seed, channels = 1 } = options;
  const pauseSamples = Math.round(pauseSec * sampleRate);

  const spurts = segments.map((seg, i) => {
    const raw = talkSpurt(seg.seconds, sampleRate, seg.f0 ?? 105 + i * 25, seed + i * 977);
    return normaliseTo(raw, sampleRate, seg.levelLufs);
  });

  const total =
    pauseSamples * (spurts.length + 1) + spurts.reduce((sum, s) => sum + s.length, 0);

  const programme = new Float32Array(total);
  const bounds: SpeechSegment[] = [];
  let offset = pauseSamples;
  spurts.forEach((spurt, i) => {
    programme.set(spurt, offset);
    bounds.push({ start: offset, end: offset + spurt.length, levelLufs: segments[i].levelLufs });
    offset += spurt.length + pauseSamples;
  });

  // A steady floor everywhere, including under the speech.
  const floorAmp = Math.pow(10, floorDbfs / 20) * Math.SQRT2; // RMS -> uniform-noise peak
  const out: Float32Array[] = [];
  for (let c = 0; c < channels; c++) {
    const floor = noiseArray(total, seed + 5000 + c * 31);
    const ch = new Float32Array(total);
    for (let i = 0; i < total; i++) ch[i] = programme[i] + floor[i] * floorAmp;
    out.push(ch);
  }

  return { signal: { sampleRate, channels: out, length: total }, segments: bounds };
}

/** Deep copy, so degradations never mutate a shared reference signal. */
export function cloneSignal(signal: Signal): Signal {
  return {
    sampleRate: signal.sampleRate,
    length: signal.length,
    channels: signal.channels.map((ch) => Float32Array.from(ch)),
  };
}

/**
 * Add broadband noise at a requested signal-to-noise ratio, measured as
 * integrated loudness of the programme against the noise. Returns a new signal.
 */
export function addNoise(signal: Signal, snrDb: number, seed: number): Signal {
  const programmeLufs = integratedLoudness(
    preFilter(signal.channels, signal.sampleRate),
    signal.sampleRate,
  );
  if (!Number.isFinite(programmeLufs)) return cloneSignal(signal);

  const out = cloneSignal(signal);
  out.channels.forEach((ch, c) => {
    const noise = noiseArray(signal.length, seed + c * 101);
    // Scale the noise so that on its own it measures (programme - snr) LUFS.
    const noiseLufs = loudnessOf(noise, signal.sampleRate);
    const gain = Math.pow(10, (programmeLufs - snrDb - noiseLufs) / 20);
    for (let i = 0; i < ch.length; i++) ch[i] += noise[i] * gain;
  });
  return out;
}

export interface ClickOptions {
  /** How many clicks to scatter through the file. */
  count: number;
  /** Peak click amplitude, relative to the signal's peak (1 = as loud as the peak). */
  relativeAmplitude: number;
  /** Click length in samples. Real clicks are a handful of samples wide. */
  widthSamples?: number;
  seed: number;
}

export interface ClickedSignal {
  signal: Signal;
  /** Sample index of each click, for scoring what survived. */
  positions: number[];
}

/**
 * Scatter impulsive clicks through a signal — the digital-dropout kind, a
 * couple of samples wide with a sharp bipolar shape.
 */
export function addClicks(signal: Signal, options: ClickOptions): ClickedSignal {
  const { count, relativeAmplitude, widthSamples = 3, seed } = options;
  const out = cloneSignal(signal);

  let peak = 0;
  for (const ch of signal.channels) {
    for (let i = 0; i < ch.length; i++) peak = Math.max(peak, Math.abs(ch[i]));
  }
  const amplitude = peak * relativeAmplitude;

  const next = rng(seed);
  const positions: number[] = [];
  const margin = Math.max(widthSamples * 4, 64);
  for (let k = 0; k < count; k++) {
    // Evenly spread with jitter, so clicks land in speech and in pauses alike.
    const slot = (k + 0.5) / count;
    const jitter = (next() - 0.5) / count;
    const at = Math.round((slot + jitter) * (signal.length - 2 * margin)) + margin;
    positions.push(at);

    for (const ch of out.channels) {
      for (let w = 0; w < widthSamples; w++) {
        // Alternating sign: a sharp bipolar spike, broadband by construction.
        const shape = (w % 2 === 0 ? 1 : -1) * (1 - w / widthSamples);
        ch[at + w] += amplitude * shape;
      }
    }
  }

  positions.sort((a, b) => a - b);
  return { signal: out, positions };
}
