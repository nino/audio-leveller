/**
 * Preload bridge. Exposes a tiny, safe API to the renderer:
 *   - getPathForFile: resolve a dropped File to an absolute path
 *     (File.path was removed from the renderer in modern Electron)
 *   - getSchema: the stages, their exposed parameters and the presets
 *   - processFile: kick off processing in the main process
 *   - onProgress: subscribe to per-stage progress while a file is running
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
};

export type LevellerApi = typeof api;

contextBridge.exposeInMainWorld("leveller", api);
