import type { AudioBuffer } from "../src/dsp/wav";

/** Generate a mono sine wave. `amp` is peak amplitude in [0, 1]. */
export function sine(
  freq: number,
  seconds: number,
  sampleRate: number,
  amp = 0.5,
): Float32Array {
  const n = Math.round(seconds * sampleRate);
  const out = new Float32Array(n);
  const w = (2 * Math.PI * freq) / sampleRate;
  for (let i = 0; i < n; i++) out[i] = amp * Math.sin(w * i);
  return out;
}

/** A block of near-silence (very low-level white-ish noise floor). */
export function silence(seconds: number, sampleRate: number, amp = 0.0002): Float32Array {
  const n = Math.round(seconds * sampleRate);
  const out = new Float32Array(n);
  // Deterministic pseudo-noise (no Math.random) so tests are reproducible.
  let seed = 12345;
  for (let i = 0; i < n; i++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    out[i] = ((seed / 0x7fffffff) * 2 - 1) * amp;
  }
  return out;
}

/**
 * A "breath": a louder burst of noise shaped by a raised-cosine envelope
 * (swells up then down), like the breaths that contaminate real room tone.
 */
export function breath(seconds: number, sampleRate: number, amp = 0.03): Float32Array {
  const n = Math.round(seconds * sampleRate);
  const out = new Float32Array(n);
  let seed = 99991;
  for (let i = 0; i < n; i++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    const noise = (seed / 0x7fffffff) * 2 - 1;
    const env = 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (n - 1)); // 0 → 1 → 0
    out[i] = noise * amp * env;
  }
  return out;
}

export function concat(...parts: Float32Array[]): Float32Array {
  const total = parts.reduce((s, p) => s + p.length, 0);
  const out = new Float32Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

export function mono(samples: Float32Array, sampleRate: number): AudioBuffer {
  return {
    sampleRate,
    channels: [samples],
    length: samples.length,
    bitDepth: 24,
    format: "int",
  };
}

/** Peak absolute sample value across all channels. */
export function peak(audio: AudioBuffer): number {
  let p = 0;
  for (const ch of audio.channels) {
    for (let i = 0; i < ch.length; i++) {
      const a = Math.abs(ch[i]);
      if (a > p) p = a;
    }
  }
  return p;
}
