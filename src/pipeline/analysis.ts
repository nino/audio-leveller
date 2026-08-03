/**
 * Memoised measurement of a signal.
 *
 * K-weighting a long file is the expensive part of every loudness question,
 * and by the time there are six stages in the chain several of them will want
 * the answer to overlapping questions. An Analyzer is bound to one signal and
 * computes each measurement at most once.
 *
 * Analyzers are deliberately *not* shared across stages that modify audio: a
 * stage that changes the spectrum invalidates every measurement taken before
 * it, so the pipeline hands each stage a fresh Analyzer over the signal as it
 * actually arrives.
 */

import { preFilter, integratedLoudness } from "../dsp/loudness";
import {
  analyzeSilence,
  DEFAULT_SILENCE_OPTIONS,
  type SilenceAnalysis,
  type SilenceOptions,
} from "../dsp/silence";
import type { Measurement, Signal } from "./types";

export class Analyzer {
  private filteredChannels: Float32Array[] | null = null;
  private integrated: number | null = null;
  private peak: number | null = null;
  private silenceCache = new Map<string, SilenceAnalysis>();

  constructor(readonly signal: Signal) {}

  /** K-weighted channels, computed once and reused by every measurement. */
  filtered(): Float32Array[] {
    if (!this.filteredChannels) {
      this.filteredChannels = preFilter(this.signal.channels, this.signal.sampleRate);
    }
    return this.filteredChannels;
  }

  /** Gated integrated loudness of the whole signal (LUFS). */
  integratedLufs(): number {
    if (this.integrated === null) {
      this.integrated = integratedLoudness(this.filtered(), this.signal.sampleRate);
    }
    return this.integrated;
  }

  /** Highest absolute sample value across all channels, in dBFS. */
  peakDbfs(): number {
    if (this.peak === null) {
      let max = 0;
      for (const ch of this.signal.channels) {
        for (let i = 0; i < ch.length; i++) {
          const a = Math.abs(ch[i]);
          if (a > max) max = a;
        }
      }
      this.peak = max > 0 ? 20 * Math.log10(max) : -Infinity;
    }
    return this.peak;
  }

  /** Silence/speech segmentation, memoised per distinct set of options. */
  silence(options: Partial<SilenceOptions> = {}): SilenceAnalysis {
    const resolved = { ...DEFAULT_SILENCE_OPTIONS, ...options };
    const key = JSON.stringify(resolved);
    let cached = this.silenceCache.get(key);
    if (!cached) {
      cached = analyzeSilence(this.filtered(), this.signal.sampleRate, resolved);
      this.silenceCache.set(key, cached);
    }
    return cached;
  }

  /** The summary that goes into the pipeline report. */
  measure(): Measurement {
    return { integratedLufs: this.integratedLufs(), peakDbfs: this.peakDbfs() };
  }
}
