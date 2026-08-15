import { mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { decodeWav, encodeWav } from "../src/dsp/wav";
import { integratedLoudness, preFilter } from "../src/dsp/loudness";
import { buildSession } from "../src/listen/build-session";
import type { Session, SessionKey } from "../src/listen/types";

const SR = 44100;

/** A few seconds of shaped noise at a given level, so loudness is measurable. */
function noiseWav(seconds: number, gain: number, seed: number): ArrayBuffer {
  let s = seed;
  const rnd = (): number => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0xffffffff - 0.5;
  };
  const ch = new Float32Array(SR * seconds);
  let lp = 0;
  for (let i = 0; i < ch.length; i++) {
    lp += 0.2 * (rnd() - lp);
    ch[i] = lp * gain;
  }
  return encodeWav({ sampleRate: SR, channels: [ch], length: ch.length, bitDepth: 32, format: "float" });
}

describe("listening-session builder", () => {
  it("cuts, loudness-matches, blinds, and writes a key", async () => {
    const dir = await mkdtemp(join(tmpdir(), "listen-"));
    await writeFile(join(dir, "loud.wav"), Buffer.from(noiseWav(6, 0.5, 1)));
    await writeFile(join(dir, "quiet.wav"), Buffer.from(noiseWav(6, 0.05, 2)));
    await writeFile(join(dir, "pre.wav"), Buffer.from(noiseWav(2, 0.2, 3)));

    const out = await buildSession(
      {
        name: "t",
        loudnessLufs: -30,
        matchBy: "integrated",
        windows: [
          { start: 1, dur: 2, label: "w0" },
          { start: 3, dur: 2, label: "w1" },
        ],
        variants: {
          loud: { file: "loud.wav" },
          quiet: { file: "quiet.wav" },
          pre: { clips: { 1: "pre.wav" } },
        },
        trials: [
          { window: 0, variants: ["loud", "quiet"] },
          { window: 1, variants: ["loud", "quiet", "pre"] },
        ],
      },
      dir,
      join(dir, "sessions"),
    );

    const session = JSON.parse(await readFile(join(out, "session.json"), "utf8")) as Session;
    const key = JSON.parse(await readFile(join(out, "key.json"), "utf8")) as SessionKey;

    expect(session.trials).toHaveLength(2);
    expect(session.trials[0].clips.map((c) => c.label)).toEqual(["A", "B"]);
    expect(session.trials[1].clips.map((c) => c.label)).toEqual(["A", "B", "C"]);
    // File names leak nothing about the variant.
    for (const t of session.trials) for (const c of t.clips) expect(c.file).toMatch(/^t\d\d_[A-C]\.wav$/);
    // The key maps every blind clip to a variant, and each variant appears once per trial.
    expect(Object.values(key.clips.t01).sort()).toEqual(["loud", "quiet"]);
    expect(Object.values(key.clips.t02).sort()).toEqual(["loud", "pre", "quiet"]);
    expect(key.windows.t01.start).toBe(1);

    const files = (await readdir(out)).filter((f) => f.endsWith(".wav")).sort();
    expect(files).toEqual(["t01_A.wav", "t01_B.wav", "t02_A.wav", "t02_B.wav", "t02_C.wav"]);

    // Every clip is the window length and sits at the target loudness.
    for (const f of files) {
      const raw = await readFile(join(out, f));
      const a = decodeWav(raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength));
      expect(a.length).toBe(2 * SR);
      const lufs = integratedLoudness(preFilter(a.channels, a.sampleRate), a.sampleRate);
      expect(Math.abs(lufs - -30)).toBeLessThan(0.3);
    }
  });

  it("refuses a match that would clip", async () => {
    const dir = await mkdtemp(join(tmpdir(), "listen-"));
    await writeFile(join(dir, "a.wav"), Buffer.from(noiseWav(3, 0.5, 1)));
    await writeFile(join(dir, "b.wav"), Buffer.from(noiseWav(3, 0.5, 2)));
    await expect(
      buildSession(
        {
          name: "t",
          loudnessLufs: -3,
          windows: [{ start: 0, dur: 2 }],
          variants: { a: { file: "a.wav" }, b: { file: "b.wav" } },
        },
        dir,
        join(dir, "sessions"),
      ),
    ).rejects.toThrow(/clips/);
  });
});
