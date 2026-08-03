# Audio Leveller

A local, [Auphonic](https://auphonic.com)-style audio processing pipeline for
spoken word — everything runs on your machine, nothing is uploaded. Drag a
`.wav` file onto the window and it writes the processed result next to it.

It writes two files next to the input:

- **`<name>_processed.wav`** — the recording through the whole chain: clicks
  repaired, steady noise attenuated, late reverberation suppressed, room and
  microphone colouration corrected, resonances and sibilance tamed, and every
  speech segment normalised to **−23 LUFS** with smooth gain ramps across the
  silences in between.
- **`<name>_roomtone.wav`** — a seamless **room-tone bed** built from the
  cleanest bits of the silences (see below).

## The chain

Six stages, in order. Each decides for itself whether it has anything to do and
says so in the report — a stage that acts on material needing no treatment is a
cost with no benefit, so most of them can decline.

| stage | what it does | when it declines |
| ----- | ------------ | ---------------- |
| `declick` | Finds impulsive damage on the linear-prediction residual and rebuilds it by AR interpolation. | Refuses entirely if detection covers more than 2% of the file — the threshold is wrong for that material. |
| `denoise` | Attenuates background noise — DeepFilterNet3 when its weights are installed, Wiener suppression otherwise. | Scales itself back as the source gets cleaner, to nothing at a 35 dB noise floor; and throws away any backend's output that costs more than 3 dB of programme loudness. |
| `dereverb` | Suppresses late reverberation by weighted prediction error. | Passes dry material through untouched, judged by a blind decay measurement. |
| `eq` | Corrects room and microphone colouration from the long-term average spectrum. | Places no bands when the spectrum is already even. |
| `dyneq` | Suppresses resonances and sibilance while they occur — this is also the de-esser. | Cannot decline (it is per-frame by nature), so its transparency is measured instead. |
| `level` | Normalises each speech segment to a target loudness, with a true-peak limiter. | — |

The order is load-bearing. De-click comes first because impulses are
out-of-distribution for any denoiser and get smeared rather than removed. EQ
comes after the denoiser and dereverberator because both change the spectrum you
would otherwise be fitting a curve to. Levelling is last so the loudness target
is measured on the audio that actually gets written.

## De-click

Detection runs on the linear-prediction residual: over a few tens of
milliseconds speech is well described by an all-pole (AR) model, and a click —
which owes nothing to the samples around it — leaves a spike in the prediction
error far larger than anything speech produces. Three details carry the
quality (see `src/dsp/declick.ts`):

- **Two-sided detection.** One corrupt sample pollutes the *forward* residual
  for a whole model order after it, so thresholding it alone flags (and
  "repairs") ~32 samples of good audio per click. Requiring the *backward*
  residual to agree isolates exactly the damaged samples.
- **Robust statistics.** The threshold is set from the median absolute
  deviation of the residual, not its standard deviation — the clicks
  themselves would inflate a standard deviation and hide behind it. Blocks
  where detections are *dense* are skipped entirely (clicks are rare by
  definition; a dense block is a transient the model doesn't fit), and the
  stage refuses to touch the file at all if detection covers more than 2% of
  it.
- **Repair, then refit, then repair again.** Gaps are rebuilt by least-squares
  AR interpolation (Janssen), which reconstructs the resonance rather than
  bridging the hole. The first repair uses the model that did the detecting —
  fitted on a block *containing the click*, which in quiet audio describes the
  click more than the audio. So the model is refitted on the repaired
  neighbourhood and the gap interpolated once more.

The eval bound reflects what repair can honestly achieve: at a pause site the
residual of even a perfect repair is the unknowable noise realisation that was
under the click (~0 dB against the local peaks); repairs measure −1.2 dB, a
surviving click +40.

## Noise reduction, and the two backends

Two kinds of denoiser are worth having and they are not interchangeable, so
both sit behind one interface and the report says which ran.

- **`spectral`** — Wiener suppression with a decision-directed SNR estimate.
  No weights, no native dependency, always available, and it cannot invent
  detail because it only ever attenuates. This is the default and what every
  baseline number is measured against.
- **`onnx`** — DeepFilterNet3 through ONNX Runtime. Better on real speech, and
  only present if you ask for it.

```bash
pnpm fetch-model
```

That downloads the ONNX export from
[the upstream repository](https://github.com/Rikorose/DeepFilterNet) — the
project's own author, deliberately not one of the community mirrors that carry
the same bytes without the same provenance — checks the archive against a
pinned SHA-256, and checks every file it extracts against its own. Weights are
never bundled: they are large, they carry their own licences, and a local-first
tool should let you decide what to download. `onnxruntime-node` is an *optional*
dependency for the same reason, so the classical backend keeps working for
anyone who does not want a 100 MB runtime. When either is missing the stage says
exactly what is missing and runs the classical backend.

### What the model is, and what had to be written around it

The export is not a denoiser. It is three graphs — an encoder and two decoders —
that consume normalised spectral features and emit an ERB gain mask plus complex
deep-filter taps. The 960-point transform (not a power of two, so
`createFftPlan` exists), the ERB filterbank, the running feature normalisation,
the mask application, the deep filter and the resynthesis are all outside the
model, and each has to match what it was trained on. See `src/models/deepfilternet.ts`,
which is deliberately free of any ONNX dependency so it can be tested without
weights present. Three things there are worth knowing because each contradicts
the obvious guess:

- **There is no recurrent state on the graph boundary.** The GRUs are inside the
  graph and unroll over whatever time axis they are handed, so this is not a
  frame-at-a-time runner threading hidden state. It feeds long spans. The graph
  is also exactly causal — verified by growing the future context and watching
  the output not move — so splitting a file only restarts hidden state, which a
  discarded warm-up prefix covers.

- **The lookahead is the caller's job.** The PyTorch model shifts its own input
  by `conv_lookahead`; the ONNX export does not include that shift, so the
  output of model frame *t* applies to spectrum frame *t − 2*. Get it wrong and
  the mask still "works", it is simply 40 ms early. Measured across candidate
  alignments, the deep filter scores 19.9 dB SI-SDR at a lookahead of 2 and
  about −2 dB at 0, 1 or 3.

- **The ERB mask is only valid above `nb_df`.** This one is a departure from
  upstream's own runtime, and it is the difference between a working denoiser
  and a speech destroyer. The training graph keeps the masked spectrum only for
  bins the deep filter does not cover (`spec_e[..., nb_df:, :] = spec_m[..., nb_df:, :]`),
  so the network was never given a reason to emit sensible gains below that
  boundary — and it does not: on clean speech the bands below bin 96 come back
  at about 0.25, a flat −12 dB, while the bands above sit near unity. libDF gets
  away with masking the whole spectrum because the deep filter then overwrites
  the low bins, but its `apply_stages` has a branch where the mask runs and the
  deep filter does not, and there the junk gains reach the output. Measured:
  masking the full spectrum scores 3.8 dB SI-SDR where confining the mask to the
  bins it was trained for scores 25.4, and on *clean* input 6.6 dB against 51.6.

`reductionDb` is honoured as upstream's `atten_lim_db`: a fixed fraction of the
noisy spectrum is mixed back, which bounds the attenuation of any bin to that
many dB while leaving bins the model already passes through untouched. A plain
wet/dry blend would dilute the speech by the same fraction it dilutes the noise.
Running a neural denoiser at full strength sounds obviously processed, so the
default is 12 dB, tapered to zero as the source gets clean.

### Results, including the bad one

On **real speech** at 20 dB SNR (a fixture plus broadband noise), denoise stage
alone, scored against the clean recording:

| backend | noise removed | SI-SDR | programme loudness |
| ------- | ------------- | ------ | ------------------ |
| none (input) | — | 21.20 dB | — |
| `spectral` | 10.7 dB | 21.03 dB (**−0.2**) | −0.20 dB |
| `onnx` | 8.2 dB | 26.22 dB (**+5.0**) | −0.05 dB |

The classical backend removes *more* noise and still ends up further from clean:
it is trading signal for quiet. That gap is the whole case for the download.
Forced onto already-clean real speech with the taper disabled, the model scores
42.8 dB SI-SDR and costs 0.00 dB of programme loudness.

And the bad one: **on this project's synthetic corpus the model destroys the
programme.** Given `noisy-20db` it attenuates the voice by 10 dB — it reads a
harmonic stack with formants as noise, because that is what it is. This is not
evidence the port is wrong; it is evidence that a corpus built to evaluate a
spectral suppressor cannot evaluate a trained model. A suppressor asks only what
is stationary. A model asks whether it is hearing a voice, and about synthetic
speech it is entitled to say no.

Two things follow, and both are in the code rather than in this paragraph:

- The stage measures the gated programme loudness a backend cost and **discards
  its output** when it exceeds `maxProgrammeLossDb` (3 dB), returning the input
  untouched and saying so in the report. A trained backend fails by confidently
  rewriting the signal, not by doing too little, so this guard matters more for
  the model than for the classical one. `ood-denoise-onnx` asserts it fires.
- The model's *quality* bounds live on fixture cases, which need a real
  recording. There is no synthetic quality bound, because there could not be an
  honest one.

Known limitation: the model runs at **48 kHz only**. At any other rate the
backend declines with a reason and the classical one takes over, rather than
resampling into and out of 48 kHz inside the stage — two conversions to reach a
denoiser is not obviously better than the classical one that needs neither, and
that is not a trade this corpus can settle.

## Architecture

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

CI runs the same three checks on every pull request, plus the REAPER script's
own test — `.github/workflows/ci.yml`.

```bash
pnpm test           # run the Vitest suite
pnpm test:watch     # watch mode
pnpm typecheck      # tsc --noEmit
pnpm eval           # run the evaluation corpus (see below)
pnpm fetch-model    # download and verify the DeepFilterNet3 weights
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

## Evaluation harness

```bash
pnpm eval                                   # run the corpus, exit non-zero on a broken bound
pnpm eval --verbose --case noisy            # every metric for one case
pnpm eval --baseline eval/baseline.json     # what moved since the baseline
pnpm eval --wav /tmp/out                    # dump each case's input and output to listen to
```

The corpus is synthetic and seeded — speech-*shaped* material (harmonic stack,
formant resonances, syllable-rate envelope, pauses) degraded in known ways, so
a number that changes always means the code changed. Real recordings dropped
into `eval/fixtures/` are picked up automatically as extra cases; see the README
there.

Cases needing something this machine lacks — the model weights — are reported as
**skipped** rather than dropped, because a check nobody ran must not look like
one that passed. The denoiser is pinned to a named backend in every case, so the
numbers do not depend on what happens to be sitting in `~/.audio-leveller`.

A limit worth stating plainly: **a synthetic corpus cannot evaluate a trained
model.** Speech-shaped material is enough for a spectral suppressor, which asks
only what is stationary, and not enough for a model that asks whether it is
hearing a voice — DeepFilterNet3's answer about this corpus is no, and it
removes the programme. So the model's quality bounds live on the fixture cases,
which need a real recording, and the synthetic cases assert only what must hold
whatever the material: that the stage catches a backend doing this and throws
the result away.

Each case states **bounds with reasons**, not snapshots of current behaviour, and
`pnpm eval` fails when one breaks. The metrics that matter:

| metric | what it catches |
| ------ | --------------- |
| `segmentLufsError` | did every speech segment land on target |
| `outputPeakDbfs` | did the limiter ceiling hold |
| `segmentSnrGainDb` | **the transparency check** — did a stage move speech and noise apart |
| `snrGainDb` | the whole-file version: what levelling *costs* in noise |
| `siSdrGainDb` | did the chain damage the signal relative to a clean reference |
| `outputClickResidualDb` | how *audible* the worst surviving click is: residual peak vs the local peaks around it (±10 ms) |
| `changeDb` | did a stage touch the audio at all (`-inf` = bit-identical) |

Two of these deserve a note, because getting them wrong makes a harness that
lies to you:

- **SI-SDR is scored against the clean reference put through the same chain**,
  not against the raw reference. The leveller changes segment levels on purpose;
  comparing against raw audio would score that intended change as damage.

- **`segmentSnrGainDb` is local, `snrGainDb` is not.** Pulling segments toward a
  common level genuinely shrinks whole-file programme-to-floor distance, because
  boosting a quiet passage boosts its noise with it — so that figure is a
  diagnostic, not a bound. Measured *within* a segment, where a single gain
  applies, SNR is invariant, and that is the one worth asserting on. It is also
  the number a denoiser has to move.

### What the current baseline says

Findings worth carrying forward:

- **Clicks were breaking segmentation, not just ears** — phase 1 measured ~8 LU
  of segment-levelling error on the `clicky` case, because a click landing in a
  pause lifted it over the silence threshold and merged two segments at
  different levels. With de-click ahead of the leveller (phase 2), that error
  is 0.32 LU and SI-SDR on the case swung from −20.7 to +29.3 dB. The eval
  bounds now hold segmentation to ≤1.5 LU with clicks present.

- **Noise degrades the leveller's decisions.** SI-SDR drifts about 4–5 dB on the
  noisy cases even though nothing in the chain touches noise: the leveller is
  measuring segment loudness *through* the noise and choosing different gains
  than it would on clean audio. This is the baseline the denoiser has to beat.

- **Click-repair quality has a physical floor, and the metric had to learn it.**
  The click-audibility metric was rewritten twice: whole-file-RMS normalisation
  punished good repairs in loud speech and forgave bad ones in silence, and
  local-RMS still sat crest-factor above a perfect repair. It now compares the
  residual peak against the local peaks (±10 ms) — like for like. At a pause
  site even a perfect repair scores ~0 dB on this measure, because the residual
  is the noise realisation that was under the click, which nothing can know.

## Roadmap

The pipeline skeleton is in place; the stages that make this an Auphonic
replacement rather than a loudness tool are still to come.

| Phase | Status | Deliverable |
| ----- | ------ | ----------- |
| **0** | ✅ done | Pipeline skeleton: stage contract, memoised analysis, lazy resampling, progress, JSON report. Leveller ported to a stage, no behaviour change. |
| **1** | ✅ done | Evaluation harness — seeded synthetic corpus, bounds with stated reasons, baseline comparison. Real fixtures picked up from `eval/fixtures/`. |
| **2** | ✅ done | De-click: two-sided AR residual detection, MAD threshold, Janssen interpolation with a refit pass. `segmentLufsError` on `clicky` went 7.93 → 0.32 LU, SI-SDR −20.7 → +29.3 dB, worst repair −1.2 dB against local peaks. |
| **3** | ✅ done | Corrective EQ fitted to the long-term average spectrum (gain-limited bells, cut-biased, boosts gated on noise), rumble high-pass, and a true-peak limiter replacing the sample-peak one. `spectralFlatteningDb` on `boxy` +4.35 dB, +1.22 dB on clean material. |
| **4** | ✅ done | Noise reduction with a pluggable backend. The classical spectral suppressor is built, tested and default: +11.8 dB of segment SNR on a 20 dB-noise source, scaled back automatically on clean ones. DeepFilterNet3 now runs through ONNX Runtime when its weights are installed — +5.0 dB SI-SDR on real noisy speech where the classical backend manages −0.2 — with a programme-loss guard that discards any backend's output when it starts removing the voice instead of the noise. |
| **5** | ✅ done | Dereverb by weighted prediction error (WPE). The model options in the original plan were unreachable, so the classical one got built — and it earns its place on merit: it cannot invent speech, only subtract a linear prediction. Modest, honestly: ~12% off the decay, +1.3 dB SI-SDR. Engages only on genuinely reverberant material. |
| **6** | ✅ done | Dynamic EQ: per-bin suppression of whatever protrudes above the spectrum's own local envelope, with attack/release smoothing. Doubles as the de-esser via extra sensitivity in the sibilance band. +5.1 dB SI-SDR on a ringing resonance, ~5% of cells touched on clean speech. |
| **7** | ✅ done | Chain inspector in the app: every stage says what it actually did, and can be toggled off and re-rendered for A/B. |

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
