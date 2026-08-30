/**
 * The listening-test data model, shared by the session builder (Node) and the
 * browser app (which imports these types only).
 *
 * A **session** is a folder under `listening/sessions/<name>/` holding
 * `session.json` (what the listener sees), `key.json` (the blinding, which the
 * app never loads until the listener asks to reveal), the clip WAVs, and one
 * `results.<listener>.json` per person who scored it.
 */

/** A question asked about every clip in a trial. */
export type ClipQuestion =
  | { kind: "scale"; id: string; label: string; min: number; max: number; minLabel?: string; maxLabel?: string }
  | { kind: "tags"; id: string; label: string; options: string[] }
  | { kind: "text"; id: string; label: string };

/** A question asked once per trial, about the set as a whole. */
export type TrialQuestion =
  | { kind: "pick"; id: string; label: string }
  | { kind: "text"; id: string; label: string };

export interface Clip {
  /** Blind label shown to the listener: "A", "B", ... */
  label: string;
  /** File name inside the session folder. */
  file: string;
}

export interface Trial {
  id: string;
  /** Free text shown to the listener, e.g. "loud passage, 1080 s". Never names variants. */
  title: string;
  clips: Clip[];
}

export interface Session {
  name: string;
  createdAt: string;
  /** Loudness every clip was matched to, so results are comparable. */
  loudnessLufs: number;
  matchBy: "integrated" | "shortTermMax" | "momentaryMax";
  clipQuestions: ClipQuestion[];
  trialQuestions: TrialQuestion[];
  trials: Trial[];
}

/** The blinding: which variant each blind clip actually is. */
export interface SessionKey {
  name: string;
  variants: Record<string, { file: string; description?: string }>;
  /** trial id → clip label → variant id */
  clips: Record<string, Record<string, string>>;
  /** trial id → the window this trial was cut from */
  windows: Record<string, { start: number; dur: number; label?: string }>;
}

export type Answer = number | string | string[] | null;

export interface TrialResult {
  clips: Record<string, Record<string, Answer>>; // clip label → question id → answer
  trial: Record<string, Answer>; // question id → answer
  /** ms the listener spent with this trial open. */
  timeSpentMs: number;
}

export interface Results {
  session: string;
  listener: string;
  startedAt: string;
  updatedAt: string;
  trials: Record<string, TrialResult>;
}

/** Input to the session builder. */
export interface SessionSpec {
  name: string;
  /** Match every clip to this loudness (default -28 LUFS). */
  loudnessLufs?: number;
  /**
   * What "loudness" means for matching. `integrated` (BS.1770 gated) is the
   * broadcast answer but lets a peakier clip's loud words stand out – clips
   * with different dynamics can share an integrated value and still sound
   * unequal. `shortTermMax` matches the loudest 3 s window (so nothing is
   * ever louder than the others), `momentaryMax` the loudest 400 ms.
   * Default `momentaryMax`: nothing is ever louder than the others, which is what a preference judgement needs.
   */
  matchBy?: "integrated" | "shortTermMax" | "momentaryMax";
  /**
   * Named sources. `file` is a full-length recording that gets cut per window;
   * `clip` is an already-cut excerpt (still loudness-matched, never cut);
   * `clips` maps window index → already-cut excerpt for that window.
   */
  variants: Record<
    string,
    { file?: string; clip?: string; clips?: Record<number, string>; description?: string }
  >;
  windows: { start: number; dur: number; label?: string }[];
  /**
   * Explicit trials, or omit to auto-build one trial per window from every
   * variant, chunked to at most `maxClipsPerTrial` (default 4).
   */
  trials?: { window: number; variants: string[]; title?: string }[];
  maxClipsPerTrial?: number;
  clipQuestions?: ClipQuestion[];
  trialQuestions?: TrialQuestion[];
}

export const DEFAULT_CLIP_QUESTIONS: ClipQuestion[] = [
  {
    kind: "scale",
    id: "distortion",
    label: "Distortion",
    min: 0,
    max: 5,
    minLabel: "none",
    maxLabel: "severe",
  },
  {
    kind: "tags",
    id: "artefacts",
    label: "Artefacts",
    options: ["harsh", "pumping", "dull", "noisy", "thin", "boomy", "clicks", "sibilant", "unnatural"],
  },
  { kind: "text", id: "notes", label: "Notes" },
];

export const DEFAULT_TRIAL_QUESTIONS: TrialQuestion[] = [
  { kind: "pick", id: "best", label: "Which sounds best overall?" },
  { kind: "text", id: "comment", label: "Anything else about this set?" },
];

// ---- region annotation ----
//
// Files under `listening/annotate/*.wav` can be marked up in the app's
// `/annotate` editor: regions with a label (breath, click, ...) that later
// train and score a breath-reduction stage. Labels are saved next to the WAV
// as `<name>.annotations.json`.

export type Confidence = "sure" | "maybe";

export interface Annotation {
  id: string;
  /** Seconds from the start of the file. */
  start: number;
  end: number;
  label: string;
  note?: string;
  confidence: Confidence;
}

export interface AnnotationFile {
  /** WAV file name inside `listening/annotate/`. */
  file: string;
  sampleRate: number;
  duration: number;
  /** The label set offered in the editor; the first letter of each is its shortcut. */
  labels: string[];
  annotations: Annotation[];
  updatedAt: string;
}

export const DEFAULT_ANNOTATION_LABELS = ["breath", "click", "plosive", "mouth-noise", "other"];
