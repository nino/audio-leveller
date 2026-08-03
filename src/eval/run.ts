/**
 * Evaluation harness runner.
 *
 *   pnpm eval [options]
 *
 * Renders every case through the pipeline, measures the result, and checks it
 * against the case's stated bounds. Exits non-zero when a bound is broken, so
 * it works as a regression gate as well as a tuning tool.
 *
 * The workflow it exists for: run it, note the numbers, build a stage, run it
 * again with `--baseline` and see exactly what moved. "Sounds better" is not
 * evidence; a click residual that fell 45 dB while SI-SDR held steady is.
 */

import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { basename, extname, join, resolve } from "node:path";
import { decodeWav, encodeWav } from "../dsp/wav";
import { runPipeline } from "../pipeline/pipeline";
import type { PipelineReport, Signal } from "../pipeline/types";
import { buildChain } from "../stages";
import { CASES, type CaseInput, type EvalCase, type Expectation } from "./cases";
import { computeMetrics, type Metrics } from "./metrics";

interface Args {
  filter?: string;
  fixturesDir: string;
  baselinePath?: string;
  saveBaselinePath?: string;
  outPath?: string;
  wavDir?: string;
  verbose: boolean;
  json: boolean;
}

const USAGE = `Usage: pnpm eval [options]

Options:
  --case <substring>     run only cases whose name contains this
  --fixtures <dir>       directory of real .wav files to include (default eval/fixtures)
  --baseline <file>      compare against a saved results file and show what moved
  --save-baseline <file> write this run's metrics as the new baseline
  --out <file>           write full results as JSON
  --wav <dir>            dump each case's input and output as .wav for listening
  --verbose              print every metric, not just the ones with bounds
  --json                 print results as JSON
  -h, --help             show this help`;

function parseArgs(argv: string[]): Args {
  const args: Args = {
    fixturesDir: resolve(process.cwd(), "eval/fixtures"),
    verbose: false,
    json: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const value = (): string => {
      const next = argv[++i];
      if (next === undefined) throw new Error(`${arg} needs a value`);
      return next;
    };
    switch (arg) {
      case "--case": args.filter = value(); break;
      case "--fixtures": args.fixturesDir = resolve(value()); break;
      case "--baseline": args.baselinePath = value(); break;
      case "--save-baseline": args.saveBaselinePath = value(); break;
      case "--out": args.outPath = value(); break;
      case "--wav": args.wavDir = resolve(value()); break;
      case "--verbose": args.verbose = true; break;
      case "--json": args.json = true; break;
      case "-h":
      case "--help":
        console.log(USAGE);
        process.exit(0);
        break;
      default:
        throw new Error(`Unknown option ${arg}`);
    }
  }
  return args;
}

interface CheckResult {
  expectation: Expectation;
  value: number;
  passed: boolean;
}

interface CaseResult {
  name: string;
  description: string;
  metrics: Metrics;
  checks: CheckResult[];
  passed: boolean;
  elapsedMs: number;
}

function check(metrics: Metrics, expectations: Expectation[]): CheckResult[] {
  return expectations.map((expectation) => {
    const value = metrics[expectation.metric];
    if (value === undefined) {
      // A typo in a metric name must fail the run, not silently pass it.
      return { expectation, value: NaN, passed: false };
    }
    const aboveMin = expectation.min === undefined || value >= expectation.min;
    const belowMax = expectation.max === undefined || value <= expectation.max;
    return { expectation, value, passed: aboveMin && belowMax };
  });
}

async function dumpWav(dir: string, name: string, signal: Signal): Promise<void> {
  await mkdir(dir, { recursive: true });
  const audio = { ...signal, bitDepth: 24, format: "int" as const };
  await writeFile(join(dir, `${name}.wav`), Buffer.from(encodeWav(audio)));
}

function runCase(
  evalCase: EvalCase,
  input: CaseInput,
): { metrics: Metrics; output: Signal; report: PipelineReport } {
  const chain = evalCase.chain ?? buildChain();
  const { signal: output, report } = runPipeline(input.input, { stages: chain });

  // The reference goes through the same chain, so scores reflect what the
  // degradation did rather than what the chain was asked to do.
  const reference = input.reference
    ? {
        forInput: input.reference,
        forOutput: runPipeline(input.reference, { stages: chain }).signal,
      }
    : undefined;

  const metrics = computeMetrics({
    input: input.input,
    output,
    reference,
    segments: input.segments,
    clickPositions: input.clickPositions,
    targetLufs: input.targetLufs,
  });

  // Report-derived facts worth asserting on.
  metrics.resampled = report.resampled ? 1 : 0;

  return { metrics, output, report };
}

/** Real recordings dropped into the fixtures directory, if any. */
async function fixtureCases(dir: string): Promise<EvalCase[]> {
  if (!existsSync(dir)) return [];
  const entries = await readdir(dir);
  const wavs = entries.filter((f) => extname(f).toLowerCase() === ".wav").sort();

  const cases: EvalCase[] = [];
  for (const file of wavs) {
    // Read up front so `build()` stays synchronous like the synthetic cases.
    const raw = await readFile(join(dir, file));
    const buffer = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
    const audio = decodeWav(buffer);

    cases.push({
      name: `fixture:${basename(file, extname(file))}`,
      description: `Real recording from ${dir}`,
      build: (): CaseInput => ({
        input: { sampleRate: audio.sampleRate, channels: audio.channels, length: audio.length },
        targetLufs: -23,
      }),
      // No clean reference exists for real material, so only the
      // self-consistent measurements apply.
      expectations: [
        {
          metric: "lufsError",
          max: 1.5,
          because: "the programme should land on target regardless of source material",
        },
        {
          metric: "outputPeakDbfs",
          max: -0.95,
          because: "the -1 dBFS limiter ceiling must hold on real material too",
        },
      ],
    });
  }

  return cases;
}

const fmt = (n: number): string => {
  if (Number.isNaN(n)) return "n/a"; // a metric key that doesn't exist
  if (!Number.isFinite(n)) return n > 0 ? "+inf" : "-inf";
  return n.toFixed(2);
};

const useColour = process.stdout.isTTY;
const green = (s: string): string => (useColour ? `[32m${s}[0m` : s);
const red = (s: string): string => (useColour ? `[31m${s}[0m` : s);

function printCase(result: CaseResult, verbose: boolean): void {
  const mark = result.passed ? green("✓") : red("✗");
  console.log(`${mark} ${result.name.padEnd(18)} ${result.description}`);

  const m = result.metrics;
  const parts: string[] = [];
  if (m.inputLufs !== undefined) parts.push(`${fmt(m.inputLufs)} → ${fmt(m.outputLufs)} LUFS`);
  if (m.outputPeakDbfs !== undefined) parts.push(`peak ${fmt(m.outputPeakDbfs)} dBFS`);
  if (m.snrGainDb !== undefined) parts.push(`snr Δ ${fmt(m.snrGainDb)} dB`);
  if (m.siSdrGainDb !== undefined) parts.push(`si-sdr Δ ${fmt(m.siSdrGainDb)} dB`);
  if (m.outputClickResidualDb !== undefined) {
    parts.push(`clicks ${fmt(m.outputClickResidualDb)} dB`);
  }
  console.log(`    ${parts.join(" · ")}`);

  if (verbose) {
    for (const [key, value] of Object.entries(m).sort()) {
      console.log(`      ${key.padEnd(22)} ${fmt(value)}`);
    }
  }

  for (const c of result.checks) {
    if (c.passed && !verbose) continue;
    const bounds = [
      c.expectation.min !== undefined ? `min ${c.expectation.min}` : "",
      c.expectation.max !== undefined ? `max ${c.expectation.max}` : "",
    ]
      .filter(Boolean)
      .join(", ");
    const mark2 = c.passed ? "  ok" : "FAIL";
    console.log(`    ${mark2} ${c.expectation.metric} = ${fmt(c.value)} (${bounds})`);
    if (!c.passed) console.log(`         ${c.expectation.because}`);
  }
}

function printBaselineDiff(results: CaseResult[], baseline: Record<string, Metrics>): void {
  console.log("\nAgainst baseline (only changes over 0.05 dB shown):");
  let anyChange = false;

  for (const result of results) {
    const before = baseline[result.name];
    if (!before) {
      console.log(`  ${result.name}: new case, no baseline`);
      anyChange = true;
      continue;
    }
    const moved: string[] = [];
    for (const [key, value] of Object.entries(result.metrics)) {
      const previous = before[key];
      if (previous === undefined) continue;
      if (!Number.isFinite(previous) && !Number.isFinite(value)) continue;
      const delta = value - previous;
      if (Math.abs(delta) > 0.05) {
        moved.push(`${key} ${fmt(previous)} → ${fmt(value)} (${delta > 0 ? "+" : ""}${fmt(delta)})`);
      }
    }
    if (moved.length > 0) {
      anyChange = true;
      console.log(`  ${result.name}:`);
      for (const line of moved) console.log(`    ${line}`);
    }
  }

  if (!anyChange) console.log("  nothing moved");
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));

  const cases = [...CASES, ...(await fixtureCases(args.fixturesDir))].filter(
    (c) => !args.filter || c.name.includes(args.filter),
  );

  if (cases.length === 0) {
    console.error(args.filter ? `No case matches "${args.filter}"` : "No cases to run");
    process.exit(1);
  }

  const results: CaseResult[] = [];
  for (const evalCase of cases) {
    const input = evalCase.build();
    const started = performance.now();
    const { metrics, output } = runCase(evalCase, input);
    const elapsedMs = performance.now() - started;

    const checks = check(metrics, evalCase.expectations);
    results.push({
      name: evalCase.name,
      description: evalCase.description,
      metrics,
      checks,
      passed: checks.every((c) => c.passed),
      elapsedMs,
    });

    if (args.wavDir) {
      await dumpWav(args.wavDir, `${evalCase.name}_input`, input.input);
      await dumpWav(args.wavDir, `${evalCase.name}_output`, output);
    }
  }

  if (args.json) {
    console.log(JSON.stringify(results, null, 2));
  } else {
    for (const result of results) printCase(result, args.verbose);
  }

  const metricsByCase: Record<string, Metrics> = {};
  for (const result of results) metricsByCase[result.name] = result.metrics;

  if (args.baselinePath) {
    const baseline = JSON.parse(await readFile(args.baselinePath, "utf8")) as Record<string, Metrics>;
    printBaselineDiff(results, baseline);
  }
  if (args.saveBaselinePath) {
    await writeFile(args.saveBaselinePath, JSON.stringify(metricsByCase, null, 2));
    console.log(`\nWrote baseline to ${args.saveBaselinePath}`);
  }
  if (args.outPath) {
    await writeFile(args.outPath, JSON.stringify(results, null, 2));
    console.log(`Wrote results to ${args.outPath}`);
  }
  if (args.wavDir) console.log(`Wrote rendered audio to ${args.wavDir}`);

  const failed = results.filter((r) => !r.passed);
  if (!args.json) {
    const total = (results.reduce((s, r) => s + r.elapsedMs, 0) / 1000).toFixed(1);
    console.log(
      `\n${results.length - failed.length}/${results.length} cases passed in ${total}s`,
    );
  }
  if (failed.length > 0) process.exitCode = 1;
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
