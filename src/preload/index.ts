/**
 * Preload bridge. Exposes a tiny, safe API to the renderer:
 *   - getPathForFile: resolve a dropped File to an absolute path
 *     (File.path was removed from the renderer in modern Electron)
 *   - processFile: kick off processing in the main process
 */

import { contextBridge, ipcRenderer, webUtils } from "electron";
import type { ProcessResult } from "../process";

export interface ProcessResponse {
  ok: boolean;
  result?: ProcessResult;
  error?: string;
}

const api = {
  getPathForFile: (file: File): string => webUtils.getPathForFile(file),
  processFile: (inputPath: string): Promise<ProcessResponse> =>
    ipcRenderer.invoke("process-file", inputPath),
};

export type LevellerApi = typeof api;

contextBridge.exposeInMainWorld("leveller", api);
