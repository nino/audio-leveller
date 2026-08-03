/**
 * FFT convolution by overlap-add.
 *
 * Needed for room impulse responses, where direct convolution is hopeless: a
 * half-second tail at 48 kHz is 24 000 taps, and multiplying that by every
 * sample of a ten-second file is 10^10 operations. Transforming blocks instead
 * turns it into a few thousand FFTs.
 */

import { fftComplex, ifftComplex } from "./fft";

/** Smallest power of two greater than or equal to `n`. */
function nextPowerOfTwo(n: number): number {
  let size = 1;
  while (size < n) size <<= 1;
  return size;
}

/**
 * Convolve `signal` with `impulse`, returning `signal.length` samples.
 *
 * The result is truncated to the input length rather than the full
 * `n + m - 1`: for an impulse response this means the tail that would run past
 * the end of the recording is discarded, which is what applying a room to a
 * finite recording should do.
 */
export function convolve(signal: Float32Array, impulse: Float32Array): Float32Array {
  const impulseLength = impulse.length;
  if (impulseLength === 0 || signal.length === 0) return new Float32Array(signal.length);

  // A block that leaves at least as much room for the tail as the impulse
  // needs; 4x the impulse keeps the number of transforms sensible.
  const fftSize = nextPowerOfTwo(Math.max(impulseLength * 4, 1024));
  const blockSize = fftSize - impulseLength + 1;

  // Transform of the impulse, reused for every block.
  const kernel = new Float64Array(2 * fftSize);
  for (let i = 0; i < impulseLength; i++) kernel[2 * i] = impulse[i];
  fftComplex(kernel);

  const out = new Float32Array(signal.length);
  const buffer = new Float64Array(2 * fftSize);

  for (let start = 0; start < signal.length; start += blockSize) {
    buffer.fill(0);
    const end = Math.min(signal.length, start + blockSize);
    for (let i = start; i < end; i++) buffer[2 * (i - start)] = signal[i];

    fftComplex(buffer);

    // Complex multiply by the kernel.
    for (let k = 0; k < fftSize; k++) {
      const re = buffer[2 * k];
      const im = buffer[2 * k + 1];
      const kre = kernel[2 * k];
      const kim = kernel[2 * k + 1];
      buffer[2 * k] = re * kre - im * kim;
      buffer[2 * k + 1] = re * kim + im * kre;
    }

    ifftComplex(buffer);

    // Overlap-add, discarding anything past the end of the signal.
    for (let i = 0; i < fftSize; i++) {
      const at = start + i;
      if (at >= out.length) break;
      out[at] += buffer[2 * i];
    }
  }

  return out;
}
