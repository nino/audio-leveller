/**
 * Region-annotation helpers shared by the dev-server API and tests: where an
 * annotation file lives, what a valid one looks like, and the CSV export the
 * evaluation harness reads.
 */

import { DEFAULT_ANNOTATION_LABELS, type Annotation, type AnnotationFile } from "./types";

/** `foo.wav` → `foo.annotations.json` (kept next to the WAV). */
export const annotationsFileFor = (wav: string): string => `${wav.replace(/\.wav$/i, "")}.annotations.json`;

export const emptyAnnotationFile = (file: string, sampleRate = 0, duration = 0): AnnotationFile => ({
  file,
  sampleRate,
  duration,
  labels: [...DEFAULT_ANNOTATION_LABELS],
  annotations: [],
  updatedAt: new Date().toISOString(),
});

const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/** Throws with a reason if `v` is not an AnnotationFile; returns it typed otherwise. */
export function parseAnnotationFile(v: unknown): AnnotationFile {
  const o = v as Record<string, unknown>;
  if (!o || typeof o !== "object") throw new Error("annotation file: not an object");
  if (typeof o.file !== "string") throw new Error("annotation file: missing file");
  if (!isNum(o.sampleRate) || !isNum(o.duration)) throw new Error("annotation file: bad sampleRate/duration");
  if (!Array.isArray(o.labels) || !o.labels.every((l) => typeof l === "string"))
    throw new Error("annotation file: bad labels");
  if (!Array.isArray(o.annotations)) throw new Error("annotation file: bad annotations");
  for (const a of o.annotations as Record<string, unknown>[]) {
    if (typeof a.id !== "string" || !isNum(a.start) || !isNum(a.end) || typeof a.label !== "string")
      throw new Error(`annotation file: bad annotation ${JSON.stringify(a)}`);
    if (a.end < a.start) throw new Error(`annotation file: end before start in ${a.id}`);
    if (a.confidence !== "sure" && a.confidence !== "maybe")
      throw new Error(`annotation file: bad confidence in ${a.id}`);
    if (a.note !== undefined && typeof a.note !== "string") throw new Error(`annotation file: bad note in ${a.id}`);
  }
  return o as unknown as AnnotationFile;
}

export const byStart = (a: Annotation, b: Annotation): number => a.start - b.start || a.end - b.end;

const csvCell = (v: unknown): string => {
  const s = String(v ?? "");
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
};

/** One row per annotation across every file, sorted by file then time. */
export function annotationsToCsv(files: AnnotationFile[]): string {
  const rows = [["file", "start", "end", "label", "confidence", "note"].join(",")];
  for (const f of [...files].sort((a, b) => a.file.localeCompare(b.file))) {
    for (const a of [...f.annotations].sort(byStart)) {
      rows.push(
        [f.file, a.start.toFixed(4), a.end.toFixed(4), a.label, a.confidence, a.note ?? ""].map(csvCell).join(","),
      );
    }
  }
  return rows.join("\n") + "\n";
}
