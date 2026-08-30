/**
 * Preload bridge. Exposes a tiny, safe API to the renderer:
 *   - getPathForFile: resolve a dropped File to an absolute path
 *     (File.path was removed from the renderer in modern Electron)
 *   - getSchema: the stages, their exposed parameters and the presets
 *   - processFile: kick off processing in the main process
 *   - onProgress: subscribe to per-stage progress while a file is running
 *   - windowCommand / onWindowActive: the title bar's traffic lights, which are
 *     drawn by the renderer and so have to ask for what they do
 *
 * It also marks the document off macOS, where the system draws a title bar of
 * its own and the renderer's must not be drawn as well.
 */

import { contextBridge, ipcRenderer, webUtils } from "electron";
import type { ProcessResult } from "../process";
import type { PipelineProgress } from "../pipeline/types";
import type { ParamOverrides, PipelineSchema } from "../params";

export interface ProcessResponse {
  ok: boolean;
  result?: ProcessResult;
  error?: string;
}

export type WindowCommand = "close" | "minimise" | "zoom";

/** What the renderer sends when it asks for a render. */
export interface ProcessRequest {
  /** Stages to skip. They still appear in the report, marked bypassed. */
  bypass?: string[];
  /** Parameter overrides, keyed by stage name. */
  params?: ParamOverrides;
}

const api = {
  getPathForFile: (file: File): string => webUtils.getPathForFile(file),
  /** The parameter schema and presets, resolved in the main process. */
  getSchema: (): Promise<PipelineSchema> => ipcRenderer.invoke("pipeline-schema"),
  processFile: (inputPath: string, request: ProcessRequest = {}): Promise<ProcessResponse> =>
    ipcRenderer.invoke("process-file", inputPath, request),
  /** Returns an unsubscribe function. */
  onProgress: (callback: (progress: PipelineProgress) => void): (() => void) => {
    const listener = (_event: unknown, progress: PipelineProgress): void => callback(progress);
    ipcRenderer.on("process-progress", listener);
    return () => {
      ipcRenderer.off("process-progress", listener);
    };
  },
  /** Close, minimise or zoom the window this renderer is in. */
  windowCommand: (command: WindowCommand): void => ipcRenderer.send("window-command", command),
  /** Whether the window is the front one, for dimming the traffic lights. */
  onWindowActive: (callback: (active: boolean) => void): void => {
    ipcRenderer.on("window-active", (_event, active: boolean) => callback(active));
  },
};

export type LevellerApi = typeof api;

contextBridge.exposeInMainWorld("leveller", api);

/*
 * The renderer paints a 10.2 title bar, which is only correct where the main
 * process hid the real one – that is, on macOS. Everywhere else the system
 * draws its own, and the painted strip would sit below it as a second,
 * non-functional-looking copy, with its buttons on the side Windows does not
 * put them. So mark the document and let the stylesheet drop it.
 *
 * This belongs in the preload rather than the renderer because the preload
 * runs before the page does: the class is in place before the first paint, so
 * the strip is never briefly visible. The renderer's own module is deferred
 * and would sometimes hide it a frame too late.
 */
function markWindowFrame(): void {
  document.documentElement.classList.toggle("native-frame", process.platform !== "darwin");
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", markWindowFrame, { once: true });
} else {
  markWindowFrame();
}
