/**
 * The evaluation corpus.
 *
 * Each case pairs a synthetic input with expectations that make the run pass
 * or fail. Expectations are bounds with a stated reason, not snapshots of
 * whatever the code currently does — a bound you can't justify in a sentence
 * is a bound that will be quietly relaxed the first time it fails.
 *
 * Cases carrying a `reference` (the undegraded signal) get SI-SDR scored
 * against that reference *put through the same chain*, so the score measures
 * what the degradation did rather than what the chain was asked to do.
 */

import type { Signal, StageSpec } from "../pipeline/types";
import { buildChain, DEFAULT_CHAIN } from "../stages";
import { addClicks, addNoise, addReverb, cloneSignal, syntheticSpeech, type SpeechSegment } from "./signals";
import { applyCascade, peakingEq } from "../dsp/biquad";

/** Colour a signal with fixed resonances, as a room and a cheap mic would. */
function colour(signal: Signal, bands: { freq: number; gainDb: number; q: number }[]): Signal {
  const cascade = bands.map((b) => peakingEq(signal.sampleRate, b.freq, b.gainDb, b.q));
  return {
    sampleRate: signal.sampleRate,
    length: signal.length,
    channels: signal.channels.map((ch) => applyCascade(ch, cascade)),
  };
}

/** Add a low-frequency tone, standing in for traffic or handling noise. */
function addRumble(signal: Signal, freq: number, amplitude: number): Signal {
  const out = cloneSignal(signal);
  for (const ch of out.channels) {
    for (let i = 0; i < ch.length; i++) {
      ch[i] += amplitude * Math.sin((2 * Math.PI * freq * i) / signal.sampleRate);
    }
  }
  return out;
}

export interface Expectation {
  /** Key from `computeMetrics`. Unknown keys fail the run rather than pass it. */
  metric: string;
  min?: number;
  max?: number;
  /** Why this bound is the right one. Printed when it fails. */
  because: string;
}

export interface CaseInput {
  input: Signal;
  reference?: Signal;
  segments?: SpeechSegment[];
  clickPositions?: number[];
  targetLufs?: number;
}

export interface EvalCase {
  name: string;
  description: string;
  build(): CaseInput;
  /** Chain to run. Defaults to the standard chain. */
  chain?: StageSpec[];
  expectations: Expectation[];
}

const SR = 48000;
const TARGET = -23;

/** The standard three-spurt programme, at whatever levels a case needs. */
function programme(
  levels: number[],
  options: { sampleRate?: number; floorDbfs?: number; channels?: number; seed?: number } = {},
) {
  const { sampleRate = SR, floorDbfs = -62, channels = 1, seed = 4242 } = options;
  return syntheticSpeech({
    sampleRate,
    segments: levels.map((levelLufs, i) => ({ seconds: 3.5, levelLufs, f0: 105 + i * 22 })),
    pauseSec: 1.4,
    floorDbfs,
    seed,
    channels,
  });
}

/**
 * Within a segment, levelling is a single gain change, so speech and its own
 * noise floor must stay the same distance apart.
 *
 * Note this is the *segment-local* SNR, not the whole-file `snrGainDb`. The
 * whole-file figure legitimately drops when segments are pulled together —
 * boosting a quiet passage boosts its noise with it — so bounding that one
 * would be asserting something false about what levelling does.
 */
const SNR_PRESERVED: Expectation = {
  metric: "segmentSnrGainDb",
  min: -1,
  max: 1,
  because:
    "levelling applies one gain per segment, which moves that segment's speech " +
    "and noise together, so on material clean enough that the denoiser backs " +
    "off to nothing this must not move at all. It doubles as the check that the " +
    "backoff works: if the denoiser started processing clean sources, this is " +
    "where it would show",
};

/** Cases noisy enough that the denoiser is supposed to engage. */
function snrImproved(minDb: number, maxDb: number, note: string): Expectation {
  return {
    metric: "segmentSnrGainDb",
    min: minDb,
    max: maxDb,
    because:
      `the denoiser should engage here and deliver ${minDb} dB or better. ` +
      `${note} The upper bound matters as much as the lower: over-delivering ` +
      "means it is reaching past what the source supports, which is where " +
      "artefacts come from",
  };
}

/** Below the limiter ceiling, with a hair of slack for float rounding. */
const UNDER_CEILING: Expectation = {
  metric: "outputPeakDbfs",
  max: -0.95,
  because: "the -1 dBFS limiter ceiling must hold even when segments are boosted",
};

/**
 * The ceiling that actually matters. Between two samples the waveform a
 * converter reconstructs can overshoot both of them, so a sample-peak ceiling
 * of -1 dBFS routinely passes material that hits -0.3 dBTP on the way out —
 * and lossy encoders, which reconstruct the same inter-sample content, clip on
 * it. The limiter detects on the 4x-oversampled envelope so this holds too.
 */
const UNDER_TRUE_PEAK_CEILING: Expectation = {
  metric: "outputTruePeakDbfs",
  max: -0.95,
  because:
    "the ceiling is only meaningful as a true-peak ceiling; a sample-peak " +
    "limiter leaves inter-sample overshoot for the converter to clip",
};

const ON_TARGET: Expectation = {
  metric: "segmentLufsError",
  max: 1,
  because: "every speech segment should land within 1 LU of the target",
};

export const CASES: EvalCase[] = [
  {
    name: "clean",
    description: "Three segments at moderately different levels, quiet floor",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      UNDER_TRUE_PEAK_CEILING,
      SNR_PRESERVED,
      {
        metric: "lufsError",
        max: 1.5,
        because: "with every segment on target the programme loudness should be too",
      },
    ],
  },

  {
    name: "bypass-null",
    description: "The same programme with every stage bypassed",
    // Every stage, by name from the chain itself — a new stage added to
    // DEFAULT_CHAIN is covered by this null test automatically.
    chain: buildChain({ bypass: [...DEFAULT_CHAIN] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -300,
        because:
          "a bypassed chain must return the input untouched; bit-identical gives " +
          "-Infinity, so any real number here means something wrote to the signal",
      },
    ],
  },

  {
    name: "level-drift",
    description: "Segments 25 LU apart — the case the leveller exists for",
    build: () => {
      const { signal, segments } = programme([-40, -15, -33]);
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      { ...ON_TARGET, max: 1.5, because: "large corrections may cost a little accuracy" },
      UNDER_CEILING,
      {
        ...SNR_PRESERVED,
        max: 2.5,
        because:
          "same check as elsewhere, with headroom for a measurement artifact this " +
          "case makes unavoidable: adjacent segments here differ by ~18 dB of gain, " +
          "and the noise sample sits in the ramp between them, so it is measured a " +
          "fraction of that difference away from the speech it is compared against. " +
          "The negative bound is unchanged — nothing may reduce SNR",
      },
    ],
  },

  {
    name: "quiet",
    description: "A whole recording 22 dB too quiet",
    build: () => {
      const { signal, segments } = programme([-45, -44, -46], { floorDbfs: -78 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      snrImproved(
        1.5,
        6,
        "Its floor sits ~31 dB down, so the taper gives it only a few dB " +
          "rather than the full amount — that is the intended behaviour, not a shortfall.",
      ),
    ],
  },

  {
    name: "noisy-20db",
    description: "Broadband noise 20 dB below programme",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return {
        input: addNoise(signal, 20, 991),
        reference: cloneSignal(signal),
        segments,
        targetLufs: TARGET,
      };
    },
    expectations: [
      { ...ON_TARGET, max: 1.5, because: "noise costs the measurement a little accuracy" },
      UNDER_CEILING,
      snrImproved(6, 14, "A 20 dB floor is squarely in range for the full 12 dB."),
      {
        metric: "siSdrGainDb",
        min: -8,
        because:
          "kept as a collapse detector, not as a measure of the denoiser. It is " +
          "confounded here: the reference goes through the same chain, but the " +
          "chain is input-dependent — the clean reference is quiet enough that " +
          "the denoiser backs off, while the noisy input gets the full 12 dB — so " +
          "the two take different paths and the score reflects that as much as " +
          "any damage. `segmentSnrGainDb` is the honest number for this stage",
      },
    ],
  },

  {
    name: "noisy-6db",
    description: "Broadband noise only 6 dB below programme — the hard case",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return {
        input: addNoise(signal, 6, 7717),
        reference: cloneSignal(signal),
        segments,
        targetLufs: TARGET,
      };
    },
    expectations: [
      UNDER_CEILING,
      snrImproved(6, 14, "At 6 dB SNR there is plenty to remove."),
      {
        metric: "siSdrGainDb",
        min: 0,
        because:
          "the one case where SI-SDR is not confounded: at 6 dB SNR there is so " +
          "much noise that the denoiser engages on the reference too, and the " +
          "score must actually improve. If removing noise this thick does not " +
          "move the signal closer to clean, the stage is not working",
      },
    ],
  },

  {
    name: "clicky",
    description: "Clean programme with 40 sharp clicks scattered through it",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      const { signal: clicked, positions } = addClicks(signal, {
        count: 40,
        relativeAmplitude: 0.9,
        seed: 5150,
      });
      return {
        input: clicked,
        reference: cloneSignal(signal),
        segments,
        clickPositions: positions,
        targetLufs: TARGET,
      };
    },
    expectations: [
      UNDER_CEILING,
      {
        ...ON_TARGET,
        max: 1.5,
        because:
          "phase 1 found clicks were breaking segmentation itself — a click in a " +
          "pause lifted it over the silence threshold and merged two segments at " +
          "different levels (~8 LU of error). With de-click ahead of the leveller " +
          "the pauses are clean again, so levelling must be back on target",
      },
      {
        metric: "siSdrGainDb",
        min: 10,
        because:
          "repairing the clicks should recover most of the 20 dB they cost; " +
          "well short of this means bursts are being missed or repairs are poor",
      },
      // Click audibility is bounded on the `clicky-stage` case instead: at
      // full chain, the output-vs-processed-reference comparison at click
      // sites is dominated by micro-differences in the leveller's silence
      // boundaries between the two renders, which segmentLufsError already
      // bounds — not by surviving clicks.
    ],
  },

  {
    name: "clicky-stage",
    description: "The same clicks, de-click stage alone — the repair itself, unconfounded",
    chain: buildChain({ only: ["declick"] }),
    build: () => {
      const { signal } = programme([-30, -18, -25]);
      const { signal: clicked, positions } = addClicks(signal, {
        count: 40,
        relativeAmplitude: 0.9,
        seed: 5150,
      });
      return { input: clicked, reference: cloneSignal(signal), clickPositions: positions };
    },
    expectations: [
      {
        metric: "outputClickResidualDb",
        max: 2,
        because:
          "no repair may poke meaningfully above the peaks already present " +
          "around it (±10 ms) — that is what makes a click audible. The bound " +
          "sits just above the theoretical floor: at a pause site the residual " +
          "of even a perfect repair is the unknowable noise realisation that " +
          "was under the click, which lands at ~0 dB on this peak-vs-local-peak " +
          "measure. Repairs currently reach -1.2 dB; a missed click reads +40",
      },
      {
        metric: "changeDb",
        max: -15,
        because:
          "repairing ~40 bursts of a few samples each must leave the rest of " +
          "the file untouched. The bound is loose because the metric is energy " +
          "ratio, not sample count: 400 repaired samples out of 600k is 6e-4 of " +
          "the file, but each carries click-sized energy against a crest-heavy " +
          "programme, so a *correct* repair lands near -20 dB. Transparency is " +
          "asserted properly on `clean-declick`, where nothing should change",
      },
    ],
  },

  {
    name: "stereo",
    description: "Two-channel programme, to keep the multi-channel paths honest",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { channels: 2 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [ON_TARGET, UNDER_CEILING, SNR_PRESERVED],
  },

  {
    name: "rate-44k1",
    description: "44.1 kHz source — must not be resampled by a rate-agnostic chain",
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { sampleRate: 44100 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      {
        metric: "resampled",
        max: 0,
        because: "no enabled stage requires a fixed rate, so conversion would be pure loss",
      },
    ],
  },
  {
    name: "clean-declick",
    description: "Clean speech through de-click alone — the transparency check",
    chain: buildChain({ only: ["declick"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -60,
        because:
          "most material has no clicks at all, so the cost of a de-clicker is " +
          "measured on clean audio, where it must do essentially nothing. Voiced " +
          "speech is driven by a glottal pulse every pitch period — impulsive " +
          "excitation that an outlier detector will happily mistake for clicks — " +
          "so this is the bound that keeps the analysis block short enough for " +
          "those pulses to set their own threshold",
      },
    ],
  },

  {
    name: "clean-eq",
    description: "Clean speech through EQ alone — an auto-EQ that always acts is not corrective",
    chain: buildChain({ only: ["eq"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, segments };
    },
    expectations: [
      {
        metric: "spectralFlatteningDb",
        min: -0.5,
        max: 1.5,
        because:
          "uncoloured speech has nothing to correct; the EQ may shave a little " +
          "but must not reshape a voice that arrived fine, and must never make " +
          "the spectrum less even than it found it",
      },
    ],
  },

  {
    name: "boxy",
    description: "Speech through room resonances and sub-bass rumble",
    chain: buildChain({ only: ["eq"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      // A boxy low-mid mode and a harsh upper-mid peak, plus traffic underneath.
      const coloured = colour(signal, [
        { freq: 240, gainDb: 9, q: 3 },
        { freq: 3200, gainDb: 7, q: 3.5 },
      ]);
      return {
        input: addRumble(coloured, 38, 0.02),
        reference: cloneSignal(signal),
        segments,
      };
    },
    expectations: [
      {
        metric: "spectralFlatteningDb",
        min: 1.5,
        because:
          "two injected resonances and a rumble tone are exactly what corrective " +
          "EQ is for; if this does not measurably flatten, the stage is not working",
      },
      {
        metric: "segmentSnrGainDb",
        min: -1,
        because: "removing colouration must not cost signal-to-noise",
      },
    ],
  },

  {
    name: "hot",
    description: "Quiet crest-heavy speech boosted hard into the ceiling",
    build: () => {
      // Quiet enough to need ~20 dB of boost, so the limiter has real work to
      // do and inter-sample overshoot is where the ceiling gets decided.
      const { signal, segments } = programme([-43, -41, -44], { floorDbfs: -80 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      UNDER_TRUE_PEAK_CEILING,
      {
        metric: "segmentSnrGainDb",
        min: -1,
        max: 1,
        because: "limiting hard must still not change speech-to-noise within a segment",
      },
    ],
  },

  {
    name: "reverberant",
    description: "Speech in a live room, dereverb stage alone",
    chain: buildChain({ only: ["dereverb"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -75 });
      return {
        input: addReverb(signal, { rt60Sec: 0.7, directToReverbDb: 4 }),
        reference: cloneSignal(signal),
        segments,
      };
    },
    expectations: [
      {
        metric: "decayShorteningMs",
        min: 10,
        because:
          "the room's tail must measurably shorten. Single-channel WPE is a " +
          "modest tool — roughly 12% off the decay here — and the bound is set " +
          "to catch it stopping working, not to claim it transforms the recording",
      },
      {
        metric: "siSdrGainDb",
        min: 0,
        because: "whatever it removes must move the signal toward the dry reference, not away",
      },
    ],
  },

  {
    name: "dry-dereverb",
    description: "A dry recording through the dereverb stage — it should decline to act",
    chain: buildChain({ only: ["dereverb"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -75 });
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -300,
        because:
          "dry speech decays in ~35 ms, far below the engagement threshold, so " +
          "the stage must pass the signal through untouched rather than " +
          "processing it slightly. WPE is not transparent enough to run " +
          "unconditionally: at the pipeline's usual frame size it reduces a dry " +
          "recording to 1 dB SI-SDR",
      },
    ],
  },

];
