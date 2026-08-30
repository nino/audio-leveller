/**
 * The parameters the UI exposes, and the presets that set them.
 *
 * Every stage has more parameters than are worth putting in front of someone –
 * FFT sizes, iteration counts, detector time constants that only make sense
 * next to the code that reads them. What is listed here is the subset that
 * changes how the result *sounds*, with the range each one is sane over. The
 * defaults are not repeated: they are read back out of the stage registry, so
 * this file cannot drift away from the code it describes.
 *
 * Presets are plain parameter overrides on top of those defaults, which keeps
 * "what a preset is" honest – there is no hidden second path through the
 * pipeline, only a different set of numbers.
 */

import { getStage } from "./pipeline/registry";
import { DEFAULT_CHAIN } from "./stages";

export interface NumberParam {
  kind: "number";
  key: string;
  label: string;
  min: number;
  max: number;
  step: number;
  /** Shown after the value: dB, ms, LUFS… */
  unit?: string;
  help: string;
}

export interface BooleanParam {
  kind: "boolean";
  key: string;
  label: string;
  help: string;
}

export interface ChoiceParam {
  kind: "choice";
  key: string;
  label: string;
  options: { value: string; label: string }[];
  help: string;
}

export type ParamSpec = NumberParam | BooleanParam | ChoiceParam;

export interface StageParamGroup {
  /** Stage name, as registered. */
  stage: string;
  /** Title case, for the UI. */
  label: string;
  /** The stage's own one-liner, filled in from the registry. */
  description: string;
  params: ParamSpec[];
}

/** Parameter overrides, keyed by stage name – the shape `buildChain` takes. */
export type ParamOverrides = Record<string, Record<string, unknown>>;

export interface Preset {
  name: string;
  /** One line on who this preset is for. */
  description: string;
  /** Stages to bypass entirely. */
  bypass: string[];
  params: ParamOverrides;
}

const num = (
  key: string,
  label: string,
  min: number,
  max: number,
  step: number,
  unit: string,
  help: string,
): NumberParam => ({ kind: "number", key, label, min, max, step, unit, help });

/**
 * The exposed parameters, in chain order. Ranges are the useful range rather
 * than the representable one: the leveller will happily target -40 LUFS, but
 * nobody wants that, and a slider that spends most of its travel in useless
 * territory is worse than no slider.
 */
const GROUPS: Omit<StageParamGroup, "description">[] = [
  {
    stage: "declick",
    label: "De-click",
    params: [
      num("thresholdSigma", "Sensitivity", 3, 12, 0.5, "σ", "How far above the local noise a sample must sit to count as a click. Lower finds more, and eventually finds consonants."),
      num("maxRepairFraction", "Repair limit", 0.002, 0.1, 0.002, "of file", "If more than this fraction of the file looks like clicks, the stage declines rather than rebuilding the recording."),
    ],
  },
  {
    stage: "denoise",
    label: "Denoise",
    params: [
      num("reductionDb", "Reduction", 0, 24, 1, "dB", "How much noise to ask the backend to remove."),
      num("cleanSnrDb", "Clean threshold", 15, 60, 1, "dB", "Programme-to-floor distance above which the recording counts as already clean and is left alone."),
      num("maxProgrammeLossDb", "Programme loss cap", 0, 6, 0.5, "dB", "Reject the denoised result if it cost more than this much of the speech itself."),
    ],
  },
  {
    stage: "dereverb",
    label: "De-reverb",
    params: [
      num("minDecayMs", "Dryness threshold", 40, 300, 5, "ms", "Recordings whose decay is shorter than this are dry enough to leave untouched."),
      num("taps", "Prediction taps", 5, 40, 1, "", "Length of the late-reverberation model. More taps reach further back, at a cost in time."),
      num("iterations", "Iterations", 1, 6, 1, "", "How many times the estimate is refined."),
    ],
  },
  {
    stage: "eq",
    label: "EQ",
    params: [
      {
        kind: "choice",
        key: "voicing",
        label: "Voicing",
        options: [
          { value: "warm", label: "Warm" },
          { value: "neutral", label: "Neutral" },
        ],
        help: "A fixed tonal tilt applied after the corrective fit. Warm is +1 dB under 130 Hz and -1 dB across 2.5-5 kHz; neutral is off.",
      },
      num("maxBands", "Max bands", 0, 8, 1, "", "How many corrective bands the fitter may place."),
      num("correction", "Correction strength", 0, 1, 0.05, "×", "Fraction of each measured deviation actually corrected. 1 flattens the response; less leaves the recording its character."),
      num("maxCutDb", "Max cut", 0, 12, 0.5, "dB", "Deepest single corrective cut."),
      num("maxBoostDb", "Max boost", 0, 12, 0.5, "dB", "Largest single corrective boost. Kept smaller than the cut, because boosting lifts the noise with the speech."),
      {
        kind: "boolean",
        key: "rumbleEnabled",
        label: "Rumble filter",
        help: "Fit a high-pass when the sub-bass comes too close to the voice's fundamentals.",
      },
      num("rumbleFreq", "Rumble corner", 40, 160, 5, "Hz", "Corner frequency used when the rumble filter engages."),
    ],
  },
  {
    stage: "dyneq",
    label: "Dynamic EQ",
    params: [
      num("thresholdDb", "Threshold", 4, 24, 1, "dB", "How far a band must rise above its own neighbours before it is treated as a resonance."),
      num("maxReductionDb", "Max reduction", 0, 18, 0.5, "dB", "Cap on how hard any one band is pushed down."),
      num("sibilanceExtraDb", "De-esser", 0, 9, 0.5, "dB", "Extra sensitivity across 5-9 kHz, where sibilance lives."),
    ],
  },
  {
    stage: "expand",
    label: "Expander",
    params: [
      num("thresholdBelowProgrammeDb", "Threshold", 12, 40, 1, "dB below", "How far under the programme loudness the expander starts working."),
      num("ratio", "Ratio", 1, 6, 0.1, ":1", "Slope below the threshold."),
      num("rangeDb", "Range", 0, 40, 1, "dB", "Cap on attenuation. This is what keeps it an expander rather than a gate."),
      num("cleanFloorDb", "Clean floor", 30, 80, 1, "dB", "Programme-to-floor distance above which the floor is already deep enough to leave alone."),
    ],
  },
  {
    stage: "compress",
    label: "Compressor",
    params: [
      num("thresholdRelativeDb", "Threshold", -12, 6, 0.5, "dB rel.", "Where the knee sits relative to the programme loudness."),
      num("ratio", "Ratio", 1, 6, 0.1, ":1", "Slope above the threshold."),
      num("kneeDb", "Knee", 0, 20, 1, "dB", "Width of the soft knee, centred on the threshold."),
      num("attackMs", "Attack", 1, 200, 1, "ms", "Time constant while the gain falls."),
      num("releaseMs", "Release", 20, 800, 5, "ms", "Time constant while the gain recovers."),
      num("maxReductionDb", "Max reduction", 0, 24, 1, "dB", "Hard cap on attenuation."),
      num("minLoudnessRangeLu", "Skip below", 0, 10, 0.5, "LU", "Loudness range under which the material is already even and the stage declines."),
    ],
  },
  {
    stage: "level",
    label: "Leveller",
    params: [
      num("targetLufs", "Target", -31, -12, 0.5, "LUFS", "Loudness every speech segment is normalised to."),
      num("ceilingDb", "Ceiling", -9, 0, 0.5, "dBTP", "True-peak ceiling for the output limiter."),
      num("maxGainDb", "Max gain", 0, 40, 1, "dB", "Clamp on per-segment gain, so a near-silent segment is not boosted into its own noise."),
      num("minSilenceSec", "Min silence", 0.2, 3, 0.1, "s", "How long a pause has to be before it counts as a segment boundary."),
    ],
  },
];

/** The exposed parameters, with each stage's own description attached. */
export function stageParamGroups(): StageParamGroup[] {
  return GROUPS.map((group) => ({
    ...group,
    description: getStage(group.stage).description,
  }));
}

/** Registry defaults, narrowed to the exposed keys. */
export function stageParamDefaults(): ParamOverrides {
  const defaults: ParamOverrides = {};
  for (const group of GROUPS) {
    const stageDefaults = getStage(group.stage).defaults as Record<string, unknown>;
    const picked: Record<string, unknown> = {};
    for (const param of group.params) picked[param.key] = stageDefaults[param.key];
    defaults[group.stage] = picked;
  }
  return defaults;
}

/**
 * Presets.
 *
 * `Nino` is the chain as tuned – an empty override set, deliberately, so that
 * "the default preset" and "the defaults" cannot come apart.
 *
 * `ACX` targets Audible's submission requirements: RMS between -23 and -18 dB,
 * peaks no higher than -3 dBFS, and a noise floor at or below -60 dB RMS. The
 * numbers here aim at the middle of the loudness window rather than its edge,
 * leave 0.5 dB of headroom under the peak limit, and lean harder on the two
 * stages that set the floor. Voicing goes neutral: the warm tilt is a taste,
 * and a submission that gets rejected for tone is not a taste anyone wanted.
 */
export const PRESETS: Preset[] = [
  {
    name: "Nino",
    description: "The chain as tuned – spoken word, warm, -18 LUFS.",
    bypass: [],
    params: {},
  },
  {
    name: "ACX",
    description: "Audible/ACX submission: -20 LUFS, -3.5 dBTP ceiling, deep noise floor, no tonal tilt.",
    bypass: [],
    params: {
      level: { targetLufs: -20, ceilingDb: -3.5 },
      eq: { voicing: "neutral" },
      // ACX fails a submission on its noise floor, so both floor stages are
      // asked to work on material the defaults would call clean enough.
      denoise: { reductionDb: 16, cleanSnrDb: 45 },
      expand: { cleanFloorDb: 65, rangeDb: 16 },
      // A narrower dynamic range keeps the whole book inside the RMS window,
      // chapter to chapter, without riding the fader.
      compress: { ratio: 2.2, minLoudnessRangeLu: 2 },
    },
  },
];

export const DEFAULT_PRESET = PRESETS[0].name;

export function findPreset(name: string): Preset | undefined {
  const wanted = name.toLowerCase();
  return PRESETS.find((preset) => preset.name.toLowerCase() === wanted);
}

/** Resolve a preset by name, or throw with the list of valid names. */
export function presetByName(name: string): Preset {
  const preset = findPreset(name);
  if (!preset) {
    throw new Error(
      `Unknown preset "${name}" (available: ${PRESETS.map((p) => p.name).join(", ")})`,
    );
  }
  return preset;
}

/** Fully resolved parameters for a preset: exposed defaults plus its overrides. */
export function presetSettings(name: string): ParamOverrides {
  const preset = presetByName(name);
  const resolved = stageParamDefaults();
  for (const [stage, overrides] of Object.entries(preset.params)) {
    resolved[stage] = { ...resolved[stage], ...overrides };
  }
  return resolved;
}

/** Everything the UI needs to draw the parameter panel, in one payload. */
export interface PipelineSchema {
  /** Chain order, including stages with no exposed parameters. */
  chain: string[];
  groups: StageParamGroup[];
  defaults: ParamOverrides;
  presets: Preset[];
  defaultPreset: string;
}

export function pipelineSchema(): PipelineSchema {
  return {
    chain: DEFAULT_CHAIN,
    groups: stageParamGroups(),
    defaults: stageParamDefaults(),
    presets: PRESETS,
    defaultPreset: DEFAULT_PRESET,
  };
}
