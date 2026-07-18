/**
 * Minimal, dependency-free WAV (RIFF/WAVE) reader and writer.
 *
 * Supports the PCM formats that show up in the wild for spoken-word audio:
 *   - 16, 24 and 32-bit signed integer PCM   (WAVE_FORMAT_PCM = 1)
 *   - 32-bit IEEE float                       (WAVE_FORMAT_IEEE_FLOAT = 3)
 *   - the above wrapped in WAVE_FORMAT_EXTENSIBLE (0xFFFE)
 *
 * All samples are decoded to Float32 in the range [-1, 1). The original
 * bit depth / sample format is preserved so the output can be written back
 * in the same format as the input.
 */

export type SampleFormat = "int" | "float";

export interface AudioBuffer {
  sampleRate: number;
  /** One Float32Array per channel, each of length `length`. */
  channels: Float32Array[];
  /** Number of sample frames (per channel). */
  length: number;
  /** Bit depth of the source file (used when re-encoding). */
  bitDepth: number;
  /** Sample format of the source file. */
  format: SampleFormat;
}

const FORMAT_PCM = 0x0001;
const FORMAT_IEEE_FLOAT = 0x0003;
const FORMAT_EXTENSIBLE = 0xfffe;

function fourCC(view: DataView, offset: number): string {
  return String.fromCharCode(
    view.getUint8(offset),
    view.getUint8(offset + 1),
    view.getUint8(offset + 2),
    view.getUint8(offset + 3),
  );
}

export function decodeWav(buffer: ArrayBuffer): AudioBuffer {
  const view = new DataView(buffer);
  if (fourCC(view, 0) !== "RIFF") throw new Error("Not a RIFF file");
  if (fourCC(view, 8) !== "WAVE") throw new Error("Not a WAVE file");

  let audioFormat = FORMAT_PCM;
  let numChannels = 0;
  let sampleRate = 0;
  let bitsPerSample = 0;
  let dataOffset = -1;
  let dataLength = 0;

  // Walk the chunk list. Chunks are word-aligned (padded to even length).
  let pos = 12;
  while (pos + 8 <= view.byteLength) {
    const id = fourCC(view, pos);
    const size = view.getUint32(pos + 4, true);
    const body = pos + 8;

    if (id === "fmt ") {
      audioFormat = view.getUint16(body, true);
      numChannels = view.getUint16(body + 2, true);
      sampleRate = view.getUint32(body + 4, true);
      bitsPerSample = view.getUint16(body + 14, true);
      if (audioFormat === FORMAT_EXTENSIBLE && size >= 24) {
        // The real format lives in the first two bytes of the SubFormat GUID.
        audioFormat = view.getUint16(body + 24, true);
      }
    } else if (id === "data") {
      dataOffset = body;
      dataLength = size;
    }

    pos = body + size + (size & 1); // advance, honouring the pad byte
  }

  if (dataOffset < 0) throw new Error("WAV file has no data chunk");
  if (numChannels < 1) throw new Error("WAV file has no channels");

  const format: SampleFormat = audioFormat === FORMAT_IEEE_FLOAT ? "float" : "int";
  const bytesPerSample = bitsPerSample >> 3;
  const frameSize = bytesPerSample * numChannels;
  const length = Math.floor(dataLength / frameSize);

  const channels: Float32Array[] = [];
  for (let c = 0; c < numChannels; c++) channels.push(new Float32Array(length));

  const read = sampleReader(view, format, bitsPerSample);
  for (let i = 0; i < length; i++) {
    const frame = dataOffset + i * frameSize;
    for (let c = 0; c < numChannels; c++) {
      channels[c][i] = read(frame + c * bytesPerSample);
    }
  }

  return { sampleRate, channels, length, bitDepth: bitsPerSample, format };
}

function sampleReader(
  view: DataView,
  format: SampleFormat,
  bits: number,
): (offset: number) => number {
  if (format === "float") {
    if (bits === 32) return (o) => view.getFloat32(o, true);
    if (bits === 64) return (o) => view.getFloat64(o, true);
    throw new Error(`Unsupported float bit depth: ${bits}`);
  }
  switch (bits) {
    case 16:
      return (o) => view.getInt16(o, true) / 32768;
    case 24:
      return (o) => {
        // little-endian 24-bit signed
        const b0 = view.getUint8(o);
        const b1 = view.getUint8(o + 1);
        const b2 = view.getUint8(o + 2);
        let val = b0 | (b1 << 8) | (b2 << 16);
        if (val & 0x800000) val |= ~0xffffff; // sign-extend
        return val / 8388608;
      };
    case 32:
      return (o) => view.getInt32(o, true) / 2147483648;
    case 8:
      // 8-bit PCM is unsigned, centred on 128.
      return (o) => (view.getUint8(o) - 128) / 128;
    default:
      throw new Error(`Unsupported PCM bit depth: ${bits}`);
  }
}

export interface EncodeOptions {
  bitDepth?: number;
  format?: SampleFormat;
}

export function encodeWav(audio: AudioBuffer, opts: EncodeOptions = {}): ArrayBuffer {
  const format = opts.format ?? audio.format;
  const bitDepth = opts.bitDepth ?? audio.bitDepth;
  const numChannels = audio.channels.length;
  const bytesPerSample = bitDepth >> 3;
  const frameSize = bytesPerSample * numChannels;
  const dataLength = audio.length * frameSize;

  const buffer = new ArrayBuffer(44 + dataLength);
  const view = new DataView(buffer);

  const writeStr = (offset: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(offset + i, s.charCodeAt(i));
  };

  const audioFormat = format === "float" ? FORMAT_IEEE_FLOAT : FORMAT_PCM;
  writeStr(0, "RIFF");
  view.setUint32(4, 36 + dataLength, true);
  writeStr(8, "WAVE");
  writeStr(12, "fmt ");
  view.setUint32(16, 16, true); // fmt chunk size
  view.setUint16(20, audioFormat, true);
  view.setUint16(22, numChannels, true);
  view.setUint32(24, audio.sampleRate, true);
  view.setUint32(28, audio.sampleRate * frameSize, true); // byte rate
  view.setUint16(32, frameSize, true); // block align
  view.setUint16(34, bitDepth, true);
  writeStr(36, "data");
  view.setUint32(40, dataLength, true);

  const write = sampleWriter(view, format, bitDepth);
  for (let i = 0; i < audio.length; i++) {
    const frame = 44 + i * frameSize;
    for (let c = 0; c < numChannels; c++) {
      write(frame + c * bytesPerSample, audio.channels[c][i]);
    }
  }

  return buffer;
}

function sampleWriter(
  view: DataView,
  format: SampleFormat,
  bits: number,
): (offset: number, value: number) => void {
  if (format === "float") {
    if (bits === 32) return (o, v) => view.setFloat32(o, v, true);
    if (bits === 64) return (o, v) => view.setFloat64(o, v, true);
    throw new Error(`Unsupported float bit depth: ${bits}`);
  }
  const clamp = (v: number) => (v > 1 ? 1 : v < -1 ? -1 : v);
  switch (bits) {
    case 16:
      return (o, v) => view.setInt16(o, Math.round(clamp(v) * 32767), true);
    case 24:
      return (o, v) => {
        const s = Math.round(clamp(v) * 8388607);
        view.setUint8(o, s & 0xff);
        view.setUint8(o + 1, (s >> 8) & 0xff);
        view.setUint8(o + 2, (s >> 16) & 0xff);
      };
    case 32:
      return (o, v) => view.setInt32(o, Math.round(clamp(v) * 2147483647), true);
    case 8:
      return (o, v) => view.setUint8(o, Math.round(clamp(v) * 127) + 128);
    default:
      throw new Error(`Unsupported PCM bit depth: ${bits}`);
  }
}
