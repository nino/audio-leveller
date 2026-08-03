import { describe, it, expect } from "vitest";
import {
  syntheticSpeech,
  addNoise,
  addClicks,
  cloneSignal,
  rng,
} from "../src/eval/signals";
import {
  siSdrDb,
  changeDb,
  impulsiveResidualDb,
  segmentLufsError,
  computeMetrics,
  lufsOf,
} from "../src/eval/metrics";
import { CASES } from "../src/eval/cases";
import type { Signal } from "../src/pipeline/types";

const sr = 48000;

// Generating speech-shaped material is the slow part of these tests, and it is
// deterministic, so identical requests are built once and shared.
const speechCache = new Map<string, ReturnType<typeof syntheticSpeech>>();

function speech(levels = [-30, -18, -25], options = {}) {
  const key = JSON.stringify([levels, options]);
  let built = speechCache.get(key);
  if (!built) {
    built = syntheticSpeech({
      sampleRate: sr,
      segments: levels.map((levelLufs, i) => ({ seconds: 2, levelLufs, f0: 105 + i * 22 })),
      pauseSec: 1.2,
      floorDbfs: -62,
      seed: 4242,
      ...options,
    });
    speechCache.set(key, built);
  }
  return built;
}

/** Scale every channel of a signal by a constant. */
function scaled(signal: Signal, gain: number): Signal {
  const out = cloneSignal(signal);
  for (const ch of out.channels) {
    for (let i = 0; i < ch.length; i++) ch[i] *= gain;
  }
  return out;
}

describe("synthetic signals", () => {
  it("is deterministic across runs", () => {
    // Deliberately bypasses the cache — two independent builds must agree.
    const build = () =>
      syntheticSpeech({
        sampleRate: sr,
        segments: [{ seconds: 1, levelLufs: -23, f0: 110 }],
        pauseSec: 0.5,
        floorDbfs: -62,
        seed: 4242,
      }).signal;
    expect(Array.from(build().channels[0])).toEqual(Array.from(build().channels[0]));
  });

  it("generates each segment at the loudness it was asked for", () => {
    const { signal, segments } = speech([-30, -18, -25]);
    for (const seg of segments) {
      const slice: Signal = {
        sampleRate: sr,
        channels: signal.channels.map((ch) => ch.slice(seg.start, seg.end)),
        length: seg.end - seg.start,
      };
      // The noise floor sits 30+ dB down, so it barely shifts the measurement.
      expect(lufsOf(slice)).toBeCloseTo(seg.levelLufs, 0);
    }
  });

  it("reports segment boundaries that contain the speech, not the pauses", () => {
    const { signal, segments } = speech();
    const pauseStart = Math.round(0.2 * sr);
    const pauseEnd = segments[0].start - Math.round(0.2 * sr);
    const pause: Signal = {
      sampleRate: sr,
      channels: [signal.channels[0].slice(pauseStart, pauseEnd)],
      length: pauseEnd - pauseStart,
    };
    // The leading pause should be far quieter than the first speech segment.
    expect(lufsOf(pause)).toBeLessThan(segments[0].levelLufs - 20);
  });

  it("adds noise at the requested signal-to-noise ratio", () => {
    const { signal } = speech();
    for (const snr of [6, 20]) {
      const noisy = addNoise(signal, snr, 991);
      // Recover the noise that was added and measure it against the programme.
      const noise: Signal = {
        sampleRate: sr,
        channels: [
          noisy.channels[0].map((v, i) => v - signal.channels[0][i]) as Float32Array,
        ],
        length: signal.length,
      };
      expect(lufsOf(signal) - lufsOf(noise)).toBeCloseTo(snr, 0);
    }
  });

  it("places the requested number of clicks and reports where", () => {
    const { signal } = speech();
    const { signal: clicked, positions } = addClicks(signal, {
      count: 12,
      relativeAmplitude: 0.5,
      seed: 5150,
    });

    expect(positions).toHaveLength(12);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    for (const pos of positions) {
      // Something changed at each reported position.
      expect(clicked.channels[0][pos]).not.toBe(signal.channels[0][pos]);
    }
  });

  it("leaves the source signal untouched when degrading it", () => {
    const { signal } = speech();
    const before = Array.from(signal.channels[0]);
    addNoise(signal, 10, 1);
    addClicks(signal, { count: 5, relativeAmplitude: 1, seed: 2 });
    expect(Array.from(signal.channels[0])).toEqual(before);
  });

  it("produces a seeded PRNG with a plausible uniform distribution", () => {
    const next = rng(7);
    let sum = 0;
    const n = 20000;
    for (let i = 0; i < n; i++) {
      const v = next();
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThan(1);
      sum += v;
    }
    expect(sum / n).toBeCloseTo(0.5, 1);
  });
});

describe("metrics", () => {
  it("scores an identical signal as infinite SI-SDR", () => {
    const { signal } = speech();
    expect(siSdrDb(signal, cloneSignal(signal))).toBe(Infinity);
  });

  it("is scale-invariant — the whole point of using SI-SDR here", () => {
    // The leveller changes overall gain by design; the metric must not
    // interpret that as damage. Gains that aren't powers of two leave float32
    // rounding behind, so this lands around 150 dB rather than at infinity —
    // still far below anything audible (float32 tops out near 144 dB).
    const { signal } = speech();
    for (const gain of [0.1, 0.5, 2, 8]) {
      expect(siSdrDb(signal, scaled(signal, gain))).toBeGreaterThan(120);
    }
  });

  it("falls as noise is added, monotonically", () => {
    const { signal } = speech();
    const scores = [30, 20, 10, 3].map((snr) => siSdrDb(signal, addNoise(signal, snr, 55)));
    for (let i = 1; i < scores.length; i++) {
      expect(scores[i]).toBeLessThan(scores[i - 1]);
    }
    // A 20 dB SNR degradation should score in a believable range, not wildly off.
    expect(scores[1]).toBeGreaterThan(5);
    expect(scores[1]).toBeLessThan(35);
  });

  it("reports -Infinity change for a bit-identical signal", () => {
    const { signal } = speech();
    expect(changeDb(signal, cloneSignal(signal))).toBe(-Infinity);
  });

  it("reports a finite change when a single sample moves", () => {
    const { signal } = speech();
    const altered = cloneSignal(signal);
    altered.channels[0][1234] += 0.01;
    expect(Number.isFinite(changeDb(signal, altered))).toBe(true);
  });

  it("measures louder clicks as a higher impulsive residual", () => {
    const { signal } = speech();
    const quiet = addClicks(signal, { count: 20, relativeAmplitude: 0.05, seed: 3 });
    const loud = addClicks(signal, { count: 20, relativeAmplitude: 0.9, seed: 3 });

    const quietScore = impulsiveResidualDb(signal, quiet.signal, quiet.positions);
    const loudScore = impulsiveResidualDb(signal, loud.signal, loud.positions);
    expect(loudScore).toBeGreaterThan(quietScore + 10);
  });

  it("ignores an overall gain change when scoring clicks", () => {
    const { signal } = speech();
    const { signal: clicked, positions } = addClicks(signal, {
      count: 20,
      relativeAmplitude: 0.5,
      seed: 3,
    });
    const direct = impulsiveResidualDb(signal, clicked, positions);
    const gained = impulsiveResidualDb(signal, scaled(clicked, 4), positions);
    expect(gained).toBeCloseTo(direct, 1);
  });

  it("measures segment error against the target, not the source level", () => {
    const { signal, segments } = speech([-23, -23, -23]);
    expect(segmentLufsError(signal, segments, -23)).toBeLessThan(1);
    // The same signal is 10 LU off a -13 target.
    expect(segmentLufsError(signal, segments, -13)).toBeGreaterThan(9);
  });

  it("holds SNR steady under a pure gain change", () => {
    // The transparency check the whole harness leans on: gain moves programme
    // and floor together, so snrGainDb must stay at zero.
    const { signal } = speech();
    const metrics = computeMetrics({ input: signal, output: scaled(signal, 3) });
    expect(metrics.snrGainDb).toBeCloseTo(0, 1);
  });
});

describe("case corpus", () => {
  it("gives every case a unique name", () => {
    const names = CASES.map((c) => c.name);
    expect(new Set(names).size).toBe(names.length);
  });

  it("states a reason for every expectation", () => {
    for (const evalCase of CASES) {
      for (const expectation of evalCase.expectations) {
        expect(expectation.because.length, `${evalCase.name}/${expectation.metric}`).toBeGreaterThan(
          10,
        );
        // A bound with neither end is not a bound.
        expect(
          expectation.min !== undefined || expectation.max !== undefined,
          `${evalCase.name}/${expectation.metric}`,
        ).toBe(true);
      }
    }
  });

  it("only references metrics that computeMetrics can produce", () => {
    // Guards against a renamed metric silently turning a bound into a no-op.
    const evalCase = CASES.find((c) => c.name === "clicky");
    if (!evalCase) throw new Error("expected a clicky case");
    const input = evalCase.build();
    const available = new Set(
      Object.keys(
        computeMetrics({
          input: input.input,
          output: input.input,
          reference: input.reference
            ? { forInput: input.reference, forOutput: input.reference }
            : undefined,
          segments: input.segments,
          clickPositions: input.clickPositions,
          targetLufs: input.targetLufs,
        }),
      ),
    );
    available.add("resampled"); // added by the runner from the pipeline report

    for (const c of CASES) {
      for (const expectation of c.expectations) {
        expect(available.has(expectation.metric), `${c.name}/${expectation.metric}`).toBe(true);
      }
    }
  });
});
