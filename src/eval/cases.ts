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
import { addClicks, addNoise, cloneSignal, syntheticSpeech, type SpeechSegment } from "./signals";

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
    "and noise together; a denoiser should push this positive, nothing else may move it",
};

/** Below the limiter ceiling, with a hair of slack for float rounding. */
const UNDER_CEILING: Expectation = {
  metric: "outputPeakDbfs",
  max: -0.95,
  because: "the -1 dBFS limiter ceiling must hold even when segments are boosted",
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
    expectations: [ON_TARGET, UNDER_CEILING, SNR_PRESERVED],
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
      SNR_PRESERVED,
      {
        metric: "siSdrGainDb",
        min: -8,
        because:
          "noise makes the leveller's own decisions less accurate — it measures " +
          "segment loudness through the noise — so the score drifts even though " +
          "nothing here claims to remove noise; this bound catches a collapse, " +
          "and the denoiser should turn it positive",
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
      SNR_PRESERVED,
      {
        metric: "siSdrGainDb",
        min: -8,
        because:
          "same drift as noisy-20db, and at 6 dB SNR the leveller is measuring " +
          "almost as much noise as speech; a collapse past this means segmentation broke",
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
        max: -25,
        because:
          "repairing ~40 bursts of a few samples each must leave the other " +
          "99.9% of the file untouched; more change than this means the " +
          "detector is firing on clean audio",
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
];
