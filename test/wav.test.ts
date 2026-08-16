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

  it("re-encodes decoded integer PCM bit-for-bit", () => {
    // A bypassed pipeline must produce a byte-identical file, which only holds
    // if decode and encode use the same scale. Cover every integer depth, and
    // include the extreme codes where clamping bites.
    for (const bitDepth of [8, 16, 24, 32]) {
      const original = encodeWav(stereo, { bitDepth, format: "int" });
      const decoded = decodeWav(original);
      const reencoded = encodeWav(decoded, { bitDepth, format: "int" });
      expect(Array.from(new Uint8Array(reencoded)), `${bitDepth}-bit`).toEqual(
        Array.from(new Uint8Array(original)),
      );
    }
  });

  it("clamps rather than wrapping at and beyond full scale", () => {
    const extremes: AudioBuffer = {
      sampleRate: sr,
      channels: [new Float32Array([-1, 1, -1.5, 1.5, 0])],
      length: 5,
      bitDepth: 16,
      format: "int",
    };
    const view = new DataView(encodeWav(extremes, { bitDepth: 16, format: "int" }));
    expect(view.getInt16(44, true)).toBe(-32768); // -1.0 is exactly full scale
    expect(view.getInt16(46, true)).toBe(32767); // +1.0 clamps to the top code
    expect(view.getInt16(48, true)).toBe(-32768); // over-range clamps, not wraps
    expect(view.getInt16(50, true)).toBe(32767);
    expect(view.getInt16(52, true)).toBe(0);
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
