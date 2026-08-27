/**
 * Multi-resolution min/max peak cache for drawing long files at any zoom.
 *
 * Level 0 holds one min/max pair per BASE samples (all channels folded
 * together); each further level halves the resolution. Drawing a view picks
 * the finest level whose blocks are still at least twice as fine as a pixel,
 * so every pixel folds a handful of blocks instead of thousands of samples.
 * Zoomed in below BASE samples per pixel, it reads the raw samples instead.
 */

const BASE = 128;

interface Level {
  block: number;
  min: Float32Array;
  max: Float32Array;
}

export interface PeakCache {
  buffer: AudioBuffer;
  /**
   * Our own snapshot of the channel data, taken at build time. Draw paths
   * must read this, never `buffer.getChannelData()`: a decoded AudioBuffer's
   * storage is reclaimable — under memory pressure Chrome can purge it
   * page-by-page (playback stays correct; the audio thread holds its own
   * copy), and getChannelData then returns partly zeroed data. Seen in
   * practice as the wave "getting quieter" below 256 samples/px over a long
   * annotating session, while the pyramid — ordinary JS memory — stayed
   * right. 16-bit is deliberate: it halves the snapshot so the fix does not
   * feed the very memory pressure that triggers the bug, and 1/32768 is far
   * below anything a waveform plot can show.
   */
  samples: Int16Array[];
  levels: Level[];
}

export function buildPeaks(buffer: AudioBuffer): PeakCache {
  const n = buffer.length;
  const count = Math.ceil(n / BASE);
  const min = new Float32Array(count).fill(1);
  const max = new Float32Array(count).fill(-1);
  const samples: Int16Array[] = [];
  for (let c = 0; c < buffer.numberOfChannels; c++) {
    const data = buffer.getChannelData(c);
    const snap = new Int16Array(n);
    for (let i = 0; i < n; i++) {
      const v = data[i];
      snap[i] = v >= 1 ? 32767 : v <= -1 ? -32768 : (v * 32767) | 0;
    }
    samples.push(snap);
    for (let b = 0; b < count; b++) {
      let lo = min[b], hi = max[b];
      const s1 = Math.min(n, (b + 1) * BASE);
      for (let i = b * BASE; i < s1; i++) {
        const v = data[i];
        if (v < lo) lo = v;
        if (v > hi) hi = v;
      }
      min[b] = lo;
      max[b] = hi;
    }
  }
  const levels: Level[] = [{ block: BASE, min, max }];
  while (levels[levels.length - 1].min.length > 512) {
    const prev = levels[levels.length - 1];
    const len = Math.ceil(prev.min.length / 2);
    const lmin = new Float32Array(len), lmax = new Float32Array(len);
    for (let i = 0; i < len; i++) {
      const j = 2 * i, k = Math.min(prev.min.length - 1, j + 1);
      lmin[i] = Math.min(prev.min[j], prev.min[k]);
      lmax[i] = Math.max(prev.max[j], prev.max[k]);
    }
    levels.push({ block: prev.block * 2, min: lmin, max: lmax });
  }
  return { buffer, samples, levels };
}

/**
 * Min/max per pixel column for `width` columns starting at sample
 * `startSample`, `spp` samples per pixel. Columns past the end of the file
 * are left at (0, 0).
 */
export function peaksFor(cache: PeakCache, startSample: number, spp: number, width: number): { min: Float32Array; max: Float32Array } {
  const min = new Float32Array(width), max = new Float32Array(width);
  const n = cache.buffer.length;
  if (spp >= BASE * 2) {
    let level = cache.levels[0];
    for (const l of cache.levels) if (l.block * 2 <= spp) level = l;
    const { block } = level;
    for (let x = 0; x < width; x++) {
      const s0 = startSample + x * spp;
      if (s0 >= n) break;
      const b0 = Math.floor(s0 / block), b1 = Math.min(level.min.length, Math.ceil((s0 + spp) / block));
      let lo = 1, hi = -1;
      for (let b = b0; b < b1; b++) {
        if (level.min[b] < lo) lo = level.min[b];
        if (level.max[b] > hi) hi = level.max[b];
      }
      if (hi < lo) lo = hi = 0;
      min[x] = lo;
      max[x] = hi;
    }
    return { min, max };
  }
  // Fall back to the live buffer only for a cache built before `samples`
  // existed (a hot-reloaded page); a fresh build always carries the snapshot.
  const chans: (Int16Array | Float32Array)[] =
    cache.samples ??
    Array.from({ length: cache.buffer.numberOfChannels }, (_, c) => cache.buffer.getChannelData(c));
  const scale = cache.samples ? 1 / 32767 : 1;
  for (let x = 0; x < width; x++) {
    const s0 = Math.floor(startSample + x * spp);
    if (s0 >= n) break;
    const s1 = Math.min(n, Math.max(s0 + 1, Math.floor(startSample + (x + 1) * spp)));
    let lo = 32768, hi = -32768;
    for (const data of chans) {
      for (let i = s0; i < s1; i++) {
        const v = data[i];
        if (v < lo) lo = v;
        if (v > hi) hi = v;
      }
    }
    min[x] = lo * scale;
    max[x] = hi * scale;
  }
  return { min, max };
}
