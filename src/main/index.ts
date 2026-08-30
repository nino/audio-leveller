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
    // The brushed metal the renderer paints, so a slow first frame and a window
    // resize both show the same colour rather than a flash of white.
    backgroundColor: "#c3c3c3",
    // The renderer draws the title bar, the traffic lights included: the modern
    // buttons are flat circles, and this window is a 10.2 one, where they were
    // gel. So hide the real ones and let the renderer's own lights drive the
    // window over IPC. Off macOS there are no such buttons to hide.
    titleBarStyle: process.platform === "darwin" ? "hidden" : "default",
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  if (process.platform === "darwin") win.setWindowButtonVisibility(false);

  // Aqua flattens and mutes a window that is not in front. The renderer paints
  // all of its own chrome, so it needs to be told which way round it is – and
  // told at once, because a window can open behind another one.
  const sendFocus = (active: boolean): void => {
    if (!win.webContents.isDestroyed()) win.webContents.send("window-active", active);
  };
  win.on("focus", () => sendFocus(true));
  win.on("blur", () => sendFocus(false));
  win.webContents.on("did-finish-load", () => sendFocus(win.isFocused()));

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

  // What the renderer's traffic lights do. Zoom is a toggle, as the green
  // button has always been.
  ipcMain.on("window-command", (event, command: "close" | "minimise" | "zoom") => {
    const win = BrowserWindow.fromWebContents(event.sender);
    if (!win) return;
    if (command === "close") win.close();
    else if (command === "minimise") win.minimize();
    else if (win.isMaximized()) win.unmaximize();
    else win.maximize();
  });

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
