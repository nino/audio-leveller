import { describe, expect, test } from "bun:test";
import { basename, formatNumber, formatValue, pluralize, signed } from "./format";

describe("format", () => {
  test("basename takes the last component on either separator", () => {
    expect(basename("/Users/someone/talk.wav")).toBe("talk.wav");
    expect(basename("C:\\Users\\someone\\talk.wav")).toBe("talk.wav");
    expect(basename("talk.wav")).toBe("talk.wav");
  });

  test("pluralize picks the form for the count", () => {
    expect(pluralize(1, "segment")).toBe("1 segment");
    expect(pluralize(0, "segment")).toBe("0 segments");
    expect(pluralize(2, "clean clip")).toBe("2 clean clips");
  });

  test("signed always shows a sign and one decimal", () => {
    expect(signed(3)).toBe("+3.0");
    expect(signed(-1.25)).toBe("-1.3");
    expect(signed(0)).toBe("+0.0");
  });

  test("a slider's step decides its decimals", () => {
    expect(formatNumber(-18, 1)).toBe("-18");
    expect(formatNumber(-18, 0.5)).toBe("-18.0");
    expect(formatNumber(0.05, 0.05)).toBe("0.05");
    expect(formatNumber(0.0025, 0.001)).toBe("0.003");
  });

  test("a unit is spaced from its number, and an empty one is left off", () => {
    expect(formatValue(-18, 0.5, "LUFS")).toBe("-18.0 LUFS");
    expect(formatValue(4, 1, "")).toBe("4");
  });
});
