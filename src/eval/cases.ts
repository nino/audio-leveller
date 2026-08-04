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
import { onnxBackend } from "../models";
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
  /**
   * Why this case cannot run here, or null when it can. A case that needs
   * model weights reports them missing and is skipped rather than failing —
   * but it says so in the output, because a silently absent check is worse
   * than a missing one.
   */
  unavailableReason?(): string | null;
}

const SR = 48000;
const TARGET = -18;

/**
 * Pin the denoiser to the classical backend.
 *
 * Every case that runs the default chain says this explicitly, so the corpus
 * measures the same thing on a machine with model weights installed as on one
 * without. Leaving it to `resolveBackend` would make the baseline depend on
 * what happens to be in `~/.audio-leveller`, and "the numbers moved because
 * your home directory changed" is exactly the sort of thing this harness
 * exists to rule out. The model backend gets its own cases below.
 */
const SPECTRAL: Record<string, unknown> = { backends: ["spectral"] };

function chainWith(params: Record<string, unknown>, options?: { only?: string[] }): StageSpec[] {
  return buildChain(options).map((stage) =>
    stage.name === "denoise" ? { ...stage, params: { ...stage.params, ...params } } : stage,
  );
}

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
 * What the whole chain does to segment-local SNR on clean material.
 *
 * This bound used to read -1 to 1, on the reasoning that levelling applies one
 * gain per segment and therefore moves speech and noise together. That premise
 * expired when the expander joined the chain: an expander deliberately
 * attenuates the quiet parts, and the quiet parts inside a segment are the gaps
 * between words, so it moves this number by design. Widening it is a
 * consequence of the chain changing, not of the bound being inconvenient — and
 * the two jobs the old bound was doing have been separated rather than dropped.
 *
 * Note this is the *segment-local* SNR, not the whole-file `snrGainDb`. The
 * whole-file figure legitimately drops when segments are pulled together —
 * boosting a quiet passage boosts its noise with it — so bounding that one
 * would be asserting something false about what levelling does.
 */
const SNR_EXPANDED: Expectation = {
  metric: "segmentSnrGainDb",
  min: 6,
  max: 14,
  because:
    "on clean material the expander is the only stage that moves this, so the " +
    "bound is a statement about the expander: it must engage on a floor this " +
    "close to the programme, and it must stay inside its own range cap. The " +
    "upper bound is that cap plus the little the leveller contributes — past " +
    "14 dB means rangeDb has stopped holding, which is the difference between " +
    "an expander and a gate. The other job this bound used to do, checking " +
    "that the denoiser backs off on clean sources, now lives on `clean-denoise` " +
    "and `clean-denoise-onnx`, where it is asserted as bit-identity through the " +
    "denoise stage alone — a stronger check than an SNR window ever was",
};

/**
 * Cases noisy enough that the denoiser is supposed to engage.
 *
 * On the full chain this number is now the denoiser and the expander together,
 * which is why the windows sit higher than the denoiser alone would justify.
 * The two are separable in the report — `floorReductionDb` is the expander's
 * alone — and the denoiser is bounded unconfounded on the fixture cases.
 */
function snrImproved(minDb: number, maxDb: number, note: string): Expectation {
  return {
    metric: "segmentSnrGainDb",
    min: minDb,
    max: maxDb,
    because:
      `the denoiser and the expander together should deliver ${minDb} dB or ` +
      `better here. ${note} The upper bound matters as much as the lower: ` +
      "over-delivering means something is reaching past what the source " +
      "supports, which is where artefacts come from",
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

/**
 * Cases that exercise the model backend rather than the classical one.
 *
 * They are skipped, loudly, when the weights are absent — which is the normal
 * state of a fresh checkout and of CI, since nothing here bundles a 9 MB
 * model. `pnpm fetch-model` makes them run.
 *
 * ## Why there is no synthetic quality case here
 *
 * The corpus is speech-*shaped*, not speech: a harmonic stack with formants
 * and a syllable-rate envelope. That is enough to evaluate a spectral
 * suppressor, which knows nothing about speech and only asks what is
 * stationary. It is not enough to evaluate a trained model, which asks whether
 * what it is hearing is a voice — and DeepFilterNet3's answer on this material
 * is no. Given `noisy-20db` it removes the programme rather than the noise,
 * pulling gated loudness down by 10 dB. On real speech at the same SNR the
 * same code moves loudness by 0.07 dB and gains 4.97 dB of SI-SDR.
 *
 * So a synthetic bound on the model's *quality* would be measuring the
 * corpus's synthetic-ness. What is asserted here instead is the behaviour that
 * has to hold whatever the material: the stage must not ship a render in which
 * a backend has deleted the programme. The quality bounds live on the fixture
 * cases in `run.ts`, which need a real recording.
 */
function denoiseModelCases(): EvalCase[] {
  const onnx = { backends: ["onnx"] };
  const unavailableReason = (): string | null => onnxBackend.unavailableReason(SR);

  return [
    {
      name: "ood-denoise-onnx",
      description: "Material the model misreads — the stage must reject its output",
      chain: chainWith(onnx, { only: ["denoise"] }),
      unavailableReason,
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
        {
          metric: "changeDb",
          max: -300,
          because:
            "this is the guard case, and it is asserted on synthetic speech " +
            "precisely because the model does not recognise it. Handed material " +
            "it misreads, DeepFilterNet3 attenuates the voice by 10 dB — bounded " +
            "only by the requested reduction, which is the difference between a " +
            "quiet render and an empty one. The stage measures the programme " +
            "loudness it cost and discards a backend's output when it exceeds " +
            "`maxProgrammeLossDb`, so the signal here must come back " +
            "bit-identical. A real number means a model is free to decide the " +
            "speech is the noise and have that reach the file",
        },
      ],
    },

    {
      name: "clean-denoise-onnx",
      description: "Clean speech through DeepFilterNet3 alone — the transparency check",
      chain: chainWith(onnx, { only: ["denoise"] }),
      unavailableReason,
      build: () => {
        const { signal, segments } = programme([-30, -18, -25]);
        return { input: signal, reference: cloneSignal(signal), segments };
      },
      expectations: [
        {
          metric: "changeDb",
          max: -300,
          because:
            "the same bound as `clean-denoise`, reached by a different route. " +
            "This backend declares a 45 dB clean-source threshold rather than " +
            "the stage's 35, so unlike the classical one it *does* run here — " +
            "and then costs 6.2 dB of programme loudness, because it does not " +
            "recognise synthetic speech as speech, so the guard discards it. " +
            "Bit-identical output is therefore the check that the two " +
            "safeguards compose: a backend permitted to run on clean material " +
            "still cannot get speech damage into the file. If this ever " +
            "returns a real number, one of them has stopped working",
        },
      ],
    },
  ];
}

export const CASES: EvalCase[] = [
  {
    name: "clean",
    description: "Three segments at moderately different levels, quiet floor",
    chain: chainWith(SPECTRAL),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      UNDER_TRUE_PEAK_CEILING,
      SNR_EXPANDED,
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
    chain: chainWith(SPECTRAL),
    build: () => {
      const { signal, segments } = programme([-40, -15, -33]);
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      { ...ON_TARGET, max: 1.5, because: "large corrections may cost a little accuracy" },
      UNDER_CEILING,
      {
        ...SNR_EXPANDED,
        max: 15,
        because:
          "the same expander check as elsewhere, with headroom for a measurement " +
          "artifact this case makes unavoidable: adjacent segments here differ by " +
          "~18 dB of gain, and the noise sample sits in the ramp between them, so " +
          "it is measured a fraction of that difference away from the speech it is " +
          "compared against. Nothing may reduce SNR, and nothing may exceed the " +
          "expander's range by more than that artifact explains",
      },
    ],
  },

  {
    name: "quiet",
    description: "A whole recording 22 dB too quiet",
    chain: chainWith(SPECTRAL),
    build: () => {
      const { signal, segments } = programme([-45, -44, -46], { floorDbfs: -78 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [
      ON_TARGET,
      UNDER_CEILING,
      snrImproved(
        8,
        16,
        "The denoiser's own contribution is small — the floor sits ~31 dB down, " +
          "so its clean-source taper gives it only a few dB, which is intended " +
          "rather than a shortfall — and the expander supplies the rest.",
      ),
    ],
  },

  {
    name: "noisy-20db",
    description: "Broadband noise 20 dB below programme",
    chain: chainWith(SPECTRAL),
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
      snrImproved(
        16,
        26,
        "A 20 dB floor is squarely in range for the denoiser's full 12 dB, and " +
          "the expander then finds the gaps it leaves.",
      ),
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
    chain: chainWith(SPECTRAL),
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
    chain: chainWith(SPECTRAL),
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
    chain: chainWith(SPECTRAL),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { channels: 2 });
      return { input: signal, segments, targetLufs: TARGET };
    },
    expectations: [ON_TARGET, UNDER_CEILING, SNR_EXPANDED],
  },

  {
    name: "rate-44k1",
    description: "44.1 kHz source — must not be resampled by a rate-agnostic chain",
    chain: chainWith(SPECTRAL),
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
    name: "clean-denoise",
    description: "Clean speech through the classical denoiser alone — it should decline",
    chain: chainWith(SPECTRAL, { only: ["denoise"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -300,
        because:
          "this programme's floor sits ~32 dB below the speech, past the point " +
          "where the clean-source taper has scaled the reduction to zero, so the " +
          "stage must return the signal itself rather than a slightly processed " +
          "copy. Bit-identical gives -Infinity; any real number means the taper " +
          "stopped working",
      },
    ],
  },

  ...denoiseModelCases(),

  {
    name: "uneven",
    description: "Speech whose level drifts within each phrase — compressor alone",
    chain: buildChain({ only: ["compress"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      // A slow swell inside each spurt: the unevenness a segment leveller
      // cannot see, because it is one gain per segment and this moves under it.
      const uneven = cloneSignal(signal);
      for (const ch of uneven.channels) {
        for (const seg of segments) {
          const span = seg.end - seg.start;
          for (let i = seg.start; i < seg.end; i++) {
            const t = (i - seg.start) / span;
            ch[i] *= 10 ** ((-6 + 12 * t) / 20); // -6 dB to +6 dB across the phrase
          }
        }
      }
      return { input: uneven, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "loudnessRangeReductionLu",
        min: 1,
        because:
          "loudness range is what a compressor is for, so it is what the " +
          "compressor is measured on. A phrase swelling 12 dB is exactly the " +
          "unevenness the leveller cannot reach — one gain per segment moves " +
          "with it rather than against it. Under 1 LU of reduction means the " +
          "threshold has drifted above the programme and the stage is idling",
      },
      {
        metric: "compressorMaxReductionDb",
        max: 12,
        because:
          "the cap exists so a mis-set threshold cannot turn the stage into a " +
          "fader. If this ever sits at the cap the curve is wrong, not the cap",
      },
    ],
  },

  {
    name: "even-compress",
    description: "Already-even speech through the compressor — it should decline",
    chain: buildChain({ only: ["compress"] }),
    build: () => {
      // Three spurts at the same level with no drift: nothing to even out.
      const { signal, segments } = programme([-23, -23, -23]);
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -300,
        because:
          "compressing material that is already even costs whatever life it " +
          "has left and buys nothing, so the stage measures the loudness range " +
          "first and declines below minLoudnessRangeLu. Bit-identical output " +
          "is the check that it does — a compressor with no off switch is a " +
          "sound, not a tool",
      },
    ],
  },

  {
    name: "floor-expand",
    description: "Audible room tone between words — expander alone",
    chain: buildChain({ only: ["expand"] }),
    build: () => {
      // A floor 30 dB down: close enough to the voice to be heard in the gaps.
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -52 });
      // The same programme with the floor driven out of hearing. Same seed, so
      // the speech is sample-identical and the only difference is the noise —
      // which makes this a genuine clean reference rather than a copy of the
      // input, and lets SI-SDR say whether the speech survived.
      const { signal: quiet } = programme([-30, -18, -25], { floorDbfs: -100 });
      return { input: signal, reference: quiet, segments };
    },
    expectations: [
      {
        metric: "floorReductionDb",
        min: 3,
        because:
          "the whole job: push the floor down in the gaps, where no speech is " +
          "masking it. The reference this was measured from manages 3.6 dB of " +
          "programme-to-floor on real material, so 3 dB is the floor of useful",
      },
      {
        metric: "segmentSnrGainDb",
        min: 3,
        max: 14,
        because:
          "unlike every other stage measured this way, an expander is *supposed* " +
          "to move segment-local SNR — that is its mechanism, not a side effect. " +
          "It attenuates the quiet parts, and the quiet parts inside a segment " +
          "are the gaps between words. The upper bound is what keeps it a bound: " +
          "rangeDb caps attenuation at 12 dB, so anything past 14 means the " +
          "cap has stopped holding",
      },
      {
        metric: "siSdrGainDb",
        min: 0,
        because:
          "the check that it removes noise rather than speech. Scored against " +
          "the same programme with an inaudible floor, so a stage that chewed " +
          "word onsets to get its floor reduction would move away from the " +
          "reference even while `floorReductionDb` looked healthy",
      },
    ],
  },

  {
    name: "clean-expand",
    description: "A recording whose floor is already deep — the expander should decline",
    chain: buildChain({ only: ["expand"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -85 });
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "changeDb",
        max: -300,
        because:
          "a floor already more than cleanFloorDb below the programme has " +
          "nothing left worth pushing, and expanding anyway risks chewing the " +
          "quiet ends of words for no audible gain. Bit-identical or the " +
          "decline is not working",
      },
    ],
  },

  {
    name: "warm-voicing",
    description: "The measured tonal curve, applied to clean speech — it must stay a whisper",
    chain: buildChain({ only: ["eq"], params: { eq: { voicing: "warm" } } }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25]);
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "outputSiSdrDb",
        min: 12,
        because:
          "voicing is a taste rather than a correction, so the only honest " +
          "bound on it is that it stays small. The curve it applies is +1 dB " +
          "at 95 Hz and -1.1 dB at 3.4 kHz — recovered by measurement, and " +
          "much gentler than a first pass suggested, because most of what " +
          "looked like a de-harshing dip turned out to be that service's " +
          "per-band noise suppression rather than its EQ. A voicing that " +
          "scores below this has stopped being a tilt and become a filter",
      },
    ],
  },

  {
    name: "clean-eq",
    description: "Clean speech through EQ alone — an auto-EQ that always acts is not corrective",
    // Voicing off: this case is about the corrective fitter, and the two are
    // separate jobs. A fixed tilt riding on top would make the number measure
    // both and bound neither. The voicing has `warm-voicing` to itself.
    chain: buildChain({ only: ["eq"], params: { eq: { voicing: "neutral" } } }),
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
    // Voicing off, for the same reason as `clean-eq`.
    chain: buildChain({ only: ["eq"], params: { eq: { voicing: "neutral" } } }),
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
    chain: chainWith(SPECTRAL),
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
        ...SNR_EXPANDED,
        because:
          "limiting hard must not change speech-to-noise within a segment beyond " +
          "what the expander is already doing — the limiter works on peaks, and " +
          "peaks are not where a noise floor lives, so it must contribute nothing " +
          "to this number in either direction",
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

  {
    name: "ringing",
    description: "Speech with a sustained resonance riding on it, dynamic EQ alone",
    chain: buildChain({ only: ["dyneq"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -75 });
      const ringing = cloneSignal(signal);
      for (const ch of ringing.channels) {
        for (let i = 0; i < ch.length; i++) {
          ch[i] += 0.02 * Math.sin((2 * Math.PI * 2500 * i) / SR);
        }
      }
      return { input: ringing, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "siSdrGainDb",
        min: 0.5,
        because:
          "a sustained resonance is exactly what dynamic EQ is for; suppressing " +
          "it must move the signal toward the clean reference",
      },
      {
        metric: "spectralFlatteningDb",
        min: 0.5,
        because: "the resonance should measurably flatten out of the spectrum",
      },
    ],
  },

  {
    name: "clean-dyneq",
    description: "Clean speech through dynamic EQ alone — resonance suppression, not reshaping",
    chain: buildChain({ only: ["dyneq"] }),
    build: () => {
      const { signal, segments } = programme([-30, -18, -25], { floorDbfs: -75 });
      return { input: signal, reference: cloneSignal(signal), segments };
    },
    expectations: [
      {
        metric: "outputSiSdrDb",
        min: 15,
        because:
          "unlike the other spectral stages this one cannot decline to act — it " +
          "is per-frame by nature — so its transparency has to be measured " +
          "rather than arranged. A voice with no resonances must come back " +
          "essentially intact",
      },
      {
        metric: "spectralFlatteningDb",
        min: -0.5,
        max: 1.5,
        because: "nothing to suppress means nothing much should change",
      },
    ],
  },

];
