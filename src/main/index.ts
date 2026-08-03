/**
 * Electron main process: creates the window and processes dropped files in a
 * worker thread, reporting the result back to the renderer over IPC.
 */

import { app, BrowserWindow, ipcMain, type WebContents } from "electron";
import { Worker } from "node:worker_threads";
import { join } from "node:path";
import type { ProcessResult } from "../process";
import type { PipelineProgress } from "../pipeline/types";
import type { WorkerMessage } from "../worker/index";

function createWindow(): void {
  const win = new BrowserWindow({
    width: 520,
    height: 620,
    minWidth: 420,
    minHeight: 480,
    title: "Audio Leveller",
    backgroundColor: "#111318",
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

function processInWorker(
  inputPath: string,
  onProgress: (progress: PipelineProgress) => void,
): Promise<ProcessResult> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(join(__dirname, "../worker/index.js"), {
      workerData: { inputPath },
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
  ipcMain.handle("process-file", async (event, inputPath: string) => {
    const sender: WebContents = event.sender;
    try {
      const result = await processInWorker(inputPath, (progress) => {
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
