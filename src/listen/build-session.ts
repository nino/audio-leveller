/**
 * Build a blind listening session from a spec.
 *
 *   node dist/listen/build-session.js <spec.json> [--out listening/sessions]
 *
 * For every trial: cut the window from each variant (or take its pre-cut
 * clip), match all of them to the same loudness with a static gain, shuffle,
 * and write them as `<trial>_<letter>.wav` so file names leak nothing. The
 * mapping goes to `key.json`; the app shows it only on request.
 *
 * Loudness matching matters more than it looks: a 1 dB louder clip reliably
 * "sounds better", and the whole point of the exercise is to hear the
 * processing, not the level.
 */

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { decodeWav, encodeWav } from "../dsp/wav";
import { integratedLoudness, loudnessOfRange, preFilter } from "../dsp/loudness";
import { truePeakDbfs } from "../dsp/truepeak";
import {
  DEFAULT_CLIP_QUESTIONS,
  DEFAULT_TRIAL_QUESTIONS,
  type Session,
  type SessionKey,
  type SessionSpec,
  type Trial,
} from "./types";

const LETTERS = "ABCDEFGHIJ";

interface Loaded {
  sampleRate: number;
  channels: Float32Array[];
}

const cache = new Map<string, Loaded>();

async function load(path: string): Promise<Loaded> {
  const hit = cache.get(path);
  if (hit) return hit;
  const raw = await readFile(path);
  const audio = decodeWav(raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength));
  const loaded = { sampleRate: audio.sampleRate, channels: audio.channels };
  cache.set(path, loaded);
  return loaded;
}

function cut(audio: Loaded, start: number, dur: number): Loaded {
  const s0 = Math.round(start * audio.sampleRate);
  const s1 = Math.min(audio.channels[0].length, Math.round((start + dur) * audio.sampleRate));
  if (s1 <= s0) throw new Error(`Window ${start}s+${dur}s lies outside the file`);
  return { sampleRate: audio.sampleRate, channels: audio.channels.map((c) => c.slice(s0, s1)) };
}

type MatchBy = NonNullable<SessionSpec["matchBy"]>;

/** Loudest `windowSec` window (100 ms hop) of pre-filtered audio, in LUFS. */
function maxWindowLoudness(filtered: Float32Array[], sampleRate: number, windowSec: number): number {
  const win = Math.round(windowSec * sampleRate);
  const hop = Math.round(0.1 * sampleRate);
  const length = filtered[0]?.length ?? 0;
  if (length <= win) return loudnessOfRange(filtered, 0, length);
  let max = -Infinity;
  for (let s = 0; s + win <= length; s += hop) {
    const l = loudnessOfRange(filtered, s, s + win);
    if (l > max) max = l;
  }
  return max;
}

export function measure(audio: Loaded, by: MatchBy): number {
  const filtered = preFilter(audio.channels, audio.sampleRate);
  if (by === "integrated") return integratedLoudness(filtered, audio.sampleRate);
  return maxWindowLoudness(filtered, audio.sampleRate, by === "momentaryMax" ? 0.4 : 3);
}

/** Static gain to `targetLufs`; returns the true peak afterwards for a clipping check. */
function match(audio: Loaded, targetLufs: number, by: MatchBy): { channels: Float32Array[]; peakDb: number } {
  const measured = measure(audio, by);
  const g = Number.isFinite(measured) ? Math.pow(10, (targetLufs - measured) / 20) : 1;
  const channels = audio.channels.map((c) => {
    const out = new Float32Array(c.length);
    for (let i = 0; i < c.length; i++) out[i] = c[i] * g;
    return out;
  });
  return { channels, peakDb: truePeakDbfs(channels) };
}

/** Deterministic-enough shuffle; we don't need cryptographic quality, just no bias. */
function shuffle<T>(items: T[]): T[] {
  const out = items.slice();
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

function chunk<T>(items: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}

export async function buildSession(spec: SessionSpec, specDir: string, outRoot: string): Promise<string> {
  const loudnessLufs = spec.loudnessLufs ?? -28;
  const matchBy: MatchBy = spec.matchBy ?? "momentaryMax";
  const maxPerTrial = spec.maxClipsPerTrial ?? 4;
  const outDir = join(outRoot, spec.name);
  await mkdir(outDir, { recursive: true });

  // Rebuilding an existing session (say, with a different loudness match)
  // must not reshuffle: results already recorded refer to the old letters.
  let previous: SessionKey | null = null;
  try {
    previous = JSON.parse(await readFile(join(outDir, "key.json"), "utf8")) as SessionKey;
  } catch {
    previous = null;
  }

  const trialSpecs: NonNullable<SessionSpec["trials"]> =
    spec.trials ??
    spec.windows.flatMap((_, w) =>
      chunk(Object.keys(spec.variants), maxPerTrial).map((variants) => ({ window: w, variants })),
    );

  const trials: Trial[] = [];
  const key: SessionKey = { name: spec.name, variants: {}, clips: {}, windows: {} };
  for (const [id, v] of Object.entries(spec.variants)) {
    key.variants[id] = {
      file: v.file ?? v.clip ?? Object.values(v.clips ?? {}).join(", "),
      description: v.description,
    };
  }

  for (let t = 0; t < trialSpecs.length; t++) {
    const ts = trialSpecs[t];
    const window = spec.windows[ts.window];
    if (!window) throw new Error(`Trial ${t} refers to unknown window ${ts.window}`);
    if (ts.variants.length < 2 || ts.variants.length > LETTERS.length) {
      throw new Error(`Trial ${t} needs 2–${LETTERS.length} variants, got ${ts.variants.length}`);
    }
    const trialId = `t${String(t + 1).padStart(2, "0")}`;
    const kept = previous?.clips[trialId];
    const sameSet =
      kept && Object.values(kept).slice().sort().join("|") === ts.variants.slice().sort().join("|");
    const order = sameSet
      ? Object.keys(kept).sort().map((label) => kept[label])
      : shuffle(ts.variants);
    key.clips[trialId] = {};
    key.windows[trialId] = window;
    const clips = [];

    for (let i = 0; i < order.length; i++) {
      const variantId = order[i];
      const variant = spec.variants[variantId];
      if (!variant) throw new Error(`Trial ${t} refers to unknown variant "${variantId}"`);
      const precut = variant.clip ?? variant.clips?.[ts.window];
      if (!precut && !variant.file) {
        throw new Error(`Variant "${variantId}" has no file, and no clip for window ${ts.window}`);
      }
      const source = await load(resolve(specDir, precut ?? variant.file ?? ""));
      const excerpt = precut ? source : cut(source, window.start, window.dur);
      const { channels, peakDb } = match(excerpt, loudnessLufs, matchBy);
      if (peakDb > -0.1) {
        throw new Error(
          `Clip ${trialId}/${variantId} peaks at ${peakDb.toFixed(1)} dBTP after matching to ` +
            `${loudnessLufs} LUFS – lower loudnessLufs so nothing clips`,
        );
      }
      const label = LETTERS[i];
      const file = `${trialId}_${label}.wav`;
      await writeFile(
        join(outDir, file),
        Buffer.from(
          encodeWav({
            sampleRate: excerpt.sampleRate,
            channels,
            length: channels[0].length,
            bitDepth: 24,
            format: "int",
          }),
        ),
      );
      key.clips[trialId][label] = variantId;
      clips.push({ label, file });
    }

    trials.push({
      id: trialId,
      title: ts.title ?? `${window.label ?? "passage"} · ${window.start}s (${window.dur}s)`,
      clips,
    });
    process.stderr.write(
      `${trialId}: ${order.length} clips from ${window.start}s${sameSet ? " (kept previous order)" : ""}\n`,
    );
  }

  const session: Session = {
    name: spec.name,
    createdAt: new Date().toISOString(),
    loudnessLufs,
    matchBy,
    clipQuestions: spec.clipQuestions ?? DEFAULT_CLIP_QUESTIONS,
    trialQuestions: spec.trialQuestions ?? DEFAULT_TRIAL_QUESTIONS,
    trials,
  };
  await writeFile(join(outDir, "session.json"), JSON.stringify(session, null, 2));
  await writeFile(join(outDir, "key.json"), JSON.stringify(key, null, 2));
  return outDir;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  const specPath = args.find((a) => !a.startsWith("--"));
  if (!specPath) {
    console.error("usage: build-session <spec.json> [--out listening/sessions]");
    process.exit(1);
  }
  const outIdx = args.indexOf("--out");
  const outRoot = outIdx >= 0 ? args[outIdx + 1] : "listening/sessions";
  const spec = JSON.parse(await readFile(specPath, "utf8")) as SessionSpec;
  const dir = await buildSession(spec, dirname(resolve(specPath)), outRoot);
  console.log(`Wrote session "${basename(dir)}" to ${dir}`);
}

if (process.argv[1] && /build-session\.js$/.test(process.argv[1])) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
