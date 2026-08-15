import { describe, expect, it } from "vitest";
import {
  annotationsFileFor,
  annotationsToCsv,
  emptyAnnotationFile,
  parseAnnotationFile,
} from "../src/listen/annotations";
import { DEFAULT_ANNOTATION_LABELS, type AnnotationFile } from "../src/listen/types";

describe("region annotations", () => {
  it("names the sidecar after the WAV", () => {
    expect(annotationsFileFor("take1.wav")).toBe("take1.annotations.json");
    expect(annotationsFileFor("Take1.WAV")).toBe("Take1.annotations.json");
  });

  it("validates the file shape", () => {
    const ok = emptyAnnotationFile("a.wav", 48000, 12.5);
    expect(ok.labels).toEqual(DEFAULT_ANNOTATION_LABELS);
    expect(parseAnnotationFile(ok)).toBe(ok);
    expect(() => parseAnnotationFile({ ...ok, labels: "x" })).toThrow(/labels/);
    expect(() =>
      parseAnnotationFile({
        ...ok,
        annotations: [{ id: "1", start: 2, end: 1, label: "breath", confidence: "sure" }],
      }),
    ).toThrow(/end before start/);
    expect(() =>
      parseAnnotationFile({
        ...ok,
        annotations: [{ id: "1", start: 1, end: 2, label: "breath", confidence: "dunno" }],
      }),
    ).toThrow(/confidence/);
  });

  it("exports one CSV row per annotation, sorted by file then time, with quoting", () => {
    const files: AnnotationFile[] = [
      {
        ...emptyAnnotationFile("b.wav", 44100, 10),
        annotations: [
          { id: "2", start: 5, end: 5.5, label: "click", confidence: "maybe", note: 'say "hi", ok' },
          { id: "1", start: 1, end: 1.25, label: "breath", confidence: "sure" },
        ],
      },
      {
        ...emptyAnnotationFile("a.wav", 44100, 10),
        annotations: [{ id: "3", start: 0.5, end: 0.75, label: "breath", confidence: "sure" }],
      },
    ];
    const csv = annotationsToCsv(files).split("\n");
    expect(csv[0]).toBe("file,start,end,label,confidence,note");
    expect(csv[1]).toBe("a.wav,0.5000,0.7500,breath,sure,");
    expect(csv[2]).toBe("b.wav,1.0000,1.2500,breath,sure,");
    expect(csv[3]).toBe('b.wav,5.0000,5.5000,click,maybe,"say ""hi"", ok"');
    expect(csv[4]).toBe("");
  });
});
