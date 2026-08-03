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

import { applyCascade, peakingEq } from "../dsp/biquad";
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
  /**
   * A fixed tonal tilt applied after the corrective bands, by name.
   *
   * Correction and voicing are different jobs and this stage now does both,
   * deliberately kept apart. Correction is fitted per recording and removes
   * what that room and microphone added. Voicing is the same on every file: it
   * is a taste, not a measurement, and it is applied last so it survives
   * whatever the fitter decided.
   *
   * `warm` is measured rather than invented — see {@link VOICINGS}.
   */
  voicing: VoicingName;
}

export type VoicingName = "neutral" | "warm";

/**
 * Fixed voicing curves.
 *
 * `warm` is the static tone curve of a commercial mastering service, recovered
 * from a 24-minute before/after pair. Getting it required separating tone from
 * dynamics, and the first attempt got that wrong in an instructive way: on the
 * loudest frames its 5-6.5 kHz region reads about -3 dB, which looks like a
 * de-harshing dip. Measured only on frames where *that band* sits 25 dB above
 * its own noise floor, the same region reads -0.2 dB. The dip was its
 * per-band noise suppression, not its EQ — even a loud vowel has little
 * genuine 6 kHz content, so most frames have that band near the floor where
 * the suppressor is working. Baking it in would have made every recording
 * duller for no reason anyone could hear.
 *
 * What survives that correction is very gentle indeed: about +1 dB under
 * 130 Hz and about -1 dB across 2.5-5 kHz. Which is the finding — that
 * service's "sound" is almost entirely its dynamics, not its tone. This curve
 * is offered because it is what was asked for and it is real, not because it
 * is where the character comes from.
 */
export const VOICINGS: Record<VoicingName, EqBand[]> = {
  neutral: [],
  warm: [
    // The +1.0 dB measured at 80-101 Hz, wide enough to read as weight rather
    // than as a bump on the fundamental.
    { freq: 95, gainDb: 1, q: 0.7 },
    // The -1.1 dB centred near 3.2 kHz and spanning 2.5-5 kHz. This is the
    // one that reads as "smooth": it is where a close microphone in an
    // untreated room puts its hardness.
    { freq: 3400, gainDb: -1.1, q: 0.9 },
  ],
};

export const DEFAULT_EQ_PARAMS: EqParams = {
  ...DEFAULT_EQ_FIT_OPTIONS,
  rumbleEnabled: true,
  rumbleFreq: 80,
  rumbleMarginDb: -12,
  voicing: "neutral",
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
  /** Voicing applied, and its bands, so the report shows tone separately from correction. */
  voicing: VoicingName;
  voicingBands: EqBand[];
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

    const voicingBands = VOICINGS[params.voicing] ?? [];
    const empty: EqStageReport = {
      bands: [],
      rumbleFreq: null,
      rumbleExcessDb: 0,
      deviationBeforeDb: 0,
      deviationAfterDb: 0,
      speechFrames: 0,
      noiseFrames: 0,
      skipped: true,
      voicing: params.voicing,
      voicingBands,
    };

    // Too little speech to characterise. Fitting to a couple of frames would
    // be fitting to an accident, so decline — and decline the voicing too,
    // since a file with no speech in it is not one to impose a voice on.
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
      voicing: params.voicing,
      voicingBands,
    };

    // Voicing rides after correction, so the fitter's decisions are not
    // reshaped by it and the two stay separable in the report.
    const voicingCascade = voicingBands.map((b) => peakingEq(signal.sampleRate, b.freq, b.gainDb, b.q));
    const full = [...cascade, ...voicingCascade];

    // Nothing worth correcting and no voice to impose: return the signal
    // itself, so a clean file comes out of this stage bit-identical rather
    // than merely similar.
    if (full.length === 0) return { signal, report };

    const channels = signal.channels.map((ch) => applyCascade(ch, full));
    ctx.progress(1);

    return {
      signal: { sampleRate: signal.sampleRate, channels, length: signal.length },
      report,
    };
  },
};
