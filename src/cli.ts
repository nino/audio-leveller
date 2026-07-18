/**
 * Command-line entry point:  node dist/cli.js input.wav [output.wav]
 * Handy for batch use and for eyeballing the leveller report.
 */

import { processFile, outputPathFor } from "./process";

async function main(): Promise<void> {
  const [inputPath, outputArg] = process.argv.slice(2);
  if (!inputPath) {
    console.error("Usage: audio-leveller <input.wav> [output.wav]");
    process.exit(1);
  }
  const outputPath = outputArg ?? outputPathFor(inputPath);

  const { report, roomTonePath } = await processFile(inputPath, {}, outputPath);

  console.log(`Input integrated loudness: ${report.integratedLufs.toFixed(1)} LUFS`);
  console.log(
    `Silence threshold: ${report.thresholdLufs.toFixed(1)} LUFS ` +
      `(floor ${report.floorLufs.toFixed(1)})`,
  );
  const plural = (n: number, s: string, p = `${s}s`) => `${n} ${n === 1 ? s : p}`;
  console.log(`Detected ${plural(report.silences.length, "silence")}, ${plural(report.segments.length, "segment")}:`);
  report.segments.forEach((s, i) => {
    const sec = (n: number) => (n / report.sampleRate).toFixed(2);
    const l = Number.isFinite(s.loudnessLufs) ? `${s.loudnessLufs.toFixed(1)} LUFS` : "silent";
    console.log(
      `  #${i + 1}  ${sec(s.start)}s–${sec(s.end)}s  ${l}  →  gain ${s.gainDb >= 0 ? "+" : ""}${s.gainDb.toFixed(1)} dB`,
    );
  });
  if (report.limiterGainReductionDb > 0.01) {
    console.log(`Limiter engaged: up to ${report.limiterGainReductionDb.toFixed(1)} dB reduction`);
  }
  console.log(`Wrote ${outputPath}`);

  const rt = report.roomTone;
  if (roomTonePath) {
    console.log(
      `Room tone: ${rt.durationSec.toFixed(1)}s from ${plural(rt.clips.length, "clean clip")} ` +
        `(${rt.instances} looped), gain ${rt.gainDb >= 0 ? "+" : ""}${rt.gainDb.toFixed(1)} dB`,
    );
    console.log(`Wrote ${roomTonePath}`);
  } else {
    console.log("Room tone: skipped (no usable silence found)");
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
