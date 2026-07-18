/**
 * Worker thread that runs the (CPU-bound) DSP off the Electron main thread,
 * keeping the window responsive while a file is processed.
 */

import { parentPort, workerData } from "node:worker_threads";
import { processFile, type LevellerOptions } from "../process";

interface WorkerInput {
  inputPath: string;
  options?: Partial<LevellerOptions>;
}

async function run(): Promise<void> {
  if (!parentPort) throw new Error("worker: no parent port");
  const { inputPath, options } = workerData as WorkerInput;
  const result = await processFile(inputPath, options ?? {});
  parentPort.postMessage({ ok: true, result });
}

run().catch((err) => {
  parentPort?.postMessage({
    ok: false,
    error: err instanceof Error ? err.message : String(err),
  });
});
