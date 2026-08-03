/**
 * Worker thread that runs the (CPU-bound) pipeline off the Electron main
 * thread, keeping the window responsive while a file is processed.
 *
 * Messages are tagged: `progress` may arrive many times, then exactly one
 * `done` or `error`.
 */

import { parentPort, workerData } from "node:worker_threads";
import { processFile, type ProcessOptions, type ProcessResult } from "../process";
import type { PipelineProgress } from "../pipeline/types";

interface WorkerInput {
  inputPath: string;
  options?: Omit<ProcessOptions, "onProgress">;
}

export type WorkerMessage =
  | { type: "progress"; progress: PipelineProgress }
  | { type: "done"; result: ProcessResult }
  | { type: "error"; error: string };

async function run(): Promise<void> {
  if (!parentPort) throw new Error("worker: no parent port");
  const port = parentPort;
  const { inputPath, options } = workerData as WorkerInput;

  const result = await processFile(inputPath, {
    ...options,
    onProgress: (progress) => port.postMessage({ type: "progress", progress } as WorkerMessage),
  });

  port.postMessage({ type: "done", result } as WorkerMessage);
}

run().catch((err) => {
  parentPort?.postMessage({
    type: "error",
    error: err instanceof Error ? err.message : String(err),
  } as WorkerMessage);
});
