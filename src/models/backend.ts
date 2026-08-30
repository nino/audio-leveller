/**
 * Pluggable denoise backends.
 *
 * There are two kinds of noise reduction worth having and they are not
 * interchangeable. A classical spectral suppressor knows nothing about speech;
 * it removes what is *stationary*, which covers hiss, hum, fan and traffic,
 * and it cannot invent detail because it only ever attenuates. A trained model
 * (DeepFilterNet and its relatives) knows what speech looks like and removes
 * non-stationary noise the classical method cannot touch – at the cost of
 * being able to hallucinate plausible speech detail that was never there.
 *
 * So the interface is deliberately narrow and both live behind it: the
 * classical backend is always present and always works, the model backend is
 * better when it is available, and the stage records which one ran. That
 * matters for a local tool – a user without the weights should still get a
 * working denoiser rather than a stage that silently does nothing.
 */

import type { SampleRange } from "../dsp/denoise";

export interface DenoiseRequest {
  channels: Float32Array[];
  sampleRate: number;
  /** Ranges believed to hold no speech, from the silence analysis. */
  pauses: SampleRange[];
  /** How far to push the noise floor down, in dB. */
  reductionDb: number;
}

export interface DenoiseResponse {
  channels: Float32Array[];
  /** Backend-specific detail for the report, JSON-serialisable. */
  info: Record<string, unknown>;
}

export interface DenoiseBackend {
  readonly name: string;
  readonly description: string;
  /**
   * Programme-to-floor distance, in dB, above which *this* backend has nothing
   * left to offer and should be tapered to nothing. Falls back to the stage's
   * own parameter when a backend does not state one.
   *
   * It belongs to the backend rather than the stage because it is a statement
   * about that backend's transparency, and the two here are not equally
   * transparent. The threshold is the point past which the cure costs more
   * than the disease, and that point is different for a suppressor with no
   * idea what speech is than for a model that decides frame by frame whether
   * a frame is worth touching.
   */
  readonly cleanSnrDb?: number;
  /**
   * Whether this backend can run right now. A backend that needs weights is
   * unavailable until they are on disk; `unavailableReason` says why so the
   * report can be honest instead of silently falling back.
   */
  available(sampleRate: number): boolean;
  unavailableReason(sampleRate: number): string | null;
  process(request: DenoiseRequest): DenoiseResponse;
}

const backends = new Map<string, DenoiseBackend>();

export function registerBackend(backend: DenoiseBackend): void {
  backends.set(backend.name, backend);
}

export function registeredBackends(): DenoiseBackend[] {
  return [...backends.values()];
}

export interface BackendResolution {
  backend: DenoiseBackend | null;
  /** Backends that were preferred but could not run, and why. */
  skipped: { name: string; reason: string }[];
}

/**
 * Pick the first backend in `preference` order that can actually run,
 * recording what was skipped and why.
 */
export function resolveBackend(preference: string[], sampleRate: number): BackendResolution {
  const skipped: { name: string; reason: string }[] = [];

  for (const name of preference) {
    const backend = backends.get(name);
    if (!backend) {
      skipped.push({ name, reason: "not registered" });
      continue;
    }
    if (!backend.available(sampleRate)) {
      skipped.push({ name, reason: backend.unavailableReason(sampleRate) ?? "unavailable" });
      continue;
    }
    return { backend, skipped };
  }

  return { backend: null, skipped };
}
