/**
 * Deterministic synthetic material for the evaluation harness.
 *
 * These are not speech – they are speech-*shaped*: a glottal-ish harmonic
 * stack with formant resonances, a syllable-rate envelope and pauses between
 * talk spurts. That is enough structure for the things the pipeline actually
 * measures (loudness gating, silence detection, noise floor, impulses) while
 * staying reproducible, which real recordings are not.
 *
 * Everything is seeded. No `Math.random`, so a run today and a run next month
 * produce identical numbers and a metric change always means a code change.
 */

import { preFilter, integratedLoudness } from "../dsp/loudness";
import { applyCascade, peakingEq } from "../dsp/biquad";
import { convolve } from "../dsp/convolve";
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

/**
 * Vowel inventory: F1/F2/F3 for five vowels, typical adult male values.
 *
 * The spurt moves between these, which matters more than it looks. A single
 * fixed vowel leaves a ~13 dB valley between F1 and F2 in the long-term
 * average – deep enough that an injected resonance sitting in it reads as
 * filling a hole rather than as a peak, and any spectral stage evaluated
 * against it learns the wrong lesson. Real speech averages over vowels whose
 * F1 spans 270-730 Hz and F2 840-2290 Hz, which is exactly why measured
 * speech LTAS is smooth.
 *
 * Modelled as peaking filters, not two-pole resonators: cascaded resonators
 * each roll off -12 dB/octave above their centre, so four in series fall off a
 * cliff past the top formant and everything above 4 kHz becomes fiction.
 * Peaking filters return to 0 dB away from centre, adding formant contrast
 * without touching the overall tilt.
 */
const VOWELS = [
  [270, 2290, 3010], // beet
  [530, 1840, 2480], // bet
  [730, 1090, 2440], // father
  [570, 840, 2410], // boat
  [300, 870, 2240], // boot
];

/** Peaking filters for one vowel, plus a fixed upper formant. */
function vowelCascade(vowel: number[], sampleRate: number) {
  const gains = [8, 6, 4];
  const qs = [1.2, 1.2, 1.5];
  const bands = vowel.map((freq, i) => peakingEq(sampleRate, freq, gains[i], qs[i]));
  bands.push(peakingEq(sampleRate, 3700, 3, 1.5));
  return bands;
}

/**
 * One talk spurt at unit scale, by source-filter synthesis.
 *
 * An additive harmonic stack was the obvious approach and the wrong one: any
 * finite number of harmonics puts a cliff in the spectrum (16 harmonics of a
 * 110 Hz voice ends at 1.8 kHz), and everything above it is silence dressed up
 * as signal – useless for judging anything spectral. A glottal pulse train
 * through formant resonators produces energy all the way to Nyquist, from the
 * same mechanism real speech uses, at a fraction of the cost.
 */
function talkSpurt(seconds: number, sampleRate: number, f0: number, seed: number): Float32Array {
  const n = Math.round(seconds * sampleRate);
  const out = new Float32Array(n);
  const next = rng(seed);
  const syllableRate = 3.8 + next() * 1.2;
  const syllablePhase = next() * Math.PI * 2;
  const fricative = noiseArray(n, seed + 1);

  // Prosody. Without this the harmonics sit at exactly the same frequencies for
  // the whole file, and the long-term average spectrum resolves a deep comb
  // instead of a spectral envelope – an artefact no real voice produces, and
  // one that would send any spectral stage chasing the pitch rather than the
  // room. Real speech moves: pitch declines across an utterance, rises and
  // falls with intonation, and jitters cycle to cycle.
  const declinationDepth = 0.12; // ~+12% at the start to -12% at the end
  let jitter = 0;

  // Glottal source: a train of Rosenberg pulses. The pulse shape is smooth, so
  // its spectrum falls at roughly -12 dB/octave the way a real glottal flow
  // does, without the aliasing a naive sawtooth would bring.
  const openQuotient = 0.4;
  const closeQuotient = 0.16;
  let cycle = 0; // position within the current pitch period, in [0, 1)

  for (let i = 0; i < n; i++) {
    const t = i / sampleRate;
    const declination = 1 + declinationDepth * (1 - (2 * i) / Math.max(1, n - 1));
    const intonation = 1 + 0.06 * Math.sin(2 * Math.PI * 0.7 * t + syllablePhase);
    // Slow random walk, bounded, for cycle-to-cycle variation.
    jitter = Math.max(-0.03, Math.min(0.03, jitter + (next() - 0.5) * 0.0008));

    const pitch = f0 * declination * intonation * (1 + jitter);
    cycle += pitch / sampleRate;
    if (cycle >= 1) cycle -= 1;

    let pulse: number;
    if (cycle < openQuotient) {
      pulse = 0.5 * (1 - Math.cos((Math.PI * cycle) / openQuotient));
    } else if (cycle < openQuotient + closeQuotient) {
      pulse = Math.cos((Math.PI * (cycle - openQuotient)) / (2 * closeQuotient));
    } else {
      pulse = 0;
    }

    // Aspiration rides with the voicing, as it does in real speech. Kept low:
    // the radiation difference below applies +6 dB/octave to it as well, and
    // too much turns the top of the spectrum into rising noise rather than the
    // falling tilt real speech has.
    out[i] = pulse - 0.5 + fricative[i] * 0.004;
  }

  // Lip radiation is a first difference (+6 dB/octave), which turns the
  // source's -12 into the ~-6 dB/octave tilt measured speech actually shows.
  let previous = 0;
  for (let i = 0; i < n; i++) {
    const current = out[i];
    out[i] = current - 0.97 * previous;
    previous = current;
  }

  // Vocal-tract colouring, moving between vowels. Each vowel filters the whole
  // source once, then the spurt is assembled from chunks of the results with
  // short crossfades – cheaper than a time-varying filter and free of the
  // transients that switching coefficients mid-stream would introduce.
  const vowelTracks = VOWELS.map((vowel) => applyCascade(out, vowelCascade(vowel, sampleRate)));
  const chunk = Math.max(1, Math.round(0.18 * sampleRate));
  const fade = Math.max(1, Math.round(0.02 * sampleRate));

  let previousTrack = vowelTracks[Math.floor(next() * VOWELS.length) % VOWELS.length];
  for (let start = 0; start < n; start += chunk) {
    const end = Math.min(n, start + chunk);
    const track = vowelTracks[Math.floor(next() * VOWELS.length) % VOWELS.length];
    for (let i = start; i < end; i++) {
      // Equal-power crossfade from the previous vowel over the chunk's head.
      const into = i - start;
      if (into < fade && start > 0) {
        const t = into / fade;
        out[i] = previousTrack[i] * Math.cos((t * Math.PI) / 2) + track[i] * Math.sin((t * Math.PI) / 2);
      } else {
        out[i] = track[i];
      }
    }
    previousTrack = track;
  }

  // Syllable-rate amplitude envelope and edge fades, applied last so the
  // filters see a continuously voiced excitation.
  for (let i = 0; i < n; i++) {
    const t = i / sampleRate;
    const syllable =
      0.25 +
      0.75 *
        Math.pow(0.5 - 0.5 * Math.cos(2 * Math.PI * syllableRate * t + syllablePhase), 0.6);
    // Gentle fade at the spurt edges so onsets aren't clicks themselves.
    const edge = Math.min(1, Math.min(i, n - 1 - i) / (0.02 * sampleRate));
    out[i] *= syllable * edge;
  }

  // Bring the spurt to a sane working level before anything measures it. The
  // cascade's absolute output depends on formant bandwidths and the radiation
  // difference, and lands far below the -70 LUFS absolute gate – which would
  // make the loudness measurement return -Infinity and the caller's
  // normalisation silently do nothing.
  let energy = 0;
  for (let i = 0; i < n; i++) energy += out[i] * out[i];
  const rms = Math.sqrt(energy / Math.max(1, n));
  if (rms > 0) {
    const scale = 0.1 / rms;
    for (let i = 0; i < n; i++) out[i] *= scale;
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
  /**
   * Keep neighbouring clicks at least this far apart (default 0: unconstrained
   * jitter). The de-clicker treats two comparable impulses one pitch period
   * apart as voicing, by design; dense injection would otherwise manufacture
   * exactly that and score it as a miss.
   */
  minGapSec?: number;
  seed: number;
}

export interface ClickedSignal {
  signal: Signal;
  /** Sample index of each click, for scoring what survived. */
  positions: number[];
}

/**
 * Scatter impulsive clicks through a signal – the digital-dropout kind, a
 * couple of samples wide with a sharp bipolar shape.
 */
export function addClicks(signal: Signal, options: ClickOptions): ClickedSignal {
  const { count, relativeAmplitude, widthSamples = 3, seed, minGapSec = 0 } = options;
  const out = cloneSignal(signal);

  let peak = 0;
  for (const ch of signal.channels) {
    for (let i = 0; i < ch.length; i++) peak = Math.max(peak, Math.abs(ch[i]));
  }
  const amplitude = peak * relativeAmplitude;

  const next = rng(seed);
  const positions: number[] = [];
  const margin = Math.max(widthSamples * 4, 64);
  // Jitter is bounded so neighbouring clicks stay at least `minGapSec` apart.
  const span = signal.length - 2 * margin;
  const jitterSpan = Math.max(0, 1 / count - (minGapSec * signal.sampleRate) / Math.max(1, span));
  for (let k = 0; k < count; k++) {
    // Evenly spread with jitter, so clicks land in speech and in pauses alike.
    const slot = (k + 0.5) / count;
    const jitter = (next() - 0.5) * jitterSpan;
    const at = Math.round((slot + jitter) * span) + margin;
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

export interface RirOptions {
  /** Time for the tail to fall 60 dB, in seconds. */
  rt60Sec: number;
  /** How far the reverberant energy sits below the direct sound, in dB. */
  directToReverbDb: number;
  /** Gap between the direct sound and the first reflection. */
  preDelaySec: number;
  seed: number;
}

export const DEFAULT_RIR_OPTIONS: RirOptions = {
  rt60Sec: 0.6,
  directToReverbDb: 6,
  preDelaySec: 0.012,
  seed: 31337,
};

/**
 * A synthetic room impulse response: direct sound, then an exponentially
 * decaying noise tail after a pre-delay.
 *
 * Real rooms have discrete early reflections before the tail goes diffuse, and
 * a frequency-dependent decay (high frequencies die first). This models the
 * decay envelope and the direct-to-reverberant ratio, which are what a
 * dereverberator has to work against, and is honest about being a model rather
 * than a measurement – a measured RIR would be better material once real
 * fixtures exist.
 */
export function syntheticRir(
  sampleRate: number,
  options: Partial<RirOptions> = {},
): Float32Array {
  const opts = { ...DEFAULT_RIR_OPTIONS, ...options };
  const length = Math.max(2, Math.round(opts.rt60Sec * 1.5 * sampleRate));
  const rir = new Float32Array(length);
  const preDelay = Math.round(opts.preDelaySec * sampleRate);
  const next = rng(opts.seed);

  // Direct sound.
  rir[0] = 1;

  // Diffuse tail: noise under an exponential envelope reaching -60 dB at rt60.
  const decay = Math.log(1000) / (opts.rt60Sec * sampleRate); // ln(10^3) over rt60
  let tailEnergy = 0;
  for (let i = preDelay; i < length; i++) {
    const envelope = Math.exp(-decay * (i - preDelay));
    const sample = (next() * 2 - 1) * envelope;
    rir[i] = sample;
    tailEnergy += sample * sample;
  }

  // Scale the tail so the direct-to-reverberant ratio comes out as requested.
  if (tailEnergy > 0) {
    const wanted = Math.pow(10, -opts.directToReverbDb / 10); // energy ratio vs direct (=1)
    const scale = Math.sqrt(wanted / tailEnergy);
    for (let i = preDelay; i < length; i++) rir[i] *= scale;
  }

  return rir;
}

/** Put a signal in a room. Level is preserved so loudness comparisons stay meaningful. */
export function addReverb(
  signal: Signal,
  options: Partial<RirOptions> = {},
): Signal {
  const rir = syntheticRir(signal.sampleRate, options);
  const channels = signal.channels.map((ch) => convolve(ch, rir));

  // Convolution adds energy; renormalise to the dry loudness so that what the
  // metrics see afterwards is reverberation, not a level change.
  const dry = integratedLoudness(preFilter(signal.channels, signal.sampleRate), signal.sampleRate);
  const wet = integratedLoudness(preFilter(channels, signal.sampleRate), signal.sampleRate);
  if (Number.isFinite(dry) && Number.isFinite(wet)) {
    const gain = Math.pow(10, (dry - wet) / 20);
    for (const ch of channels) {
      for (let i = 0; i < ch.length; i++) ch[i] *= gain;
    }
  }

  return { sampleRate: signal.sampleRate, length: signal.length, channels };
}
