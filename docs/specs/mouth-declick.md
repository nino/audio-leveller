# Spec: a mouth de-click stage

Status: **specified, not built.** Written to be handed to a session that has
annotated recordings in hand. Every number below is a starting point, not a
result — the whole point of the calibration step is that the operating point
comes from the annotations, not from this document.

---

## What this is, and what it is not

A **mouth click** is a real acoustic event: a tongue release, lip separation,
or a bubble of saliva breaking. It is a few milliseconds long, biased towards
2–8 kHz, and it happens in the _lulls_ — between phonemes, at the start of a
word, in the gap before a breath. It is produced by the vocal tract. That is
precisely what makes it hard.

A **digital click** is three or four samples that are simply wrong. The
existing `declick` stage handles those, and handles them well.

The two need different detectors and, more importantly, **different repairs**.
This stage does not attempt to reconstruct anything. It attenuates.

### Why `declick` cannot be tuned into doing this

Worth stating up front, because "just relax the thresholds" is the obvious
first idea and it is wrong. Four separate mechanisms in `src/dsp/declick.ts`
reject mouth clicks, and three of them do so independently:

| Mechanism         | Default         | Why it rejects a mouth click                                                                                                                                        |
| ----------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `maxBurstSec`     | 2 ms            | A run longer than 88 samples at 44.1 kHz is never even recorded as a detection (`declick.ts:255`). Mouth clicks routinely run longer.                               |
| `maxBlockDensity` | 5%              | ~19 samples of a usable block. A click that fragments into shorter runs still blows this, and the whole block's detections are discarded (`declick.ts:266`).        |
| Pulse-train veto  | 0.3×, 2.5–20 ms | Mouth clicks sit within 20 ms of speech that is _louder than they are_. Vetoed essentially always.                                                                  |
| MAD self-masking  | —               | A 3 ms event inside a 10 ms block is 30% of that block, so the median absolute residual is computed largely from the event itself and the threshold lifts above it. |

Raising `maxBurstSec` and `maxBlockDensity` far enough to admit mouth clicks
also admits door slams, plosives and dense crackle — which is exactly what
those two numbers were installed to refuse. And even with detection solved,
`interpolateGap` is a dense `gapLength × gapLength` solve: a 5 ms gap is 220
samples, so ~10M operations per event on an increasingly ill-conditioned
system. The deeper objection is conceptual. "Continue the ringing from both
sides and meet in the middle" is a claim about a few milliseconds of
_stationary_ resonance. Across 5 ms at a word boundary there is no shared
resonance to continue — different formants either side, or silence on one of
them. Interpolating there is inventing signal.

---

## Position in the chain

```
declick → demouth → denoise → dereverb → eq → dyneq → expand → compress → level
```

**After `declick`**, because a true impulse is better repaired than ducked.
Let the AR interpolator take its share first; what remains is genuinely
mouth-shaped.

**Before `denoise`**, for the same reason `declick` is: impulses are
out-of-distribution for the model backend, which smears them into chirps
rather than removing them. A smeared click is both still audible and much
harder to detect.

**Before the dynamics stages**, because a compressor will pump on an
unremoved click, and an expander may duck the lull the click sits in — moving
the very context this detector measures against.

Add to `BUILT_IN` in `src/stages/index.ts` in that position, and extend the
header comment there: that list is where the project records its ordering
decisions, and this one needs the two paragraphs above.

---

## Detection

### Signal path

Per channel, mirroring `declick`. Damage is usually correlated across channels
for a single-mic podcast, but keeping them independent costs little and avoids
one channel's decision rewriting the other.

Everything is specified in Hz and seconds and derived per sample rate, so the
stage is rate-agnostic and sets no `requiredSampleRate`. See the Nyquist
refusal below for the one case where that breaks down.

1. **Band split.** Two cascaded `butterworthHighPass(sampleRate, hfCornerHz)`
   from `src/dsp/biquad.ts` (24 dB/oct), default corner **1800 Hz**. This is
   the click band: high enough to reject vowel F1/F2 energy, low enough to
   catch the bottom of a tongue click.

   _No low-pass._ The obvious worry is sibilance, which lives in the same
   band — but the discriminator against `/s/` is not spectral, it is temporal.
   A sustained fricative raises the short envelope and the local context
   envelope _together_, so its prominence ratio stays near unity. Adding a
   low-pass would buy nothing and would cost the top octave of a genuine click.

2. **Envelopes**, on a hop of **0.25 ms** (fine, because the events are
   1–10 ms), each an RMS over a **1.0 ms** window:
   - `hfShort[k]` — the high-passed signal
   - `fullShort[k]` — broadband

3. **Context envelopes**, over ±**250 ms** around each hop:
   - `hfContext[k]` = **median** of `hfShort`. Median, not mean, for the same
     reason `declick` uses MAD rather than standard deviation: the clicks are
     part of the data being measured, and a mean would let them inflate their
     own threshold.
   - `speechContext[k]` = **80th percentile** of `fullShort`. A high
     percentile, not the median — this quantity means "how loud is the speech
     around here", and a window spanning pauses would drag a median down and
     make every gap look like a lull.
   - `hfFloor[k]` = **10th percentile** of `hfShort` — the HF noise floor near
     the event. Used by the repair, not the detector.

### Decision

Two gates, both required:

```
prominence  P[k] = 20·log10(hfShort[k] / hfContext[k])       ≥ prominenceDb
lull        L[k] = 20·log10(fullShort[k] / speechContext[k]) ≤ lullDb
```

Starting points: `prominenceDb = 12`, `lullDb = -12`.

Candidate hops are extended to their half-power extent to give `start`/`end`,
candidates closer than `mergeMs` (2 ms) are merged, and each merged event must
then pass:

- **Duration** in `[minDurationMs, maxDurationMs]` = `[0.5, 15]`
- **Attack**: 10%-of-peak to peak within `maxAttackMs` = 1.5 ms, measured on
  `hfShort`

### What each gate is actually rejecting

This table is the argument for the design, and it is what to check first when
the calibration numbers come back wrong:

| Confusable              | Rejected by                  | Mechanism                                                                                                                  |
| ----------------------- | ---------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Sibilance `/s/ /ʃ/`     | prominence, duration, attack | Sustained, so it lifts `hfContext` with itself; 50–200 ms; ramps rather than snaps                                         |
| Plosive `/p/ /t/ /k/`   | lull                         | LF-dominant burst, and it _is_ the speech onset — `fullShort` is high by definition                                        |
| Glottal pulse           | lull                         | Occurs during voicing, where `fullShort ≈ speechContext`                                                                   |
| Breath                  | duration, attack             | 100–400 ms, gradual                                                                                                        |
| Room-tone transient     | prominence                   | Not HF-prominent against its own context                                                                                   |
| Surviving digital click | _nothing_                    | Passes every gate. Acceptable — `declick` ran first, and a duck is a defensible fallback. Report it rather than hiding it. |

---

## Repair

**Attenuation, not interpolation.** This is the central design decision and
the reason the stage is tractable at all: you do not have to reconstruct
anything, you only have to get the event below the masking threshold of what
surrounds it. It also fails gracefully — a false positive costs a
barely-audible dip, not interpolated mush where a consonant was.

### Depth

Duck towards the local HF noise floor, and stop just above it:

```
targetDb  = 20·log10(hfFloor[event] / hfShortPeak[event]) + floorHeadroomDb
gainDb    = clamp(targetDb, -maxAttenuationDb, 0)
```

with `floorHeadroomDb = +3` and `maxAttenuationDb = 18`.

The headroom term is not decoration. **Ducking below the local room tone is
the classic mouth-declick artefact** — it punches an audible hole where the
click was, and a hole is more noticeable than the click. Landing a few dB
above the floor leaves something that reads as room rather than absence.

### Envelope shape

Flat `gainDb` across `[start, end]`, with **raised-cosine ramps of
`rampMs = 1.5`** either side, applied to the broadband signal.

The ramp length matters in both directions. A step in gain _is_ a click — so
the ramp cannot be arbitrarily short. But a long ramp ducks the speech either
side of the event, which is the collateral damage this stage is judged on. At
1.5 ms the ramp's own spectral splatter sits above 1 kHz and far below the
signal, which is the compromise.

**Ramp clamping.** A ramp must not extend into speech. Shorten it on any side
where `fullShort` rises above the lull threshold before the ramp completes,
and if it cannot fit at least `minRampMs = 0.5`, **reject the event entirely
rather than clicking on its own ramp**. Mouth clicks immediately before a word
onset are common, so expect this path to be exercised.

### An alternative worth measuring

Ducking only the HF band (split, attenuate the high side, recombine) preserves
whatever low-frequency content overlaps the event. Since detection already
requires a lull there should be little LF content to preserve, which is why
broadband is the default — but if listening shows the ducks sounding hollow,
this is the first thing to try. Spec it as `attenuationMode:
"broadband" | "hf-only"` if you build it; do not build it speculatively.

---

## Refusals

The project's house style is that a stage declines rather than guesses, and
this stage needs it more than most, since it acts on real audio rather than
on damage. Five, mirroring `declick`'s structure:

1. **`maxDuckedFraction`** (default **0.02**) — if detection covers more than
   2% of the file, the threshold is wrong for this material. Decline entirely,
   change nothing, and set `aborted: true` in the report. Direct analogue of
   `declick`'s `maxRepairFraction`.
2. **`maxEventsPerMinute`** (default **60**) — a rate cap, catching the case
   where events are short enough to stay under the fraction cap but far too
   frequent to be real. A smacky talker runs 5–40/min.
3. **Duration bounds** — per event, as above.
4. **Ramp-fit failure** — per event, as above.
5. **Nyquist guard** — if `hfCornerHz > 0.4 × sampleRate` the click band does
   not exist at this rate. Decline, and say so in the report rather than
   silently doing nothing. (At 16 kHz input, the 1800 Hz corner is fine; at
   8 kHz it is not.)

Every refusal goes in the report. A silently absent check is worse than a
missing one.

---

## Parameters

Full set lives in `DEFAULT_MOUTHCLICK_OPTIONS` in `src/dsp/mouthclick.ts`,
with a doc comment per field explaining the reasoning — follow
`DeclickOptions` as the model; those comments are the real documentation.

`src/params.ts` exposes only what changes how it _sounds_. Three:

| Key                 | Label        | Range                 | Help                                                                                                                                                 |
| ------------------- | ------------ | --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `prominenceDb`      | Sensitivity  | 6–24 dB, step 1       | How far a short high-frequency burst must stand above its surroundings to count as a mouth click. Lower finds more, and eventually finds consonants. |
| `maxAttenuationDb`  | Reduction    | 3–24 dB, step 1       | How far a click may be ducked. Deeper is not always better: past the room tone it leaves an audible hole.                                            |
| `maxDuckedFraction` | Repair limit | 0.002–0.1, step 0.002 | If more than this fraction of the file looks like mouth noise, the stage declines rather than ducking its way through the recording.                 |

---

## Calibration against annotations

**This is the part that decides whether the stage works**, and it must happen
before any default is written into the source. The de-clicker's glottal-pulse
episode — a fix that was well-argued, measured, guarded by a test, and wrong,
because the corpus was synthetic — is the precedent to avoid repeating.

### Ground truth

The annotator at `/annotate` already produces what is needed. Drop WAVs in
`listening/annotate/`, mark regions with `m` (`mouth-noise`), and it saves
`<name>.annotations.json` (`AnnotationFile` in `src/listen/types.ts`).
`/api/annotate/export.csv` flattens every file to
`file,start,end,label,confidence,note`.

Annotate the confusables too — `breath`, `plosive`, `click` — not just the
mouth noise. A precision number is nearly useless without knowing _what_ the
false positives are, and the confusion matrix is the thing that will tell you
which gate to move.

Use the `sure`/`maybe` field: score recall on `sure` regions only, and treat
`maybe` regions as neither hit nor false positive. That is what the field is
for and it keeps the borderline cases from dominating a small corpus.

### The calibration script

New entry point `src/eval/calibrate-mouth.ts`, wired as
`"calibrate:mouth": "pnpm build && node dist/eval/calibrate-mouth.js"`:

1. Load every `listening/annotate/*.annotations.json` and its WAV.
2. Run detection only (no repair) over a grid of `prominenceDb` ×
   `lullDb` — say 8–20 dB × −6 to −20 dB.
3. Match detections to annotations: a detection is a hit if its midpoint falls
   inside a `mouth-noise` region widened by ±20 ms.
4. Per grid point, report precision, recall, F1, **and the confusion
   breakdown** — how many false positives landed in `breath`, in `plosive`, in
   `click`, in nothing labelled at all.
5. Print the recommended operating point and the numbers behind it.

**Choose for precision over recall.** A missed mouth click is a mouth click —
the listener was going to hear it anyway. A false positive is a ducked
consonant in material nobody will audition frame by frame. This matches the
bias the whole project is built around, and it is the same trade the
pulse-train veto already makes deliberately.

Write the chosen point, the corpus it came from, and the precision/recall it
achieved into the doc comment next to the default. A default without that
provenance is a number nobody can later argue with.

---

## Eval integration

### Harness change

`fixtureCases()` in `src/eval/run.ts` currently reads WAVs only. Teach it to
look for `<name>.annotations.json` beside each fixture WAV (same convention
the annotator uses) and, when one exists with `mouth-noise` regions, emit an
extra case. Document in `eval/fixtures/README.md` that the annotation file
should be copied across from `listening/annotate/`.

### New metrics (`src/eval/metrics.ts`)

- **`mouthAttenuationDb`** — the _worst_ (least negative) attenuation across
  annotated `sure` mouth-noise regions, measured as
  `20·log10(hfPeakAfter / hfPeakBefore)` within each region. Worst case, not
  mean: one untouched click is an audible failure regardless of the average.
- **`changeDbOutside(before, after, regions, guardSec)`** — `changeDb`
  restricted to samples outside every annotated region widened by a guard.
  This is the collateral-damage measure and **the bound that actually
  matters**; a stage that ducks everything will score beautifully on
  attenuation.

### New cases

```
fixture:<name>:mouth          demouth alone, on the annotated excerpt
  mouthAttenuationDb  max -8   every annotated click must actually be reduced
  changeDbOutside     max -45  ...without touching anything that is not one

fixture:<name>:clean-mouth    demouth alone, untouched excerpt, no annotations needed
  changeDb            max -40  the transparency check, mirroring clean-declick
```

Bounds are placeholders. Set them from what the calibrated stage actually
achieves, then state in `because` why that number is the right one — a bound
you cannot justify in a sentence is a bound that will be quietly relaxed the
first time it fails.

### What the synthetic corpus can and cannot do

It **cannot** validate the operating point. `syntheticSpeech` has no lips,
no tongue and no saliva, and its Rosenberg-pulse voice is already known to be
more impulsive than a real glottis — that is exactly the property that
certified the de-clicker's wrong fix. Any synthetic mouth click injected into
a synthetic gap will be trivially detectable and will tell you nothing about
precision on real speech.

It **can** test the mechanics, and should. See below.

---

## Tests (`test/mouthclick.test.ts`)

Add a `mouthClick(seconds, sampleRate, amp)` generator to `test/helpers.ts`
alongside the existing `breath()` — a Gaussian-windowed burst of noise centred
around 4 kHz, ~3 ms long.

1. **Detects and attenuates.** Burst injected into a silent gap of
   `syntheticSpeech`: detected, reduced by ≥ 8 dB.
2. **Does not touch the speech.** Same signal: change outside the event
   region below −45 dB.
3. **The duck does not itself click.** Max sample-to-sample delta after
   processing must not exceed the max before it. This is the ramp test and it
   is the one most likely to catch a real bug.
4. **Refusals fire.** A signal densely packed with HF bursts sets `aborted`
   and returns the input bit-identical.
5. **Ramp clamping.** A burst placed 2 ms before a speech onset is either
   rejected or ducked without the ramp reaching into the onset.
6. **Rate-agnostic.** Comparable attenuation at 44.1 and 48 kHz.
7. **Nyquist guard.** An 8 kHz-rate signal declines with a stated reason.

Put a comment at the top of the file saying these test mechanics, not the
operating point, and that the operating point is judged by
`pnpm calibrate:mouth` on real recordings. The existing standing instruction
in `src/dsp/declick.ts` — _judge this stage on real recordings_ — applies here
with more force, not less.

---

## Files

**Create**

- `src/dsp/mouthclick.ts` — detector and repair, pure DSP, `DeclickOptions`-style doc comments
- `src/stages/demouth.ts` — stage wrapper
- `src/eval/calibrate-mouth.ts` — the calibration harness
- `test/mouthclick.test.ts`

**Touch**

- `src/dsp/index.ts` — re-export, after `./declick`
- `src/stages/index.ts` — register between `declick` and `denoise`; extend the ordering comment
- `src/params.ts` — new group after De-click
- `src/eval/metrics.ts` — `mouthAttenuationDb`, `changeDbOutside`
- `src/eval/run.ts` — annotated fixture cases
- `eval/fixtures/README.md` — document the annotation sidecar
- `test/helpers.ts` — `mouthClick()`
- `package.json` — `calibrate:mouth` script
- `README.md` — stage table (line ~31), chain-order paragraph (~40), roadmap (~627)
- `docs/explainer/pipeline-explained.md` — new stage section; remove mouth-click
  removal from "Not yet attempted at all"

---

## Open decisions

Three things this spec deliberately does not settle, because they are yours:

1. **The stage name.** `demouth` is used throughout above and it reads a
   little oddly. Alternatives: `mouthclick`, `mouthnoise`. The existing names
   are all single lowercase words, so any of these fits the registry.
2. **Broadband vs HF-only ducking.** Broadband is the default here, justified
   by the lull requirement. Worth revisiting after listening, not before.
3. **Hand-tuned rule vs fitted classifier.** The two-gate rule above is
   debuggable and inspectable, which suits a small annotated corpus. If the
   confusion matrix turns out to need more than two dimensions to separate,
   a logistic regression over the same features, fitted offline with its
   coefficients frozen into the source, stays honest — there is still no
   hidden second path through the pipeline, only a different set of numbers.
   Do not reach for it first.

## What to watch for

- **Precision collapsing on a different speaker.** The context envelopes are
  all relative, which should make the detector speaker-independent — but that
  is a claim, not a measurement. Calibrate on more than one voice.
- **Word-onset clicks.** The most common and the hardest: the lull is short,
  the ramp barely fits, and getting it wrong shaves the start of a word. If
  one class of failure dominates, expect this one.
- **Interaction with `expand`.** The expander attenuates quiet passages, which
  is where these events live. Once both are on, a click already ducked here
  gets ducked again — probably fine, possibly a hole. Check a full-chain
  render, not just `demouth` alone.
