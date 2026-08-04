/**
 * Blind reverberation-time measurement.
 *
 * Lives here rather than with the evaluation metrics because the dereverb
 * stage needs it at runtime to decide whether to engage at all — and a stage
 * reaching into the test fixtures for a measurement would be exactly backwards.
 */

import type { Signal } from "../pipeline/types";

function median(values: number[]): number {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * Blind estimate of how long the recording rings, in milliseconds.
 *
 * No reference and no impulse response: find the moments where speech stops,
 * and measure how fast the energy falls afterwards. Dry speech decays at the
 * rate the talker's own articulation sets — a syllable ends in tens of
 * milliseconds. A room adds its own, much slower decay on top, so this rises
 * with reverberation and falls when a dereverberator works.
 *
 * Reported as the time to fall 20 dB from each offset, median across offsets,
 * which is the same idea as the T20 of a room measurement without needing to
 * fire a starter pistol in it.
 */
export function reverbDecayMs(signal: Signal, frameMs = 5): number {
  const sampleRate = signal.sampleRate;
  const frame = Math.max(1, Math.round((frameMs / 1000) * sampleRate));
  const frames = Math.floor(signal.length / frame);
  if (frames < 10) return NaN;

  // Broadband energy envelope, in dB.
  const envelope = new Float64Array(frames);
  for (let f = 0; f < frames; f++) {
    let acc = 0;
    for (const ch of signal.channels) {
      for (let i = f * frame; i < (f + 1) * frame; i++) acc += ch[i] * ch[i];
    }
    envelope[f] = 10 * Math.log10(acc / (frame * signal.channels.length) + 1e-30);
  }

  // Spreading the envelope into Math.max would blow the argument limit: at
  // 5 ms a frame this array has one entry per 5 ms of audio, so a 21-minute
  // recording arrives with a quarter of a million of them and the call
  // overflows the stack. Every reduction over an array whose length scales
  // with the file has to be a loop.
  let peak = -Infinity;
  for (let f = 0; f < frames; f++) {
    if (envelope[f] > peak) peak = envelope[f];
  }
  // Only consider offsets from frames loud enough to be speech, so the
  // measurement is not dominated by the noise floor wandering.
  const activeThreshold = peak - 25;

  const decays: number[] = [];
  for (let f = 1; f < frames - 1; f++) {
    if (envelope[f] < activeThreshold) continue;
    // An offset: a local maximum that is followed by a sustained fall.
    if (envelope[f] < envelope[f - 1] || envelope[f] < envelope[f + 1]) continue;

    const start = envelope[f];
    let fell = -1;
    for (let g = f + 1; g < Math.min(frames, f + 200); g++) {
      // A rise means new speech started; this offset tells us nothing.
      if (envelope[g] > start) break;
      if (envelope[g] <= start - 20) {
        fell = g - f;
        break;
      }
    }
    if (fell > 0) decays.push(fell * frameMs);
  }

  return median(decays);
}
