import { describe, it, expect } from "vitest";
import { denoise, estimateNoiseProfile, DEFAULT_DENOISE_OPTIONS } from "../src/dsp/denoise";
import { stft } from "../src/dsp/stft";
import { syntheticSpeech, addNoise, cloneSignal, rng } from "../src/eval/signals";
import { siSdrDb } from "../src/eval/metrics";
import { loudnessOfRange, preFilter } from "../src/dsp/loudness";
import { resolveBackend, registeredBackends, registerBackend, type DenoiseBackend } from "../src/models";
import { denoiseStage, DEFAULT_DENOISE_PARAMS } from "../src/stages/denoise";
import { Analyzer } from "../src/pipeline/analysis";
import { verifyChecksum, verifyModel, MODELS, modelFilePath } from "../src/models/onnx";
import type { Signal } from "../src/pipeline/types";

const sr = 48000;

const cache = new Map<string, ReturnType<typeof syntheticSpeech>>();
function speech(floorDbfs = -55) {
  const key = String(floorDbfs);
  let built = cache.get(key);
  if (!built) {
    built = syntheticSpeech({
      sampleRate: sr,
      segments: [
        { seconds: 3, levelLufs: -23, f0: 110 },
        { seconds: 3, levelLufs: -23, f0: 130 },
      ],
      pauseSec: 1.5,
      floorDbfs,
      seed: 4242,
    });
    cache.set(key, built);
  }
  return built;
}

/** Pause ranges between and around the speech segments. */
function pausesOf(built: ReturnType<typeof syntheticSpeech>) {
  const pauses: { start: number; end: number }[] = [];
  let at = 0;
  for (const seg of built.segments) {
    if (seg.start > at) pauses.push({ start: at, end: seg.start });
    at = seg.end;
  }
  if (at < built.signal.length) pauses.push({ start: at, end: built.signal.length });
  return pauses;
}

/** Loudness of a channel over a range, ungated. */
function levelOf(signal: Signal, range: { start: number; end: number }): number {
  return loudnessOfRange(preFilter(signal.channels, signal.sampleRate), range.start, range.end);
}

/** Mean noise-floor level across the pauses. */
function floorOf(signal: Signal, pauses: { start: number; end: number }[]): number {
  const values = pauses.map((p) => levelOf(signal, p)).filter(Number.isFinite);
  return values.reduce((a, b) => a + b, 0) / Math.max(1, values.length);
}

describe("noise profile", () => {
  it("measures the floor from pauses when it can", () => {
    const built = speech();
    const analysis = stft(built.signal.channels[0]);
    const profile = estimateNoiseProfile(analysis, pausesOf(built));

    expect(profile.fromPauses).toBe(true);
    expect(profile.frames).toBeGreaterThan(4);
    expect(profile.power.every((p) => Number.isFinite(p) && p >= 0)).toBe(true);
  });

  it("falls back to minimum statistics with no usable pauses, and says so", () => {
    const built = speech();
    const analysis = stft(built.signal.channels[0]);
    const profile = estimateNoiseProfile(analysis, []);

    expect(profile.fromPauses).toBe(false);
    expect(profile.frames).toBeGreaterThan(0);
  });

  it("tracks a louder noise floor", () => {
    const quiet = speech(-70);
    const loud = speech(-45);
    const quietProfile = estimateNoiseProfile(stft(quiet.signal.channels[0]), pausesOf(quiet));
    const loudProfile = estimateNoiseProfile(stft(loud.signal.channels[0]), pausesOf(loud));

    const mean = (p: Float64Array): number => p.reduce((a, b) => a + b, 0) / p.length;
    expect(mean(loudProfile.power)).toBeGreaterThan(mean(quietProfile.power) * 10);
  });
});

describe("denoise", () => {
  it("lowers the noise floor by roughly the amount asked for", () => {
    const built = speech();
    const noisy = addNoise(built.signal, 18, 991);
    const pauses = pausesOf(built);

    for (const reductionDb of [6, 12]) {
      const result = denoise(noisy.channels, pauses, { reductionDb });
      const cleaned: Signal = { ...noisy, channels: result.channels };
      const achieved = floorOf(noisy, pauses) - floorOf(cleaned, pauses);

      // Never more than asked (the floor is a hard limit), and most of it.
      expect(achieved, `${reductionDb} dB`).toBeLessThanOrEqual(reductionDb + 1);
      expect(achieved, `${reductionDb} dB`).toBeGreaterThan(reductionDb * 0.5);
    }
  });

  it("improves SI-SDR against the clean reference", () => {
    const built = speech();
    const noisy = addNoise(built.signal, 12, 991);
    const result = denoise(noisy.channels, pausesOf(built), { reductionDb: 12 });
    const cleaned: Signal = { ...noisy, channels: result.channels };

    const before = siSdrDb(built.signal, noisy);
    const after = siSdrDb(built.signal, cleaned);
    expect(after).toBeGreaterThan(before);
  });

  it("attenuates noise far more than speech", () => {
    // The whole point: a denoiser that pulls both down equally is a fader.
    const built = speech();
    const noisy = addNoise(built.signal, 15, 55);
    const result = denoise(noisy.channels, pausesOf(built), { reductionDb: 12 });

    expect(result.meanNoiseGainDb).toBeLessThan(-4);
    expect(result.meanSpeechGainDb).toBeGreaterThan(-1.5);
  });

  it("leaves the speech level essentially unchanged", () => {
    const built = speech();
    const noisy = addNoise(built.signal, 18, 991);
    const result = denoise(noisy.channels, pausesOf(built), { reductionDb: 12 });
    const cleaned: Signal = { ...noisy, channels: result.channels };

    for (const seg of built.segments) {
      const before = levelOf(noisy, { start: seg.start, end: seg.end });
      const after = levelOf(cleaned, { start: seg.start, end: seg.end });
      expect(Math.abs(after - before)).toBeLessThan(2);
    }
  });

  it("is nearly transparent on material with almost no noise", () => {
    const built = speech(-85);
    const result = denoise(built.signal.channels, pausesOf(built), { reductionDb: 12 });
    const cleaned: Signal = { ...built.signal, channels: result.channels };

    // Nothing to remove, so the speech must survive intact.
    expect(siSdrDb(built.signal, cleaned)).toBeGreaterThan(15);
  });

  it("respects a zero-reduction request as a no-op", () => {
    const built = speech();
    const result = denoise(built.signal.channels, pausesOf(built), { reductionDb: 0 });
    // A 0 dB floor means every gain is clamped to 1.
    for (let i = 0; i < built.signal.length; i += 97) {
      expect(result.channels[0][i]).toBeCloseTo(built.signal.channels[0][i], 5);
    }
  });

  it("handles channels independently", () => {
    const built = speech();
    const noisy = addNoise(built.signal, 15, 3);
    const stereo = cloneSignal(noisy);
    stereo.channels.push(Float32Array.from(built.signal.channels[0])); // clean right

    const result = denoise(stereo.channels, pausesOf(built), { reductionDb: 12 });
    expect(result.channels).toHaveLength(2);
    expect(Array.from(result.channels[0])).not.toEqual(Array.from(result.channels[1]));
  });

  it("does not produce NaN or infinities on silence", () => {
    const silent = [new Float32Array(sr)];
    const result = denoise(silent, [{ start: 0, end: sr }], DEFAULT_DENOISE_OPTIONS);
    expect(result.channels[0].every((v) => Number.isFinite(v))).toBe(true);
  });

  it("is deterministic", () => {
    const built = speech();
    const noisy = addNoise(built.signal, 15, 7);
    const a = denoise(noisy.channels, pausesOf(built), { reductionDb: 12 });
    const b = denoise(noisy.channels, pausesOf(built), { reductionDb: 12 });
    expect(Array.from(a.channels[0])).toEqual(Array.from(b.channels[0]));
  });

  it("suppresses musical noise — the residual stays smooth across frequency", () => {
    // Musical noise is isolated surviving bins. Measured here as the spread of
    // per-bin levels in a pause after processing: a field of holes and spikes
    // has a much larger spread than the smooth hiss it replaced.
    const built = speech(-50);
    const next = rng(21);
    const noisy = cloneSignal(built.signal);
    for (const ch of noisy.channels) {
      for (let i = 0; i < ch.length; i++) ch[i] += (next() * 2 - 1) * 0.004;
    }

    const pauses = pausesOf(built);
    const result = denoise(noisy.channels, pauses, { reductionDb: 12 });

    const pause = pauses[1];
    const framesOf = (channel: Float32Array): number[] => {
      const analysis = stft(channel.slice(pause.start, pause.end));
      const mid = analysis.frames[Math.floor(analysis.frames.length / 2)];
      const levels: number[] = [];
      for (let k = 4; k < 200; k++) {
        const re = mid[2 * k];
        const im = mid[2 * k + 1];
        levels.push(10 * Math.log10(re * re + im * im + 1e-30));
      }
      return levels;
    };

    const spread = (values: number[]): number => {
      const mean = values.reduce((a, b) => a + b, 0) / values.length;
      return Math.sqrt(values.reduce((a, b) => a + (b - mean) ** 2, 0) / values.length);
    };

    // Some increase is inherent — attenuating selectively is the job — but the
    // residual must not become dramatically spikier than the input noise.
    expect(spread(framesOf(result.channels[0]))).toBeLessThan(
      spread(framesOf(noisy.channels[0])) * 2,
    );
  });
});

describe("backends", () => {
  it("registers the classical backend and always resolves to something", () => {
    expect(registeredBackends().map((b) => b.name)).toContain("spectral");
    const { backend } = resolveBackend(["onnx", "spectral"], sr);
    expect(backend).not.toBeNull();
  });

  it("explains why a backend was skipped rather than failing silently", () => {
    const { backend, skipped } = resolveBackend(["onnx", "spectral"], sr);
    // In any environment without verified weights, onnx is skipped with a reason.
    if (backend?.name === "spectral") {
      expect(skipped.some((s) => s.name === "onnx" && s.reason.length > 0)).toBe(true);
    }
  });

  it("reports an unknown backend name instead of ignoring it", () => {
    const { skipped } = resolveBackend(["nope", "spectral"], sr);
    expect(skipped).toContainEqual({ name: "nope", reason: "not registered" });
  });

  it("refuses weights with no pinned checksum", () => {
    // A registry entry nobody has pinned must not act as a wildcard that
    // accepts whatever happens to sit at that path.
    const unpinned = { name: "enc.onnx", sha256: "" };
    const result = verifyChecksum(unpinned, modelFilePath(MODELS[0], "encoder"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toMatch(/no checksum pinned/);
  });

  it("reports a missing model file", () => {
    const pinned = { name: "enc.onnx", sha256: "a".repeat(64) };
    const result = verifyChecksum(pinned, "/nonexistent/model.onnx");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toMatch(/not found/);
  });

  it("verifies every file of a model, not just the first", () => {
    // A model is the whole set or nothing: an encoder that matches paired
    // with a decoder that does not is an unknown model, not a usable one.
    const spec = MODELS[0];
    expect(Object.keys(spec.files).length).toBeGreaterThan(1);
    const broken = {
      ...spec,
      files: { ...spec.files, dfDecoder: { ...spec.files.dfDecoder, sha256: "b".repeat(64) } },
    };
    const result = verifyModel(broken);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toMatch(/df_dec\.onnx/);
  });

  it("pins a checksum for every file it lists", () => {
    for (const spec of MODELS) {
      for (const [role, file] of Object.entries(spec.files)) {
        expect(file.sha256, `${spec.id}/${role}`).toMatch(/^[0-9a-f]{64}$/);
      }
      expect(spec.archive.sha256).toMatch(/^[0-9a-f]{64}$/);
    }
  });
});

describe("the programme-loss guard", () => {
  /**
   * A backend that behaves the way DeepFilterNet3 does on material it does not
   * recognise as speech: it attenuates the voice rather than the noise. The
   * stage has to notice and throw the result away, because a trained backend
   * fails by confidently rewriting the signal, not by doing too little.
   */
  function destroyer(lossDb: number): DenoiseBackend {
    const gain = 10 ** (-lossDb / 20);
    return {
      name: "destroyer",
      description: "test backend that eats the programme",
      available: () => true,
      unavailableReason: () => null,
      process: (request) => ({
        channels: request.channels.map((ch) => ch.map((v) => v * gain)),
        info: {},
      }),
    };
  }

  function run(backend: DenoiseBackend) {
    registerBackend(backend);
    const { signal } = speech(-40); // noisy enough that the stage engages
    const source: Signal = { sampleRate: sr, channels: signal.channels, length: signal.length };
    const analysis = new Analyzer(source);
    return denoiseStage.render(
      source,
      { ...DEFAULT_DENOISE_PARAMS, backends: [backend.name] },
      { analysis, source, sourceAnalysis: analysis, progress: () => {} },
    );
  }

  it("discards a backend that removes the speech instead of the noise", () => {
    const { signal, report } = run(destroyer(10));

    expect(report.backend).toBe("destroyer");
    expect(report.programmeLossDb).toBeGreaterThan(9);
    expect(report.rejected).toMatch(/programme loudness/);
    // Rejected means the input itself comes back, not a partly-processed copy.
    expect(signal.channels[0]).toBe(speech(-40).signal.channels[0]);
  });

  it("leaves a backend alone when the loss is small enough to be noise removal", () => {
    const { report } = run(destroyer(0.5));

    expect(report.rejected).toBeNull();
    expect(report.programmeLossDb).toBeLessThan(DEFAULT_DENOISE_PARAMS.maxProgrammeLossDb);
  });
});
