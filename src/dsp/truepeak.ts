/**
 * True-peak measurement, per ITU-R BS.1770 Annex 2.
 *
 * A sample peak is not the peak. Between two samples the reconstructed
 * waveform an actual DAC produces can overshoot both of them, and a signal
 * measuring -1 dBFS sample-peak can reach +0.5 dBTP on the way out of a
 * converter — or clip inside a lossy encoder, which reconstructs the same
 * inter-sample content. The standard's answer is to oversample by at least 4x
 * through a band-limited interpolator and take the peak of that.
 *
 * The polyphase filters here are the 4x, 48-tap design from the standard,
 * expressed as four 12-tap phases.
 */

/** BS.1770-4 Annex 2 Table 3: 4x oversampling, 12 taps per phase. */
const PHASES: number[][] = [
  [
    0.0017089843750, 0.0109863281250, -0.0196533203125, 0.0332031250000, -0.0594482421875,
    0.1373291015625, 0.9721679687500, -0.1022949218750, 0.0476074218750, -0.0266113281250,
    0.0148925781250, -0.0083007812500,
  ],
  [
    -0.0291748046875, 0.0292968750000, -0.0517578125000, 0.0891113281250, -0.1665039062500,
    0.4650878906250, 0.7797851562500, -0.2003173828125, 0.1015625000000, -0.0582275390625,
    0.0330810546875, -0.0189208984375,
  ],
  [
    -0.0189208984375, 0.0330810546875, -0.0582275390625, 0.1015625000000, -0.2003173828125,
    0.7797851562500, 0.4650878906250, -0.1665039062500, 0.0891113281250, -0.0517578125000,
    0.0292968750000, -0.0291748046875,
  ],
  [
    -0.0083007812500, 0.0148925781250, -0.0266113281250, 0.0476074218750, -0.1022949218750,
    0.9721679687500, 0.1373291015625, -0.0594482421875, 0.0332031250000, -0.0196533203125,
    0.0109863281250, 0.0017089843750,
  ],
];

const TAPS = 12;

/**
 * Peak of the 4x-oversampled signal, as a linear amplitude.
 *
 * The standard also specifies a 12.5 kHz low-pass and a 48 kHz working rate;
 * for the pipeline's purposes (deciding how hard to limit) the oversampled
 * peak alone is the number that matters, and skipping the low-pass makes the
 * measurement slightly conservative rather than slightly optimistic.
 */
export function truePeakLinear(channels: Float32Array[]): number {
  let peak = 0;

  for (const ch of channels) {
    const n = ch.length;
    for (let i = 0; i < n; i++) {
      for (const phase of PHASES) {
        let acc = 0;
        for (let t = 0; t < TAPS; t++) {
          // Centre the 12-tap window on the current sample.
          const idx = i + t - (TAPS >> 1) + 1;
          if (idx < 0 || idx >= n) continue;
          acc += phase[t] * ch[idx];
        }
        const abs = acc < 0 ? -acc : acc;
        if (abs > peak) peak = abs;
      }
    }
  }

  return peak;
}

/** True peak in dBFS (dBTP). -Infinity for digital silence. */
export function truePeakDbfs(channels: Float32Array[]): number {
  const peak = truePeakLinear(channels);
  return peak > 0 ? 20 * Math.log10(peak) : -Infinity;
}

/**
 * Per-sample envelope of the 4x-oversampled peak: for each input sample, the
 * largest interpolated magnitude in its neighbourhood, across all channels.
 *
 * This is what a true-peak limiter needs — the sample-domain gain curve has to
 * respond to overshoots that live *between* the samples it can actually touch.
 */
export function truePeakEnvelope(channels: Float32Array[]): Float32Array {
  const n = channels[0]?.length ?? 0;
  const out = new Float32Array(n);

  for (const ch of channels) {
    for (let i = 0; i < n; i++) {
      let local = 0;
      for (const phase of PHASES) {
        let acc = 0;
        for (let t = 0; t < TAPS; t++) {
          const idx = i + t - (TAPS >> 1) + 1;
          if (idx < 0 || idx >= n) continue;
          acc += phase[t] * ch[idx];
        }
        const abs = acc < 0 ? -acc : acc;
        if (abs > local) local = abs;
      }
      // The interpolated peak near sample i also has to be attenuated by the
      // gain applied at i-1 and i+1, so spread it to the neighbours.
      if (local > out[i]) out[i] = local;
      if (i > 0 && local > out[i - 1]) out[i - 1] = local;
      if (i + 1 < n && local > out[i + 1]) out[i + 1] = local;
    }
  }

  return out;
}
