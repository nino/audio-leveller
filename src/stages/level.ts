/**
 * The leveller as a pipeline stage.
 *
 * This wraps the existing `levelAudio` DSP unchanged: same segmentation, same
 * envelope, same limiter, same room-tone bed — the bed is now returned as a
 * named extra output instead of a second return value.
 *
 * The stage declares no `requiredSampleRate`: the K-weighting coefficients are
 * derived analytically per rate, so the leveller is correct at 44.1 kHz just
 * as it is at 48 kHz and needs no conversion.
 */

import type { AudioBuffer } from "../dsp/wav";
import {
  levelAudio,
  DEFAULT_LEVELLER_OPTIONS,
  type LevellerOptions,
  type LevellerReport,
} from "../dsp/leveller";
import type { Signal, Stage, StageOutput } from "../pipeline/types";

export type LevelParams = LevellerOptions;

/** Adapt a Signal to the AudioBuffer shape the DSP core expects. */
function asAudioBuffer(signal: Signal): AudioBuffer {
  return { ...signal, bitDepth: 32, format: "float" };
}

function toSignal(audio: AudioBuffer): Signal {
  return { sampleRate: audio.sampleRate, channels: audio.channels, length: audio.length };
}

export const levelStage: Stage<LevelParams, LevellerReport> = {
  name: "level",
  description: "Normalise each speech segment to a target loudness, ramping across silences",
  defaults: DEFAULT_LEVELLER_OPTIONS,

  render(signal, params, ctx): StageOutput<LevellerReport> {
    const { audio, roomTone, report } = levelAudio(asAudioBuffer(signal), params);
    ctx.progress(1);

    const extras: Record<string, Signal> = {};
    // Only offer a bed when there was usable silence to build one from.
    if (roomTone.length > 0) extras.roomtone = toSignal(roomTone);

    return { signal: toSignal(audio), report, extras };
  },
};
