# Audio Leveller

A local, [Auphonic](https://auphonic.com)-style audio processing pipeline for
spoken word — everything runs on your machine, nothing is uploaded. Drag a
`.wav` file onto the window and it writes the processed result next to it.

Today the chain contains one stage, the leveller it grew out of:

- **`<name>_processed.wav`** — every speech segment normalised to **−23 LUFS**
  with smooth gain ramps across the silences in between.
- **`<name>_roomtone.wav`** — a seamless **room-tone bed** built from the
  cleanest bits of the silences (see below).

De-click, corrective EQ, dynamic EQ and AI noise/reverb removal are the stages
being built next — see [Roadmap](#roadmap).

## The pipeline

Processing is a list of **stages**, each a pure function from a signal plus
parameters to a new signal plus a report. Uniform stages are what make per-stage
bypass, an honest JSON report of every decision, and golden-file testing
possible. See `src/pipeline/`.

Three things the runner does deliberately:

- **Analysis follows the audio.** Each stage is handed an `Analyzer` over the
  signal *as it arrives*, not over the original file — a stage that changes the
  spectrum invalidates any measurement taken before it. Analyzers memoise, so
  K-weighting a long file happens once per distinct signal rather than once per
  question. Stages that genuinely need the untouched original get it separately
  via `ctx.source`: room tone has to be harvested before a denoiser deletes it.

- **Sample-rate conversion is lazy.** Rather than forcing everything through a
  canonical 48 kHz, a stage declares `requiredSampleRate` only if it needs one
  (the AI stages will; the leveller doesn't, since its K-weighting coefficients
  are derived analytically per rate). Conversion happens once in and once out,
  and only when some enabled stage actually asks for it — so a 44.1 kHz file
  through a rate-agnostic chain is never resampled at all, and a fully bypassed
  chain is **byte-identical** to its input.

- **Stages own their extra outputs.** The room-tone bed is an `extras` entry on
  the level stage's output, written as `<name>_roomtone.wav`. Any stage added
  later gets an output file by declaring one; nothing in the writing glue
  changes.

## How the leveller works

1. **Loudness measurement — ITU-R BS.1770 / EBU R128.** Audio is K-weighted
   (a high-shelf "head" filter + a ~38 Hz RLB high-pass, coefficients derived
   analytically for the file's sample rate) and measured as gated integrated
   loudness: 400 ms blocks at 75 % overlap, an absolute −70 LUFS gate and a
   relative −10 LU gate. This is exactly what "−23 LUFS" means, so every
   measurement is standards-correct. See `src/dsp/kweighting.ts` and
   `src/dsp/loudness.ts`.

2. **Silence detection.** A short window (100 ms, 25 ms hop) slides across the
   file; each window's loudness is measured. The silence threshold is set
   between the estimated noise floor (10th-percentile window loudness) and the
   integrated loudness:

   ```
   threshold = floor + fraction · (integrated − floor)      // fraction = 0.25 default
   ```

   which is the "a quarter of the way from the floor to the integrated
   loudness" heuristic. **Otsu's method** (the bimodal-histogram valley trick
   from image thresholding) is also available via `method: "otsu"`. Runs of
   sub-threshold windows longer than `minSilenceSec` (default 1 s) become
   silence regions. See `src/dsp/silence.ts`.

3. **Segmentation & gain.** Segment boundaries are the file edges plus every
   silence *midpoint* — so a segment runs from the middle of one silence to the
   middle of the next. Each segment's integrated loudness gives a target gain
   `−23 − measured` dB.

4. **Interpolated envelope.** Gain is held constant across each segment's
   speech body and **ramped linearly across each silence** from the left
   segment's gain to the right's. So if segment A was cut −3 dB and segment B
   boosted +2 dB, the silence between them ramps −3 → +2 dB. A feed-forward
   peak limiter (default −1 dBFS ceiling) catches anything the boosts push
   over. See `src/dsp/leveller.ts`.

5. **Room-tone bed.** From each detected silence the cleanest window is
   extracted. Room tone is the *quietest steady* part of a silence, so the
   cleanliness score is loudness-first (a breath is simply louder than the
   floor), with extra penalties for clicks (detected as a ~10 ms block whose
   peak towers over the *median* block peak — robust to noise's natural
   peakiness) and swells (high short-window RMS variation); silences much
   louder than the cleanest are dropped entirely. The winning clips are
   concatenated with equal-power
   crossfades and looped
   (every other repeat time-reversed, so identical material never crossfades
   into itself and builds up level) until the bed is at least
   `minDurationSec` (10 s) long. The whole bed is gained by the
   **length-weighted mean of the speech-segment gains**, so it sits at the same
   level as the processed voice. See `src/dsp/roomtone.ts`.

The DSP core (`src/dsp/`) has **zero Electron dependencies** and is fully unit
tested, including an end-to-end test that processes a synthetic multi-segment
signal and re-measures each segment back at −23 LUFS.

## Usage

### App

```bash
pnpm install
pnpm start          # build + launch the Electron window
```

Drag a `.wav` onto the window. Supported formats: 8/16/24/32-bit PCM and
32-bit float, mono or stereo, any sample rate.

### CLI

```bash
pnpm build
node dist/cli.js input.wav [output.wav] [options]
```

Prints what the chain did — input/output loudness and peak, which stages ran
and how long each took, then the leveller's own report (silence threshold, each
segment's measured loudness and applied gain, room-tone summary).

| option           | meaning                                                    |
| ---------------- | ---------------------------------------------------------- |
| `--only <a,b>`   | run only these stages, in chain order                       |
| `--bypass <a,b>` | run the chain but bypass these stages                       |
| `--target <lufs>`| target loudness for the level stage (default −23)           |
| `--report <file>`| write the full JSON report to a file                        |
| `--json`         | print the JSON report instead of the text summary           |
| `--quiet`        | suppress progress output                                    |
| `--list-stages`  | list the available stages and exit                          |

`--bypass` plus `--report` is the tuning loop: render the same file with a stage
on and off, compare the reports, and listen to the two outputs.

## Development

```bash
pnpm test           # run the Vitest suite
pnpm test:watch     # watch mode
pnpm typecheck      # tsc --noEmit
pnpm dist           # package a distributable (electron-builder)
```

### Tuning

`levelAudio(audio, options)` / `processFile(path, options)` accept:

| option           | default | meaning                                            |
| ---------------- | ------- | -------------------------------------------------- |
| `targetLufs`     | `-23`   | target loudness per segment                        |
| `minSilenceSec`  | `1.0`   | minimum gap length to count as silence             |
| `method`         | `"fraction"` | silence threshold: `"fraction"` or `"otsu"`   |
| `fraction`       | `0.25`  | floor→integrated fraction for the threshold        |
| `windowSec`      | `0.1`   | silence-detection window length                    |
| `hopSec`         | `0.025` | silence-detection hop (resolution)                 |
| `maxGainDb`      | `30`    | clamp on per-segment gain                           |
| `ceilingDb`      | `-1`    | output limiter ceiling (dBFS)                       |

Room-tone options are nested under `roomtone`:

| `roomtone.*`      | default | meaning                                            |
| ----------------- | ------- | -------------------------------------------------- |
| `minDurationSec`  | `10`    | loop the bed until it is at least this long         |
| `edgeTrimSec`     | `0.15`  | trim off each silence end (speech tails / breaths)  |
| `minClipSec`      | `0.3`   | ignore silences with a shorter usable core          |
| `maxClipSec`      | `1.0`   | cap how much one silence contributes                |
| `crossfadeSec`    | `0.05`  | crossfade length between clips                      |
| `keepMarginDb`    | `6`     | drop clips more than this (dB) dirtier than the best |

## Project layout

```
src/dsp/         pure, tested DSP core (wav, resample, kweighting, loudness, silence, roomtone, leveller)
src/pipeline/    stage contract, memoised analyzer, registry, runner
src/stages/      the stages themselves, plus the default chain order
src/process.ts   read → run pipeline → write glue (shared by worker + CLI)
src/main/        Electron main process (window + IPC)
src/preload/     contextBridge API (getPathForFile, processFile, onProgress)
src/renderer/    drag-and-drop UI
src/worker/      worker thread that runs the pipeline off the main thread
src/cli.ts       command-line entry point
test/            Vitest suite
reaper/          native REAPER ReaScript (non-destructive take-volume automation)
```

### Adding a stage

Implement `Stage`, add it to `BUILT_IN` in `src/stages/index.ts` at the right
point in the chain, and it appears in `--list-stages`, `--bypass`, the report
and the UI automatically:

```ts
export const declickStage: Stage<DeclickParams, DeclickReport> = {
  name: "declick",
  description: "Detect and interpolate over impulsive clicks",
  defaults: DEFAULT_DECLICK_OPTIONS,
  render(signal, params, ctx) {
    // ctx.analysis measures the signal as it arrives; ctx.progress(f) reports.
    return { signal: cleaned, report };
  },
};
```

Chain order encodes real decisions: de-click before the denoiser (impulses are
out-of-distribution for the model and get smeared rather than removed), EQ after
it (denoising changes the spectrum you'd otherwise fit a curve to), and levelling
last so the loudness target is exact.

## Roadmap

The pipeline skeleton is in place; the stages that make this an Auphonic
replacement rather than a loudness tool are still to come.

| Phase | Status | Deliverable |
| ----- | ------ | ----------- |
| **0** | ✅ done | Pipeline skeleton: stage contract, memoised analysis, lazy resampling, progress, JSON report. Leveller ported to a stage, no behaviour change. |
| **1** | next | Evaluation harness — a fixture corpus and a `pnpm eval` that renders it and reports per-stage measurements, so later stages can be judged on evidence rather than vibes. |
| **2** | | De-click: LPC/autoregressive detection and interpolation. Pure DSP, deterministic, no external dependencies. |
| **3** | | Corrective EQ from the long-term average spectrum (a handful of gain-limited shelves and bells, not a 31-band match), rumble high-pass, and a true-peak limiter to replace the current sample-peak one. |
| **4** | | AI denoise via ONNX (`onnxruntime-node`) with DeepFilterNet3 — models fetched on first run rather than bundled. |
| **5** | | Dereverb — an evaluation between ClearerVoice, Resemble-Enhance and classical WPE before committing to one. |
| **6** | | Dynamic EQ / resonance suppression, doubling as the de-esser. |
| **7** | | UI: chain inspector with per-stage bypass and A/B. |

A note on the generative enhancers (phases 4–5): they hallucinate plausible
speech detail, which is fine for intelligibility and bad when the speaker's
actual voice is the point. They will default to conservative settings.

## REAPER script

The REAPER script deliberately does not track the pipeline — no ONNX, no heavy
DSP in ReaScript. It stays a level-only tool.

`reaper/audio_leveller.lua` is a native, non-destructive port for REAPER: it
writes a **take volume envelope** that levels each segment to −23 LUFS instead
of rendering a new file, and reads audio through REAPER's decoder so it works
with any supported format. The leveling maths are a validated port of the DSP
core — `lua reaper/test_dsp.lua` (or `pnpm test:reaper`) re-levels a synthetic
signal and checks it lands at −23 LUFS, matching the Node tool to ~0.1 dB. See
`reaper/README.md` for install/usage and the REAPER-side checklist.
