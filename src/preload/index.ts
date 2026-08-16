/**
 * Preload bridge. Exposes a tiny, safe API to the renderer:
 *   - getPathForFile: resolve a dropped File to an absolute path
 *     (File.path was removed from the renderer in modern Electron)
 *   - processFile: kick off processing in the main process
 *   - onProgress: subscribe to per-stage progress while a file is running
 */

import { contextBridge, ipcRenderer, webUtils } from "electron";
import type { ProcessResult } from "../process";
import type { PipelineProgress } from "../pipeline/types";

export interface ProcessResponse {
  ok: boolean;
  result?: ProcessResult;
  error?: string;
}

const api = {
  getPathForFile: (file: File): string => webUtils.getPathForFile(file),
  /** `bypass` names stages to skip; they still appear in the report. */
  processFile: (inputPath: string, bypass: string[] = []): Promise<ProcessResponse> =>
    ipcRenderer.invoke("process-file", inputPath, bypass),
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
