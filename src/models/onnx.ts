/**
 * The model backend: DeepFilterNet3 speech enhancement through ONNX Runtime.
 *
 * The DSP half — transform, ERB filterbank, feature normalisation, mask
 * application, deep filtering, resynthesis — lives in `deepfilternet.ts`,
 * which has no ONNX dependency and is tested without weights. This file is the
 * part that needs the runtime: finding the weights, refusing to run
 * unverified ones, creating sessions, and feeding frames through the graphs.
 *
 * Weights are never bundled. They carry their own licences, they are large,
 * and a local-first tool should let the user decide what to download. Fetch
 * them with `pnpm fetch-model`, which verifies the archive and every extracted
 * file against the checksums pinned below.
 *
 * ## Why the registry describes a set of files
 *
 * DeepFilterNet3's ONNX export is not one model. It is three graphs — an
 * encoder and two decoders — and a `config.ini` stating the transform they
 * were trained through. There is no single-file export upstream, so a registry
 * entry describes a directory of files, each pinned separately. Hashing a
 * tarball would have been less code and worth less: it would not have caught a
 * single swapped decoder inside an otherwise-correct archive.
 *
 * ## Why the runtime is reached through its private binding
 *
 * `onnxruntime-node`'s public API is promise-based, but every stage in this
 * pipeline is a synchronous pure function, and making one stage async would
 * turn the whole chain, the worker and the CLI async for no gain — the work is
 * CPU-bound and single-threaded either way. The native binding underneath is
 * itself synchronous ("a simple synchronized inference session object wrap",
 * in its own words) and the promises in the wrapper are cosmetic, so this
 * calls the binding directly. That is a private path and it may move; the
 * shape is therefore checked at load, and a mismatch makes the backend report
 * itself unavailable, which falls back to the classical suppressor rather than
 * failing the render.
 */

import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { createFftPlan } from "../dsp/fft";
import type { DenoiseBackend, DenoiseRequest, DenoiseResponse } from "./backend";
import {
  analyse,
  applyModel,
  computeFeatures,
  DEFAULT_CHUNK_OPTIONS,
  DFN3_CONFIG,
  erbWidths,
  frameCount,
  runChunked,
  synthesise,
  type DeepFilterConfig,
  type ModelOutput,
} from "./deepfilternet";

export interface ModelFile {
  /** File name inside the model's directory. */
  name: string;
  /** SHA-256 of the file, verified before the model is trusted. */
  sha256: string;
}

export interface ModelSpec {
  id: string;
  /** Directory holding this model's files, under {@link modelDirectory}. */
  directory: string;
  /**
   * The files that make up the model, keyed by the role the runner asks for.
   * Every one is verified; a model is the whole set or nothing.
   */
  files: Record<string, ModelFile>;
  /** Sample rate the model is trained for. Nothing else may be fed to it. */
  sampleRate: number;
  /** Where a user can obtain it, and under what terms. */
  source: string;
  /** The archive `pnpm fetch-model` downloads, pinned by its own hash. */
  archive: { url: string; sha256: string; /** Leading path components to strip. */ strip: number };
  licence: string;
}

/**
 * Known models.
 *
 * The hashes are of the files inside `models/DeepFilterNet3_onnx.tar.gz` in
 * the upstream repository, which is where the project's own author publishes
 * them — deliberately not one of the several community mirrors, which carry
 * the same bytes but not the same provenance.
 */
export const MODELS: ModelSpec[] = [
  {
    id: "deepfilternet3",
    directory: "deepfilternet3",
    files: {
      encoder: {
        name: "enc.onnx",
        sha256: "7c5399d3da8a50ebef1c1a0ae421b33376aa5e45d0e92df16da7e83c9c131916",
      },
      erbDecoder: {
        name: "erb_dec.onnx",
        sha256: "ab669a1d10afe20911728b33053a452071042317a90581092b325da7b2f9d895",
      },
      dfDecoder: {
        name: "df_dec.onnx",
        sha256: "23114ce3b0f6464b763ee62f7bb8aab6b2a129a21eabd5bcfe59413db05f278a",
      },
      // Not read at runtime — the values it states are compiled into
      // DFN3_CONFIG — but pinned so a mismatched export cannot be mistaken
      // for the one those constants were derived from.
      config: {
        name: "config.ini",
        sha256: "415eb925d44990d938fb739f514aa3662c1ec0ea836cff044fa1291b82cb4290",
      },
    },
    sampleRate: 48000,
    source: "https://github.com/Rikorose/DeepFilterNet",
    archive: {
      url: "https://github.com/Rikorose/DeepFilterNet/raw/main/models/DeepFilterNet3_onnx.tar.gz",
      sha256: "c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616",
      strip: 2, // the archive nests everything under tmp/export/
    },
    licence: "MIT / Apache-2.0 (verify before redistributing)",
  },
];

/** Where weights live. Overridable for tests and for a portable install. */
export function modelDirectory(): string {
  if (process.env.AUDIO_LEVELLER_MODELS) return process.env.AUDIO_LEVELLER_MODELS;
  const home = process.env.HOME ?? process.env.USERPROFILE ?? ".";
  return join(home, ".audio-leveller", "models");
}

/** Directory holding one model's files. */
export function modelPath(spec: ModelSpec): string {
  return join(modelDirectory(), spec.directory);
}

/** Path of one file of a model. */
export function modelFilePath(spec: ModelSpec, role: string): string {
  const file = spec.files[role];
  if (!file) throw new Error(`${spec.id} has no file for role "${role}"`);
  return join(modelPath(spec), file.name);
}

export type ChecksumResult = { ok: true } | { ok: false; reason: string };

/**
 * Verify one file against its expected hash.
 *
 * An empty expected hash is a failure, not a pass. Weights are executable
 * intent: a registry entry nobody has pinned yet must not act as a wildcard
 * that accepts whatever happens to be sitting at that path.
 */
export function verifyChecksum(file: ModelFile, path: string): ChecksumResult {
  if (!file.sha256) {
    return { ok: false, reason: `no checksum pinned for ${file.name}; refusing to load unverified weights` };
  }
  if (!existsSync(path)) return { ok: false, reason: `not found at ${path}` };

  const digest = createHash("sha256").update(readFileSync(path)).digest("hex");
  if (digest !== file.sha256) {
    return { ok: false, reason: `checksum mismatch for ${file.name} (got ${digest.slice(0, 16)}…)` };
  }
  return { ok: true };
}

/**
 * Verify every file of a model. A model is all of its files or none of them —
 * an encoder that matches paired with a decoder that does not is not a
 * partially usable model, it is an unknown one.
 */
export function verifyModel(spec: ModelSpec): ChecksumResult {
  for (const role of Object.keys(spec.files)) {
    const result = verifyChecksum(spec.files[role], modelFilePath(spec, role));
    if (!result.ok) return { ok: false, reason: `${spec.id}: ${result.reason}` };
  }
  return { ok: true };
}

/** Total size of a model's files on disk, for reporting. Zero when absent. */
export function modelSize(spec: ModelSpec): number {
  let total = 0;
  for (const role of Object.keys(spec.files)) {
    const path = modelFilePath(spec, role);
    if (existsSync(path)) total += statSync(path).size;
  }
  return total;
}

// --- the ONNX Runtime, reached synchronously -------------------------------

interface NativeSession {
  loadModel(path: string, options: Record<string, unknown>): void;
  run(
    feeds: Record<string, unknown>,
    fetches: Record<string, null>,
    options: Record<string, unknown>,
  ): Record<string, { data: Float32Array; dims: number[] }>;
}

interface Runtime {
  createSession(path: string): NativeSession;
  tensor(data: Float32Array, dims: number[]): unknown;
}

let runtimeCache: Runtime | null | undefined;
let runtimeError = "";

/**
 * Load the runtime, or explain why it could not be loaded.
 *
 * Cached because the answer cannot change within a process, and because
 * `available()` is called on every render.
 */
function runtime(): Runtime | null {
  if (runtimeCache !== undefined) return runtimeCache;
  runtimeCache = null;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const ort = require("onnxruntime-node") as { Tensor: new (t: string, d: Float32Array, dims: number[]) => unknown };
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const bindings = require("onnxruntime-node/dist/binding") as {
      binding?: { InferenceSession?: new () => NativeSession };
      initOrt?: () => void;
    };
    const Session = bindings.binding?.InferenceSession;
    if (typeof Session !== "function" || typeof bindings.initOrt !== "function") {
      runtimeError =
        "onnxruntime-node is installed but its synchronous binding is not where this " +
        "expects it (dist/binding); the package layout has changed";
      return null;
    }
    bindings.initOrt();
    runtimeCache = {
      createSession(path: string): NativeSession {
        const session = new Session();
        session.loadModel(path, {});
        return session;
      },
      tensor: (data, dims) => new ort.Tensor("float32", data, dims),
    };
  } catch (err) {
    runtimeError = `onnxruntime-node is not installed (optional dependency): ${
      err instanceof Error ? err.message : String(err)
    }`;
    runtimeCache = null;
  }
  return runtimeCache;
}

/** Is `onnxruntime-node` installed and usable? It is an optional dependency. */
export function runtimeAvailable(): boolean {
  return runtime() !== null;
}

interface Sessions {
  encoder: NativeSession;
  erbDecoder: NativeSession;
  dfDecoder: NativeSession;
}

const sessionCache = new Map<string, Sessions>();

function sessionsFor(spec: ModelSpec, rt: Runtime): Sessions {
  const cached = sessionCache.get(spec.id);
  if (cached) return cached;
  const sessions: Sessions = {
    encoder: rt.createSession(modelFilePath(spec, "encoder")),
    erbDecoder: rt.createSession(modelFilePath(spec, "erbDecoder")),
    dfDecoder: rt.createSession(modelFilePath(spec, "dfDecoder")),
  };
  sessionCache.set(spec.id, sessions);
  return sessions;
}

/** Run the three graphs over one span of frames. */
function runSpan(
  sessions: Sessions,
  erb: Float32Array,
  spec: Float32Array,
  frames: number,
  config: DeepFilterConfig,
  rt: Runtime,
): ModelOutput {
  const encoded = sessions.encoder.run(
    {
      feat_erb: rt.tensor(erb, [1, 1, frames, config.nbErb]),
      feat_spec: rt.tensor(spec, [1, 2, frames, config.nbDf]),
    },
    { e0: null, e1: null, e2: null, e3: null, emb: null, c0: null, lsnr: null },
    {},
  );

  const mask = sessions.erbDecoder.run(
    {
      emb: rt.tensor(encoded.emb.data, encoded.emb.dims),
      e3: rt.tensor(encoded.e3.data, encoded.e3.dims),
      e2: rt.tensor(encoded.e2.data, encoded.e2.dims),
      e1: rt.tensor(encoded.e1.data, encoded.e1.dims),
      e0: rt.tensor(encoded.e0.data, encoded.e0.dims),
    },
    { m: null },
    {},
  );

  const filtered = sessions.dfDecoder.run(
    {
      emb: rt.tensor(encoded.emb.data, encoded.emb.dims),
      c0: rt.tensor(encoded.c0.data, encoded.c0.dims),
    },
    { coefs: null },
    {},
  );

  return { gains: mask.m.data, coefs: filtered.coefs.data, lsnr: encoded.lsnr.data };
}

/** What the backend reports about a run, beyond the audio. */
export interface OnnxInfo extends Record<string, unknown> {
  model: string;
  frames: number;
  /** Frames the local-SNR gate judged worth masking. */
  framesGained: number;
  /** Frames that also got the deep filter. */
  framesDeepFiltered: number;
  /** Frames judged clean enough to pass through untouched. */
  framesLeftAlone: number;
  /** The attenuation limit the target reduction was turned into, in dB. */
  attenuationLimitDb: number;
  meanLsnrDb: number;
}

function findModel(sampleRate: number): ModelSpec | undefined {
  return MODELS.find((m) => m.sampleRate === sampleRate);
}

export const onnxBackend: DenoiseBackend = {
  name: "onnx",
  description: "DeepFilterNet3 via ONNX Runtime (needs weights and onnxruntime-node)",

  /**
   * Higher than the stage's default, because this backend is measurably
   * gentler on material that barely needs it.
   *
   * The 35 dB default was derived from the classical suppressor, which has no
   * idea what speech is and reshapes it whether or not there was anything to
   * remove. This model decides frame by frame: on a real recording measuring
   * 38.5 dB it leaves 54 % of frames untouched outright, and forcing it to run
   * costs 0.00 dB of programme loudness and returns 42.8 dB SI-SDR against the
   * original — where the classical backend on the same material returns 28.4.
   * At 45 dB a recording like that gets 6.5 dB of reduction rather than none,
   * and a genuinely pristine one still gets nothing.
   *
   * Raising it is only safe because the stage discards a backend's output when
   * it costs too much programme loudness: on synthetic speech, which this
   * model misreads, the taper no longer hides the problem and the guard is
   * what catches it instead.
   */
  cleanSnrDb: 45,

  available(sampleRate: number): boolean {
    return this.unavailableReason(sampleRate) === null;
  },

  unavailableReason(sampleRate: number): string | null {
    if (!runtimeAvailable()) return runtimeError;
    const spec = findModel(sampleRate);
    if (!spec) {
      // The model is trained at one rate and resampling into and out of it
      // inside the stage would spend two conversions to reach a denoiser,
      // which is not obviously better than the classical one that needs
      // neither. So a 44.1 kHz file gets the spectral backend and says so.
      return `no registered model runs at ${sampleRate} Hz`;
    }
    const check = verifyModel(spec);
    return check.ok ? null : check.reason;
  },

  process(request: DenoiseRequest): DenoiseResponse {
    const spec = findModel(request.sampleRate);
    if (!spec) throw new Error(`no model for ${request.sampleRate} Hz`);

    const check = verifyModel(spec);
    if (!check.ok) throw new Error(check.reason);

    const rt = runtime();
    if (!rt) throw new Error(runtimeError);

    const config = DFN3_CONFIG;
    const sessions = sessionsFor(spec, rt);
    const widths = erbWidths(config);
    const plan = createFftPlan(config.fftSize);
    const length = request.channels[0]?.length ?? 0;

    // Frames covering the signal, plus `lookahead` more so the last frames get
    // a model output at all: the estimate for frame t is emitted at t + 2.
    const covering = frameCount(length, config.hopSize);
    const total = covering + config.lookahead;

    const channels: Float32Array[] = [];
    let framesGained = 0;
    let framesDeepFiltered = 0;
    let framesLeftAlone = 0;
    let lsnrSum = 0;
    let lsnrCount = 0;

    for (const channel of request.channels) {
      const spectrogram = analyse(channel, total, config, plan);
      const features = computeFeatures(spectrogram, config, widths);
      const model = runChunked(features, config, DEFAULT_CHUNK_OPTIONS, (erb, spec, frames) =>
        runSpan(sessions, erb, spec, frames, config, rt),
      );

      const applied = applyModel(
        spectrogram.frames.slice(0, covering),
        model,
        config,
        widths,
        request.reductionDb,
      );
      channels.push(synthesise(applied.frames, length, config, plan));

      framesGained += applied.framesGained;
      framesDeepFiltered += applied.framesDeepFiltered;
      framesLeftAlone += applied.framesLeftAlone;
      for (const v of model.lsnr) {
        lsnrSum += v;
        lsnrCount++;
      }
    }

    const info: OnnxInfo = {
      model: spec.id,
      frames: covering,
      framesGained,
      framesDeepFiltered,
      framesLeftAlone,
      attenuationLimitDb: request.reductionDb,
      meanLsnrDb: lsnrCount > 0 ? lsnrSum / lsnrCount : NaN,
    };

    return { channels, info };
  },
};
