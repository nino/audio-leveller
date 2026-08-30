/**
 * Command-line entry point.
 *
 *   node dist/cli.js <input.wav> [output.wav] [options]
 *
 * Handy for batch use, for eyeballing what the pipeline decided, and – via
 * `--bypass` plus `--report` – for comparing a stage against itself when
 * tuning by ear.
 */

import { writeFile } from "node:fs/promises";
import { processFile, outputPathFor, levellerReportOf } from "./process";
import type { PipelineProgress, PipelineReport } from "./pipeline/types";
import { DEFAULT_CHAIN } from "./stages";
import { registeredStages } from "./pipeline/registry";
import { PRESETS, presetByName, type ParamOverrides } from "./params";

interface Args {
  inputPath?: string;
  outputPath?: string;
  only?: string[];
  bypass?: string[];
  preset?: string;
  targetLufs?: number;
  reportPath?: string;
  json: boolean;
  quiet: boolean;
  listStages: boolean;
  listPresets: boolean;
}

const USAGE = `Usage: audio-leveller <input.wav> [output.wav] [options]

Options:
  --only <a,b>      run only these stages (in chain order)
  --bypass <a,b>    run the chain but bypass these stages
  --preset <name>   start from a preset's parameters (default Nino)
  --target <lufs>   target loudness for the level stage (overrides the preset)
  --report <file>   write the full JSON report to a file
  --json            print the JSON report instead of the text summary
  --quiet           suppress progress output
  --list-stages     list the available stages and exit
  --list-presets    list the available presets and exit
  -h, --help        show this help`;

function parseArgs(argv: string[]): Args {
  const args: Args = { json: false, quiet: false, listStages: false, listPresets: false };
  const positional: string[] = [];

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const value = (): string => {
      const next = argv[++i];
      if (next === undefined) throw new Error(`${arg} needs a value`);
      return next;
    };
    const list = (): string[] =>
      value()
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);

    switch (arg) {
      // A bare `--` is the conventional argument separator, and `pnpm run x --
      // --flag` inserts one. Skip it rather than rejecting it as an option.
      case "--":
        break;
      case "--only":
        args.only = list();
        break;
      case "--bypass":
        args.bypass = list();
        break;
      case "--target": {
        const parsed = Number(value());
        if (!Number.isFinite(parsed)) throw new Error("--target needs a number in LUFS");
        args.targetLufs = parsed;
        break;
      }
      case "--preset":
        args.preset = value();
        break;
      case "--report":
        args.reportPath = value();
        break;
      case "--json":
        args.json = true;
        break;
      case "--quiet":
        args.quiet = true;
        break;
      case "--list-stages":
        args.listStages = true;
        break;
      case "--list-presets":
        args.listPresets = true;
        break;
      case "-h":
      case "--help":
        console.log(USAGE);
        process.exit(0);
        break;
      default:
        if (arg.startsWith("-")) throw new Error(`Unknown option ${arg}`);
        positional.push(arg);
    }
  }

  [args.inputPath, args.outputPath] = positional;
  return args;
}

const plural = (n: number, s: string, p = `${s}s`): string => `${n} ${n === 1 ? s : p}`;
const signed = (n: number): string => `${n >= 0 ? "+" : ""}${n.toFixed(1)}`;
const lufs = (n: number): string => (Number.isFinite(n) ? `${n.toFixed(1)} LUFS` : "silent");
const dbfs = (n: number): string => (Number.isFinite(n) ? `${n.toFixed(1)} dBFS` : "silent");

function printReport(
  report: PipelineReport,
  outputPath: string,
  extraPaths: Record<string, string>,
): void {
  console.log(
    `${report.channels === 1 ? "Mono" : `${report.channels} ch`} · ` +
      `${report.sourceSampleRate} Hz · ${report.durationSec.toFixed(1)}s` +
      (report.resampled ? ` (chain ran at ${report.workingSampleRate} Hz)` : ""),
  );
  console.log(`Input:  ${lufs(report.input.integratedLufs)}, peak ${dbfs(report.input.peakDbfs)}`);
  console.log(`Output: ${lufs(report.output.integratedLufs)}, peak ${dbfs(report.output.peakDbfs)}`);

  console.log("\nChain:");
  for (const stage of report.stages) {
    if (!stage.enabled) console.log(`  ${stage.name}  – bypassed`);
    else console.log(`  ${stage.name}  (${stage.elapsedMs.toFixed(0)} ms)`);
  }

  const leveller = levellerReportOf(report);
  if (leveller) {
    console.log(
      `\nSilence threshold: ${leveller.thresholdLufs.toFixed(1)} LUFS ` +
        `(floor ${leveller.floorLufs.toFixed(1)})`,
    );
    console.log(
      `Detected ${plural(leveller.silences.length, "silence")}, ` +
        `${plural(leveller.segments.length, "segment")}:`,
    );
    const sec = (n: number): string => (n / leveller.sampleRate).toFixed(2);
    leveller.segments.forEach((s, i) => {
      console.log(
        `  #${i + 1}  ${sec(s.start)}s–${sec(s.end)}s  ${lufs(s.loudnessLufs)}  ` +
          `→  gain ${signed(s.gainDb)} dB`,
      );
    });
    if (leveller.limiterGainReductionDb > 0.01) {
      console.log(
        `Limiter engaged: up to ${leveller.limiterGainReductionDb.toFixed(1)} dB reduction`,
      );
    }
    const rt = leveller.roomTone;
    if (extraPaths.roomtone) {
      console.log(
        `Room tone: ${rt.durationSec.toFixed(1)}s from ${plural(rt.clips.length, "clean clip")} ` +
          `(${rt.instances} looped), gain ${signed(rt.gainDb)} dB`,
      );
    } else {
      console.log("Room tone: skipped (no usable silence found)");
    }
  }

  console.log(`\nWrote ${outputPath}`);
  for (const path of Object.values(extraPaths)) console.log(`Wrote ${path}`);
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));

  if (args.listStages) {
    for (const stage of registeredStages()) {
      const note = DEFAULT_CHAIN.includes(stage.name) ? "" : "  (not in default chain)";
      console.log(`${stage.name.padEnd(12)} ${stage.description}${note}`);
    }
    return;
  }

  if (args.listPresets) {
    for (const preset of PRESETS) {
      console.log(`${preset.name.padEnd(12)} ${preset.description}`);
    }
    return;
  }

  if (!args.inputPath) {
    console.error(USAGE);
    process.exit(1);
  }

  const outputPath = args.outputPath ?? outputPathFor(args.inputPath);
  const showProgress = !args.quiet && !args.json;
  let lastStage = "";
  const onProgress = (p: PipelineProgress): void => {
    if (!showProgress || p.stage === lastStage) return;
    lastStage = p.stage;
    process.stderr.write(`[${p.index + 1}/${p.total}] ${p.stage}...\n`);
  };

  // A preset is only a set of overrides, so `--target` layers on top of it
  // rather than fighting it.
  const params: ParamOverrides = args.preset ? { ...presetByName(args.preset).params } : {};
  if (args.targetLufs !== undefined) {
    params.level = { ...params.level, targetLufs: args.targetLufs };
  }
  const presetBypass = args.preset ? presetByName(args.preset).bypass : [];
  const bypass =
    args.bypass || presetBypass.length > 0 ? [...(args.bypass ?? []), ...presetBypass] : undefined;

  const { report, extraPaths } = await processFile(
    args.inputPath,
    {
      only: args.only,
      bypass,
      params: Object.keys(params).length > 0 ? params : undefined,
      onProgress,
    },
    outputPath,
  );

  if (args.reportPath) await writeFile(args.reportPath, JSON.stringify(report, null, 2));

  if (args.json) console.log(JSON.stringify(report, null, 2));
  else printReport(report, outputPath, extraPaths);
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
