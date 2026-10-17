/**
 * The bridge to the Rust shell, and the shapes that cross it.
 *
 * The shell owns the state (`leveller-ui`'s `Model`). It sends a `View` after
 * every change, as the result of `send` or as a `view` event when a run makes
 * progress, and the window draws from that alone. These types mirror
 * `crates/leveller-ui/src/view.rs`, whose tests pin the JSON they describe.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ParamSpec =
  | {
      kind: "number";
      key: string;
      label: string;
      min: number;
      max: number;
      step: number;
      unit: string;
      help: string;
    }
  | { kind: "boolean"; key: string; label: string; help: string }
  | {
      kind: "choice";
      key: string;
      label: string;
      /** `[value, label]` pairs. */
      options: [string, string][];
      help: string;
    };

export type ParamView = ParamSpec & { value: unknown; edited: boolean };

export interface StageView {
  name: string;
  label: string;
  description: string;
  enabled: boolean;
  expanded: boolean;
  params: ParamView[];
}

export interface StageLine {
  name: string;
  enabled: boolean;
  elapsedMs: number;
  decision: string;
}

export interface Summary {
  outputPath: string;
  roomTonePath: string | null;
  inputLufs: number;
  outputLufs: number;
  outputPeakDbfs: number;
  durationSec: number;
  channels: number;
  sampleRate: number;
  stages: StageLine[];
  segments: number;
  silences: number;
  roomToneSec: number;
}

export type StatusView =
  | { kind: "idle" }
  | { kind: "working"; file: string; stage: string; index: number; total: number; overall: number }
  | { kind: "done"; summary: Summary }
  | { kind: "failed"; message: string };

export interface View {
  /** Counts the messages the model has applied; a higher one is newer. */
  revision: number;
  preset: string;
  presets: { name: string; description: string }[];
  presetDescription: string;
  edited: boolean;
  bypassed: string[];
  stages: StageView[];
  status: StatusView;
  canRerender: boolean;
  lastInput: string | null;
  windowActive: boolean;
  dragOver: boolean;
}

/** What a click can ask for. Mirrors `Input` in `view.rs`. */
export type Input =
  | { kind: "preset"; name: string }
  | { kind: "toggleStage"; index: number }
  | { kind: "toggleExpanded"; index: number }
  | { kind: "setParam"; stage: string; key: string; value: unknown }
  | { kind: "revert" }
  | { kind: "rerender" };

export const shell = {
  view: (): Promise<View> => invoke("view"),
  send: (input: Input): Promise<View> => invoke("send", { input }),
  /** Show the open panel; the chosen file starts a run like a drop does. */
  chooseFile: (): Promise<View> => invoke("choose_file"),
  onView: (callback: (view: View) => void): Promise<UnlistenFn> =>
    listen<View>("view", (event) => callback(event.payload)),
};
