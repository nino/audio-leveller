import { describe, it, expect } from "vitest";
import { decodeWav, encodeWav, type AudioBuffer } from "../src/dsp/wav";
import { sine } from "./helpers";

function roundTrip(audio: AudioBuffer, bitDepth: number, format: "int" | "float"): AudioBuffer {
  const encoded = encodeWav(audio, { bitDepth, format });
  return decodeWav(encoded);
}

describe("wav codec", () => {
  const sr = 48000;
  const left = sine(1000, 0.1, sr, 0.5);
  const right = sine(1000, 0.1, sr, 0.25);
  const stereo: AudioBuffer = {
    sampleRate: sr,
    channels: [left, right],
    length: left.length,
    bitDepth: 24,
    format: "int",
  };

  it("round-trips 24-bit PCM within quantisation error", () => {
    const out = roundTrip(stereo, 24, "int");
    expect(out.sampleRate).toBe(sr);
    expect(out.channels.length).toBe(2);
    expect(out.length).toBe(left.length);
    for (let i = 0; i < left.length; i++) {
      expect(out.channels[0][i]).toBeCloseTo(left[i], 4);
      expect(out.channels[1][i]).toBeCloseTo(right[i], 4);
    }
  });

  it("round-trips 16-bit PCM within quantisation error", () => {
    const out = roundTrip(stereo, 16, "int");
    for (let i = 0; i < left.length; i++) {
      expect(out.channels[0][i]).toBeCloseTo(left[i], 3);
    }
  });

  it("round-trips 32-bit float exactly", () => {
    const out = roundTrip(stereo, 32, "float");
    for (let i = 0; i < left.length; i++) {
      expect(out.channels[0][i]).toBe(left[i]);
      expect(out.channels[1][i]).toBe(right[i]);
    }
  });

  it("writes a valid 44-byte RIFF/WAVE header", () => {
    const buf = encodeWav(stereo, { bitDepth: 24, format: "int" });
    const view = new DataView(buf);
    const tag = (o: number) =>
      String.fromCharCode(view.getUint8(o), view.getUint8(o + 1), view.getUint8(o + 2), view.getUint8(o + 3));
    expect(tag(0)).toBe("RIFF");
    expect(tag(8)).toBe("WAVE");
    expect(tag(12)).toBe("fmt ");
    expect(tag(36)).toBe("data");
    expect(view.getUint16(22, true)).toBe(2); // channels
    expect(view.getUint32(24, true)).toBe(sr); // sample rate
  });

  it("rejects non-WAVE data", () => {
    const bad = new ArrayBuffer(44);
    expect(() => decodeWav(bad)).toThrow();
  });
});
