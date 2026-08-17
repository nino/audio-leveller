/**
 * Electron main process: creates the window and processes dropped files in a
 * worker thread, reporting the result back to the renderer over IPC.
 */

import { app, BrowserWindow, ipcMain, type WebContents } from "electron";
import { Worker } from "node:worker_threads";
import { join } from "node:path";
import type { ProcessResult } from "../process";
import type { PipelineProgress } from "../pipeline/types";
import type { ParamOverrides } from "../params";
import { pipelineSchema } from "../params";
import type { WorkerMessage } from "../worker/index";

function createWindow(): void {
  const win = new BrowserWindow({
    width: 760,
    height: 900,
    minWidth: 560,
    minHeight: 560,
    title: "Audio Leveller",
    // The pinstriped desktop the renderer paints, so a slow first frame and a
    // window resize both show the same colour rather than a flash of white.
    backgroundColor: "#dfe6ef",
    // On macOS the traffic lights float over the brushed metal, as they did on
    // the windows this look is borrowed from. Elsewhere, keep the native frame.
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  win.webContents.on("console-message", (_e, _level, message, line, source) => {
    console.log(`[renderer] ${message} (${source}:${line})`);
  });
  win.webContents.on("preload-error", (_e, path, error) => {
    console.error(`[preload-error] ${path}: ${error.message}`);
  });

  win.loadFile(join(__dirname, "../renderer/index.html"));

  if (process.env.LEVELLER_DEVTOOLS) win.webContents.openDevTools({ mode: "detach" });
}

interface ProcessRequest {
  bypass?: string[];
  params?: ParamOverrides;
}

function processInWorker(
  inputPath: string,
  request: ProcessRequest,
  onProgress: (progress: PipelineProgress) => void,
): Promise<ProcessResult> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(join(__dirname, "../worker/index.js"), {
      workerData: {
        inputPath,
        options: { bypass: request.bypass ?? [], params: request.params ?? {} },
      },
    });
    worker.on("message", (msg: WorkerMessage) => {
      switch (msg.type) {
        case "progress":
          onProgress(msg.progress);
          return;
        case "done":
          resolve(msg.result);
          break;
        case "error":
          reject(new Error(msg.error));
          break;
      }
      worker.terminate();
    });
    worker.once("error", reject);
    worker.once("exit", (code) => {
      if (code !== 0) reject(new Error(`Worker stopped with exit code ${code}`));
    });
  });
}

app.whenReady().then(() => {
  ipcMain.handle("pipeline-schema", () => pipelineSchema());

  ipcMain.handle("process-file", async (event, inputPath: string, request: ProcessRequest = {}) => {
    const sender: WebContents = event.sender;
    try {
      const result = await processInWorker(inputPath, request, (progress) => {
        if (!sender.isDestroyed()) sender.send("process-progress", progress);
      });
      return { ok: true, result };
    } catch (err) {
      return { ok: false, error: err instanceof Error ? err.message : String(err) };
    }
  });

  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
