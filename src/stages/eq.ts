/**
 * Corrective EQ as a pipeline stage.
 *
 * Sits after de-click and (once it exists) after the denoiser, because
 * denoising changes the spectrum you would otherwise be fitting a curve to,
 * and before the leveller, so the loudness target is measured on the audio
 * that actually gets written.
 *
 * The stage measures the LTAS over *speech* only and the pauses separately —
 * the second one gates boosts, so the EQ never buys timbre with noise. See
 * `ltas.ts` and `eq.ts` for the reasoning behind the target curve and the
 * constraint set.
 */

import { applyCascade } from "../dsp/biquad";
import {
  buildCascade,
  decideRumbleFilter,
  fitCorrectiveEq,
  DEFAULT_EQ_FIT_OPTIONS,
  type EqBand,
  type EqFitOptions,
} from "../dsp/eq";
import { computeLtas, DEFAULT_LTAS_OPTIONS, type SampleRange } from "../dsp/ltas";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export interface EqParams extends EqFitOptions {
  /** Fit and apply a rumble high-pass when the sub-bass warrants one. */
  rumbleEnabled: boolean;
  /** Corner frequency used when the rumble filter engages. */
  rumbleFreq: number;
  /**
   * How close the sub-bass may come to the voice's fundamental region before
   * the high-pass engages, in dB. Sub-bass is normally far below; -12 means
   * "within 12 dB of the fundamentals is too much".
   */
  rumbleMarginDb: number;
}

export const DEFAULT_EQ_PARAMS: EqParams = {
  ...DEFAULT_EQ_FIT_OPTIONS,
  rumbleEnabled: true,
  rumbleFreq: 80,
  rumbleMarginDb: -12,
};

export interface EqStageReport {
  bands: EqBand[];
  /** Rumble high-pass corner, or null when it stayed off. */
  rumbleFreq: number | null;
  rumbleExcessDb: number;
  /** Worst spectral deviation before and after, in dB. */
  deviationBeforeDb: number;
  deviationAfterDb: number;
  /** Frames of speech and of pause that went into the two spectra. */
  speechFrames: number;
  noiseFrames: number;
  /** True when there was too little speech to measure and nothing was done. */
  skipped: boolean;
}

/** Mono downmix, since one EQ curve is applied to every channel. */
function downmix(signal: Signal): Float32Array {
  if (signal.channels.length === 1) return signal.channels[0];
  const out = new Float32Array(signal.length);
  for (const ch of signal.channels) {
    for (let i = 0; i < signal.length; i++) out[i] += ch[i];
  }
  const scale = 1 / signal.channels.length;
  for (let i = 0; i < signal.length; i++) out[i] *= scale;
  return out;
}

/** Speech ranges are the gaps between the detected silences. */
function speechRanges(silences: { start: number; end: number }[], length: number): SampleRange[] {
  const ranges: SampleRange[] = [];
  let at = 0;
  for (const silence of silences) {
    if (silence.start > at) ranges.push({ start: at, end: silence.start });
    at = silence.end;
  }
  if (at < length) ranges.push({ start: at, end: length });
  return ranges;
}

export const eqStage: Stage<EqParams, EqStageReport> = {
  name: "eq",
  description: "Correct room and microphone colouration from the long-term average spectrum",
  defaults: DEFAULT_EQ_PARAMS,

  render(signal, params, ctx): StageOutput<EqStageReport> {
    const mono = downmix(signal);
    const silences = ctx.analysis.silence().regions;
    const speech = speechRanges(silences, signal.length);

    const speechLtas = computeLtas(mono, signal.sampleRate, speech);
    ctx.progress(0.4);

    const empty: EqStageReport = {
      bands: [],
      rumbleFreq: null,
      rumbleExcessDb: 0,
      deviationBeforeDb: 0,
      deviationAfterDb: 0,
      speechFrames: 0,
      noiseFrames: 0,
      skipped: true,
    };

    // Too little speech to characterise. Fitting to a couple of frames would
    // be fitting to an accident, so decline.
    if (!speechLtas) return { signal, report: empty };

    // The pauses, on the same grid, to gate boosts.
    const noiseLtas = computeLtas(mono, signal.sampleRate, silences, DEFAULT_LTAS_OPTIONS);

    const fit = fitCorrectiveEq(speechLtas, noiseLtas, signal.sampleRate, params);
    const rumble = params.rumbleEnabled
      ? decideRumbleFilter(speechLtas, params.rumbleFreq, params.rumbleMarginDb)
      : { freq: null, excessDb: 0 };
    ctx.progress(0.6);

    const cascade = buildCascade(fit, rumble, signal.sampleRate);
    const report: EqStageReport = {
      bands: fit.bands,
      rumbleFreq: rumble.freq,
      rumbleExcessDb: rumble.excessDb,
      deviationBeforeDb: fit.deviationBeforeDb,
      deviationAfterDb: fit.deviationAfterDb,
      speechFrames: speechLtas.frames,
      noiseFrames: noiseLtas?.frames ?? 0,
      skipped: false,
    };

    // Nothing worth correcting: return the signal itself, so a clean file
    // comes out of this stage bit-identical rather than merely similar.
    if (cascade.length === 0) return { signal, report };

    const channels = signal.channels.map((ch) => applyCascade(ch, cascade));
    ctx.progress(1);

    return {
      signal: { sampleRate: signal.sampleRate, channels, length: signal.length },
      report,
    };
  },
};
