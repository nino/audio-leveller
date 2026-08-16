/**
 * Node-side glue: read a file from disk, run it through the pipeline, and
 * write the results. Shared by the Electron worker and the CLI so both use
 * identical logic.
 *
 * Any extra signal a stage produces (currently the room-tone bed) is written
 * as `<name>_<key>.wav` next to the input, so stages added later get an output
 * file without touching this code.
 */

import { readFile, writeFile } from "node:fs/promises";
import { join, parse } from "node:path";
import { decodeWav, encodeWav, type AudioBuffer } from "./dsp/wav";
import { runPipeline } from "./pipeline/pipeline";
import type { PipelineProgress, PipelineReport, Signal, StageSpec } from "./pipeline/types";
import type { LevellerOptions, LevellerReport } from "./dsp/leveller";
import { buildChain, type ChainOptions } from "./stages";

/** Derive the "<name>_<suffix>.wav" output path next to the input. */
function siblingPath(inputPath: string, suffix: string): string {
  const { dir, name } = parse(inputPath);
  return join(dir || ".", `${name}_${suffix}.wav`);
}

export const outputPathFor = (inputPath: string): string => siblingPath(inputPath, "processed");
export const roomTonePathFor = (inputPath: string): string => siblingPath(inputPath, "roomtone");

export interface ProcessOptions extends ChainOptions {
  /** Explicit chain, overriding `only` / `bypass` / `params`. */
  stages?: StageSpec[];
  onProgress?: (progress: PipelineProgress) => void;
}

export interface ProcessResult {
  inputPath: string;
  outputPath: string;
  /** Path of the room-tone bed, or null when no usable silence was found. */
  roomTonePath: string | null;
  /** Every extra signal written, keyed by its stage-declared name. */
  extraPaths: Record<string, string>;
  report: PipelineReport;
}

/** Pull the leveller's own report out of a pipeline report, if it ran. */
export function levellerReportOf(report: PipelineReport): LevellerReport | null {
  const stage = report.stages.find((s) => s.name === "level" && s.enabled);
  return (stage?.report as LevellerReport | undefined) ?? null;
}

function toSignal(audio: AudioBuffer): Signal {
  return { sampleRate: audio.sampleRate, channels: audio.channels, length: audio.length };
}

/** Re-attach the source file's bit depth and format for encoding. */
function toAudioBuffer(signal: Signal, source: AudioBuffer): AudioBuffer {
  return { ...signal, bitDepth: source.bitDepth, format: source.format };
}

export async function processFile(
  inputPath: string,
  options: ProcessOptions = {},
  outputPath = outputPathFor(inputPath),
): Promise<ProcessResult> {
  const raw = await readFile(inputPath);
  // Slice to the exact byte range so DataView sees a clean ArrayBuffer.
  const arrayBuffer = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
  const audio = decodeWav(arrayBuffer);

  const { stages, onProgress, ...chainOptions } = options;
  const { signal, extras, report } = runPipeline(toSignal(audio), {
    stages: stages ?? buildChain(chainOptions),
    onProgress,
  });

  await writeFile(outputPath, Buffer.from(encodeWav(toAudioBuffer(signal, audio))));

  const extraPaths: Record<string, string> = {};
  for (const [key, extra] of Object.entries(extras)) {
    const path = siblingPath(inputPath, key);
    await writeFile(path, Buffer.from(encodeWav(toAudioBuffer(extra, audio))));
    extraPaths[key] = path;
  }

  return {
    inputPath,
    outputPath,
    roomTonePath: extraPaths.roomtone ?? null,
    extraPaths,
    report,
  };
}

export type { LevellerOptions, PipelineReport };
