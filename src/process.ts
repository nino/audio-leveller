/**
 * Node-side glue: read a WAV file from disk, level it, and write the result.
 * Shared by the Electron worker and the CLI so both use identical logic.
 */

import { readFile, writeFile } from "node:fs/promises";
import { join, parse } from "node:path";
import { decodeWav, encodeWav } from "./dsp/wav";
import { levelAudio, type LevellerOptions, type LevellerReport } from "./dsp/leveller";

/** Derive the "<name>_<suffix>.wav" output path next to the input. */
function siblingPath(inputPath: string, suffix: string): string {
  const { dir, name } = parse(inputPath);
  return join(dir || ".", `${name}_${suffix}.wav`);
}

export const outputPathFor = (inputPath: string): string => siblingPath(inputPath, "processed");
export const roomTonePathFor = (inputPath: string): string => siblingPath(inputPath, "roomtone");

export interface ProcessResult {
  inputPath: string;
  outputPath: string;
  /** Path of the room-tone bed, or null when no usable silence was found. */
  roomTonePath: string | null;
  report: LevellerReport;
}

export async function processFile(
  inputPath: string,
  options: Partial<LevellerOptions> = {},
  outputPath = outputPathFor(inputPath),
  roomTonePath = roomTonePathFor(inputPath),
): Promise<ProcessResult> {
  const raw = await readFile(inputPath);
  // Slice to the exact byte range so DataView sees a clean ArrayBuffer.
  const arrayBuffer = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
  const audio = decodeWav(arrayBuffer);

  const { audio: leveled, roomTone, report } = levelAudio(audio, options);

  await writeFile(outputPath, Buffer.from(encodeWav(leveled)));

  // Only write a room-tone file if we actually gathered some clean material.
  let writtenRoomTonePath: string | null = null;
  if (roomTone.length > 0) {
    await writeFile(roomTonePath, Buffer.from(encodeWav(roomTone)));
    writtenRoomTonePath = roomTonePath;
  }

  return { inputPath, outputPath, roomTonePath: writtenRoomTonePath, report };
}

export type { LevellerOptions };
