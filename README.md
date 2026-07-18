# Audio Leveller

A tiny, [Levelator](https://en.wikipedia.org/wiki/The_Levelator)-style loudness
tool. Drag a `.wav` file onto the window and it writes two files next to it:

- **`<name>_processed.wav`** — every speech segment normalised to **−23 LUFS**
  with smooth gain ramps across the silences in between.
- **`<name>_roomtone.wav`** — a seamless **room-tone bed** built from the
  cleanest bits of the silences (see below).

## How it works

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
node dist/cli.js input.wav [output.wav]
```

Prints the loudness report (integrated loudness, silence threshold, each
segment's measured loudness and applied gain, plus the room-tone summary) and
writes both the processed file and the room-tone bed.

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
src/dsp/         pure, tested DSP core (wav, kweighting, loudness, silence, roomtone, leveller)
src/process.ts   read → level → write glue (shared by worker + CLI)
src/main/        Electron main process (window + IPC)
src/preload/     contextBridge API (getPathForFile, processFile)
src/renderer/    drag-and-drop UI
src/worker/      worker thread that runs the DSP off the main thread
src/cli.ts       command-line entry point
test/            Vitest suite
```
