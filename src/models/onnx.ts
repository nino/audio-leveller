/**
 * The model backend: DeepFilterNet-class speech enhancement through ONNX
 * Runtime.
 *
 * **Status: wired but unverified.** The plumbing here — discovery, checksum
 * verification, session creation, chunked inference — is written and unit
 * tested against its own failure paths, but it has never been run against real
 * weights, because the environment this was developed in cannot reach the
 * hosts that serve them. Treat the inference path as untested code until
 * someone runs it with a model present. It is deliberately built so that its
 * absence is loud (`unavailableReason` says exactly what is missing) rather
 * than silently degrading to a no-op.
 *
 * Weights are never bundled. They carry their own licences, they are large,
 * and a local-first tool should let the user decide what to download. The
 * expected layout is a directory (see `modelDirectory`) holding the `.onnx`
 * file named in the registry below.
 */

import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { DenoiseBackend, DenoiseRequest, DenoiseResponse } from "./backend";

export interface ModelSpec {
  id: string;
  /** File name expected inside the model directory. */
  file: string;
  /** Sample rate the model is trained for. Nothing else may be fed to it. */
  sampleRate: number;
  /** SHA-256 of the weights, verified before the model is trusted. */
  sha256: string;
  /** Where a user can obtain it, and under what terms. */
  source: string;
  licence: string;
}

/**
 * Known models. The checksums are placeholders until the files have actually
 * been fetched and hashed — `verifyChecksum` therefore refuses to pass a model
 * whose expected hash is still empty, rather than accepting anything.
 */
export const MODELS: ModelSpec[] = [
  {
    id: "deepfilternet3",
    file: "deepfilternet3.onnx",
    sampleRate: 48000,
    sha256: "",
    source: "https://github.com/Rikorose/DeepFilterNet",
    licence: "MIT / Apache-2.0 (verify before redistributing)",
  },
];

/** Where weights live. Overridable for tests and for a portable install. */
export function modelDirectory(): string {
  if (process.env.AUDIO_LEVELLER_MODELS) return process.env.AUDIO_LEVELLER_MODELS;
  const home = process.env.HOME ?? process.env.USERPROFILE ?? ".";
  return join(home, ".audio-leveller", "models");
}

export function modelPath(spec: ModelSpec): string {
  return join(modelDirectory(), spec.file);
}

export type ChecksumResult =
  | { ok: true }
  | { ok: false; reason: string };

/**
 * Verify a model file against its expected hash.
 *
 * An empty expected hash is a failure, not a pass. Weights are executable
 * intent: a registry entry nobody has pinned yet must not act as a wildcard
 * that accepts whatever happens to be sitting at that path.
 */
export function verifyChecksum(spec: ModelSpec, path = modelPath(spec)): ChecksumResult {
  if (!spec.sha256) {
    return { ok: false, reason: `no checksum pinned for ${spec.id}; refusing to load unverified weights` };
  }
  if (!existsSync(path)) return { ok: false, reason: `not found at ${path}` };

  const digest = createHash("sha256").update(readFileSync(path)).digest("hex");
  if (digest !== spec.sha256) {
    return { ok: false, reason: `checksum mismatch for ${spec.id} (got ${digest.slice(0, 16)}…)` };
  }
  return { ok: true };
}

/** Is `onnxruntime-node` installed? It is an optional dependency. */
export function runtimeAvailable(): boolean {
  try {
    require.resolve("onnxruntime-node");
    return true;
  } catch {
    return false;
  }
}

function findModel(sampleRate: number): ModelSpec | undefined {
  return MODELS.find((m) => m.sampleRate === sampleRate);
}

export const onnxBackend: DenoiseBackend = {
  name: "onnx",
  description: "DeepFilterNet-class model via ONNX Runtime (needs weights and onnxruntime-node)",

  available(sampleRate: number): boolean {
    return this.unavailableReason(sampleRate) === null;
  },

  unavailableReason(sampleRate: number): string | null {
    if (!runtimeAvailable()) {
      return "onnxruntime-node is not installed (optional dependency)";
    }
    const spec = findModel(sampleRate);
    if (!spec) {
      // The pipeline converts to a stage's required rate, so this means the
      // registry has no model at all for the rate the stage asked for.
      return `no registered model runs at ${sampleRate} Hz`;
    }
    const check = verifyChecksum(spec);
    return check.ok ? null : check.reason;
  },

  process(request: DenoiseRequest): DenoiseResponse {
    const spec = findModel(request.sampleRate);
    if (!spec) throw new Error(`no model for ${request.sampleRate} Hz`);

    const check = verifyChecksum(spec);
    if (!check.ok) throw new Error(check.reason);

    // Deliberately not implemented behind a silent fallback: reaching here
    // means the weights verified, and running them is the next piece of work.
    // Failing loudly is the correct behaviour for a path that has never been
    // executed against a real model.
    throw new Error(
      `ONNX inference for ${spec.id} is not implemented yet — the weights verified ` +
        `at ${modelPath(spec)}, but the inference path has never been run. ` +
        `Use the "spectral" backend until it has been.`,
    );
  },
};

/** File size of a model on disk, for reporting. Zero when absent. */
export function modelSize(spec: ModelSpec): number {
  const path = modelPath(spec);
  return existsSync(path) ? statSync(path).size : 0;
}
