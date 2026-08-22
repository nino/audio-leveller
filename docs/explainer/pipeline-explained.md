# The Audio Leveller pipeline, explained

*A guide to the code in this repository, written for someone who knows what a de-esser does and would like to know what makes one.*

---

## How to read this

This document has three layers, and you can stop at any of them.

The first layer is the **architecture**: why the code is shaped as a chain of independent stages, and why the order of those stages is a set of decisions rather than an accident.

The second is the **evaluation harness**, which is the single most important thing in the repository and the least visible. Nothing in it processes audio. It exists to answer, with a number, the question "did that change make it better?", and, more importantly, "did that change make anything else worse?" Several of the bugs described later were found by the harness and would have been essentially invisible by ear until much later.

The third layer is the **maths of each stage**. I have written this assuming you remember that Fourier transforms and complex numbers exist but not what they do day to day, and that you are completely fluent in the language of mixing. So every time a piece of maths appears I will try to land it on something you already have a feel for: a compressor's detector, a shelf's knee, the way a de-esser's threshold interacts with the singer's actual level. Every symbol in every equation is named, and I try to say why the equation has the shape it does rather than just presenting it.

There are also two sections you should not skip even if you skip the maths. One is on a mistake that recurred three times in different parts of the codebase for the same underlying reason. It is a genuinely interesting fact about analysing speech. The other is the honest list of what has not been verified.

---

## Part 0: Five things from physics, dusted off

You need these five ideas and no more. They take about ten minutes.

### Sampling: a recording is a list of numbers

A microphone produces a voltage that varies continuously. An analogue-to-digital converter measures that voltage at a fixed rate (44,100 or 48,000 times a second), and writes down each measurement as a number. That is the whole of digital audio: `x[0], x[1], x[2], …`, one number per instant, where the gap between instants is `1/fs` seconds and `fs` is the sample rate.

In this codebase a mono channel is a `Float32Array`, an array of numbers between roughly −1 and +1, where ±1 is full scale; the loudest a converter can represent. `0 dBFS` is that ±1. When you see `−18 LUFS` or `−1 dBFS` later, those are all measured against that same ceiling.

The one piece of theory you do need: a sampled signal can only represent frequencies below half the sample rate. That half-rate is the **Nyquist frequency** — 24 kHz at 48 kHz sampling. Anything above it, if you let it in, does not simply disappear; it *folds back down* and appears as a lower frequency that was never there. That folding is aliasing, and it is why `src/dsp/resample.ts` is 150 lines of carefully-designed filter rather than "take every other sample". This is the same reason you do not simply throw away every other frame when you change a video's frame rate.

A second, subtler consequence of sampling matters later. The samples are dots. The waveform that a converter reconstructs from those dots is a smooth curve that passes *through* them. And between two dots that curve can go higher than either of them. That is why "true peak" exists as a separate concept from "sample peak", and it comes back in the limiter section.

### Spectra: any sound is a recipe of sine waves

Fourier's result is that any signal you can record can be written as a sum of sine waves of different frequencies, each with its own amplitude and its own phase (its own timing offset). The **spectrum** of a sound is that list of amplitudes and phases.

Your ear does close to literally this. The basilar membrane in the cochlea is a mechanical frequency analyser: different places along it respond to different frequencies. When you say a recording is "boxy at 250" you are reporting a reading off your own biological spectrum analyser.

The **Discrete Fourier Transform** (DFT) is the version for a finite list of samples, and the **Fast Fourier Transform** (FFT) is a clever algorithm for computing the DFT quickly. It lives in `src/dsp/fft.ts`. Feed it `N` samples, get back `N` numbers describing how much of each of `N` evenly-spaced frequencies is present. Those frequencies are called **bins**, and they are spaced `fs/N` apart. At 48 kHz with `N = 1024`, that is 46.9 Hz per bin. With `N = 8192`, it is 5.9 Hz per bin.

That trade-off is the single most consequential number in this codebase. A bigger `N` gives you finer frequency resolution but forces you to look at a longer chunk of time at once, so you lose the ability to say *when* something happened. A smaller `N` tells you when but not precisely what. It is exactly the trade-off in a spectrum analyser plugin's "FFT size" control, and exactly the trade-off between a fast, grabby compressor detector and a slow, musical one. Most of Part 4 of this document is a consequence of picking `N` wrongly.

### Complex numbers: amplitude and phase in one number

Each FFT bin comes back as a **complex number**, written `a + bi`. You do not need to philosophise about `i = √−1`. For our purposes a complex number is a convenient way of carrying two real numbers, a magnitude and a phase, in a single object that multiplies correctly.

- The **magnitude** is `|z| = √(a² + b²)`. This is how loud that frequency is. It is what a spectrum analyser draws.
- The **phase** is the angle `atan2(b, a)`. This is *when* that sine wave crosses zero — its timing.

Why carry phase at all, when your ear is famously insensitive to absolute phase? Because you need it to get back to a waveform. Change the magnitudes, keep the phases, invert the transform, and you have processed audio. Throw away the phases and you cannot reconstruct anything. In the code, spectra are stored as `Float64Array`s with real and imaginary parts interleaved: `[re₀, im₀, re₁, im₁, …]`. When you see `spectrum[2*k]` and `spectrum[2*k+1]`, that is bin `k`'s real and imaginary part.

One operation appears repeatedly: the **conjugate**, written `conj(z)` or `z*`, which flips the sign of the imaginary part: `conj(a + bi) = a − bi`. Geometrically it is a phase reversal. `z · conj(z) = |z|²`: a complex number times its own conjugate is real, and is the power. Correlations between two complex signals are always written with a conjugate on one side, for exactly this reason: you want the answer to come out meaning "how aligned are these", not to carry a spurious rotation. A misplaced conjugate is a sign error in disguise, and one of them cost real debugging time in the dereverb code — see Part 3.

### Filters: multiplying the spectrum, without computing the spectrum

An EQ band is a filter, and a filter is a device that multiplies each frequency by a different number. A high shelf multiplies everything above its corner by 1.5; a notch multiplies one narrow region by 0.1.

You could implement one by taking an FFT, multiplying the bins, and transforming back — and sometimes this codebase does exactly that. But there is a much cheaper way for simple shapes. A **biquad** filter computes each output sample from a handful of recent inputs and recent outputs:

```
y[n] = b0·x[n] + b1·x[n−1] + b2·x[n−2] − a1·y[n−1] − a2·y[n−2]
```

Here `x[n]` is the input sample at time `n`, `y[n]` is the output sample, and `b0, b1, b2, a1, a2` are five fixed numbers — the *coefficients*. That is the entire filter: five multiplies and four adds per sample, and it can be a bell, a shelf, a high-pass, anything second-order. The feedback terms (the `y[n−1]`, `y[n−2]` part) are what let two coefficients produce a resonant peak: the filter is partly listening to itself, which is what resonance is.

`src/dsp/biquad.ts` contains the standard formulas that turn "peak at 240 Hz, −4.8 dB, Q of 3" into those five numbers. Q, which you already know as "how narrow the bell is", enters those formulas as a divisor on the damping term — high Q means little damping, means the filter rings longer, means a narrower peak. Narrow in frequency and long in time are the same statement, which is the sampling trade-off again wearing a different hat.

### Decibels and why everything is logarithmic

`20·log₁₀(amplitude ratio)`, or equivalently `10·log₁₀(power ratio)`, because power goes as amplitude squared. You know this. The only thing worth flagging is that the code moves between the two constantly, and the factor of 2 is the difference between an amplitude and a power. In `dyneq.ts` you will see `10 * Math.log10(mag * mag)`: that is magnitude squared, so power, so the 10. In `declick.ts` and the limiter you will see `20 * Math.log10(amplitude)`. Both are "dB"; they just take different inputs.

That is all the background. On to the actual project.

---

## Part 1: The architecture

### What the tool is

Drag a `.wav` onto the window, or run the CLI, and the file is decoded into memory, pushed through an ordered list of **stages**, and written back out. The project started as a Levelator-style loudness tool — one thing, done well — and has been rebuilt into a local equivalent of Auphonic: de-click, denoise, dereverb, corrective EQ (with an optional fixed voicing on top), dynamic EQ, downward expansion, compression, level. All of it runs on your machine; nothing is uploaded.

### Why a pipeline of stages

A stage is defined in `src/pipeline/types.ts` and is deliberately tiny:

```ts
render(signal, params, ctx): { signal, report, extras? }
```

That is: given audio and some parameters, return new audio and a description of what you did. Stages are **pure**. The same input and the same parameters always produce the same output, with no hidden state carried between calls.

Purity buys more than tidiness. Every stage having the same shape means:

- **Any stage can be bypassed** without special-casing anything. `--bypass eq` builds the same chain with one entry marked disabled. The bypassed stage still appears in the report, so you can see what you turned off.
- **Every stage reports.** The JSON report from a run contains, for each stage, its fully resolved parameters, how long it took, and its own account of what it decided. Not "eq ran" but "240 Hz, −4.8 dB, Q 3.1; rumble high-pass at 80 Hz; worst deviation before 9.1 dB, after 2.2 dB". This is the difference between a black box and an instrument.
- **A stage that finds nothing to do can return the input object itself**, and several do exactly that. That is stronger than "returns something very similar"; it is bit-identical, and the harness asserts on it.
- **New stages cost nothing structurally.** Implement the interface, add the name to `BUILT_IN` in `src/stages/index.ts` at the right point in the chain, and it appears in `--list-stages`, in `--bypass`, in the report, in the UI's chain inspector, and in the evaluation harness's null test — automatically.

The chain inspector in the app (phase 7) is where this pays for itself. Each stage shows a line of plain English about what it actually did — "no clicks found", "already clean — nothing removed", "dry (35 ms decay) — left alone", "240 Hz −4.8 dB" — with a checkbox that bypasses it and re-renders. As the commit puts it: *"declick 42 ms" answers a question nobody asked; whether it found anything is the question.*

### The analysis pass, and why measurement follows the audio

Nearly every stage needs to measure something before it can decide anything. The de-esser needs to know where the pauses are. The EQ needs the average spectrum of the speech, and separately of the pauses. The leveller needs gated loudness per segment.

Some of these measurements are expensive. K-weighting a long file — running two biquads over every sample of every channel, which is what BS.1770 loudness requires before you can measure anything — is the dominant cost of any loudness question. Several stages want overlapping answers.

So `src/pipeline/analysis.ts` defines an `Analyzer`: an object bound to one specific signal that computes each measurement at most once and caches it. Ask it twice for the integrated loudness and the second answer is free.

The important part is the second half of the rule. Analyzers are deliberately **not** shared between stages. Each stage is handed a fresh `Analyzer` over the signal *as it arrives at that stage*, not over the original file.

The reason is straightforward once stated and easy to get wrong in practice: a stage that changes the audio invalidates every measurement taken before it. If the denoiser has just pulled 12 dB out of the noise floor, then the noise-floor measurement the EQ uses to decide whether it is safe to boost a band is now wrong — wrong in the direction that makes the EQ too cautious. If the EQ has just cut 6 dB at 240 Hz, the loudness the leveller measures has changed. Measuring once at the top of the chain and passing that around would be a category of bug that produces plausible-looking output and is very hard to hear.

There is one carefully-drawn exception. `ctx.source` gives a stage the untouched decoded signal, and `ctx.sourceAnalysis` its measurements. Exactly one thing needs it: **room tone harvesting**. The leveller builds a room-tone bed out of the quietest, steadiest fragments of the detected silences, and if the denoiser has already run, there is no room tone left to harvest. The room tone has to come from the original recording even though it is emitted by the last stage in the chain.

### Why the stage order is what it is

The default chain, in `src/stages/index.ts`:

```
declick → denoise → dereverb → eq → dyneq → expand → compress → level
```

Each arrow is a decision.

**De-click first.** Two reasons, and the second was a surprise. The obvious one: an impulsive click is out-of-distribution for any denoiser, classical or trained. A trained model has never seen one in its training data and will smear it into a chirp rather than remove it; a spectral suppressor sees a broadband burst across every bin at once and cannot tell it from a plosive. Remove the clicks before anything else has to reason about the spectrum.

The second reason came out of the evaluation harness in phase 1, and is the sort of thing you would never guess. **Clicks were breaking the leveller's segmentation.** The leveller finds pauses by sliding a 100 ms window across the file and measuring its loudness; windows below a threshold are silence. A single click landing in a pause lifts that window's loudness above the threshold. The pause stops being detected. Two speech segments recorded at different levels get merged into one and gained as one. And the levelling error on the harness's `clicky` case measured about **8 LU**. That is not subtle. With de-click ahead of the leveller the same case measures **0.32 LU**.

**Denoise before EQ.** The corrective EQ fits a curve to the long-term average spectrum of the speech. If the denoiser is going to run and change that spectrum, fitting the curve first means fitting to a spectrum that will not exist by the time the filters are applied.

**Dereverb after denoise, before EQ.** WPE's statistics are cleaner without a noise floor under them: it is estimating correlations, and uncorrelated noise biases those estimates. And removing a room changes the spectrum, for the same reason as above.

**Dynamic EQ after static EQ.** The static EQ has already removed whatever colouration is constant, so what reaches the dynamic stage is genuinely the part that comes and goes: ringing vowels, sibilance, a boomy plosive. Running dynamic EQ first would have it chasing a permanent room mode frame by frame, which is exactly the job a static filter does better and more transparently.

**The level-domain stages after all the tone-shaping.** The compressor should be compressing the corrected spectrum, not a room resonance the EQ is about to remove — otherwise its detector spends the file reacting to something that will not survive to the output. Same argument as denoise-before-EQ, pointed the other way round.

**The expander before the compressor**, and this one is the least obvious. Be careful with it, because the tempting explanation is wrong. It is tempting to say that a soft-knee compressor lifts the floor, so the expander would spend the file undoing that. It does not: `compressorCurve` returns `-min(reduction, cap)` with `reduction` never negative, so the stage only ever attenuates, and below the knee it applies exactly unity. There is no upward gain anywhere in it.

The real argument is about *what each stage measures*. Both of them set their thresholds relative to the programme loudness of the audio handed to them, and the pipeline hands each stage a fresh analysis of the signal as it arrives, not of the original file. The expander goes further: it also decides *whether to run at all* from the programme-to-floor distance it measures, passing the signal through bit-identical if the floor already sits more than 50 dB down.

Now consider each order. Expanding first only touches material about 27 dB below the programme — far under the relative gate that the loudness measurement applies, which is to say the expander moves audio the measurement is not looking at. So the compressor downstream sees the same programme loudness, and picks the same threshold, that it would have picked on its own. The order costs it nothing.

Compressing first is not symmetric. The compressor pulls the loud parts down and leaves the floor exactly where it was, because the floor never reaches its threshold. That lowers the programme while the floor stays put, which narrows the programme-to-floor distance — the very number the expander reads before choosing a threshold, and the number its skip decision turns on. Compress first and you can change whether the expander runs.

So the order is not about the two stages fighting over the floor. It is that one order leaves both measurements intact and the other perturbs one of them.

**Level last, always.** So the loudness target is measured on the audio that actually gets written to disk. Any spectral processing after levelling would move the loudness off target, and `−18 LUFS` would become approximately −18 LUFS. Putting the compressor before it has a second benefit: the true-peak limiter at the end of the level stage sees what the compressor actually produced, rather than guessing at it.

You would apply the same reasoning to a mix chain, where nobody puts the loudness maximiser before the EQ. I have written it down because in code the order is a single array literal, and nothing stops someone reordering it.

### Whole files in memory, and what that costs

There is one more architectural commitment, and it is the sort of thing that looks like an implementation detail until it isn't: **a stage is handed the entire recording at once.** Not a stream of blocks with state carried between them. The whole signal, as an array, with a stage free to look anywhere in it.

That buys real things, and some of them are not available any other way:

- **Free lookahead.** The true-peak limiter eases its gain down *before* each peak by running a backward pass over the gain curve. In a streaming design that is a delay line and a pile of latency bookkeeping; here it is a loop that counts down instead of up.
- **Two-pass measurement.** Every stage that measures the file before deciding what to do — which is nearly all of them — needs the file first. The EQ fits a curve to the whole recording's average spectrum. The leveller needs every segment's loudness before it can choose any segment's gain.
- **Algorithms that genuinely need it.** The dereverberator estimates one room from the entire recording, which means it needs every frame of a frequency bin simultaneously. That is what the method is.

What it costs is that **memory scales with the length of the recording**, and that cost stayed invisible for a long time because everything the project was tested on was short. The eval corpus is seconds long. The unit fixtures are shorter. The first genuinely long recording put through the chain (twenty-one minutes) found the wall immediately, and then a second wall behind it.

The first was a crash. A blind reverb-decay measurement built an envelope with one entry per 5 ms of audio and then took its maximum with `Math.max(...envelope)`. Spreading an array into a call passes each element as a separate argument, and V8 stops at somewhere around 65,000 of them. A 1286-second recording produces 257,200. Every test in the suite passed; the first real recording died. I found three more places with the same shape and fixed them the same way.

The second was slower and worse. The short-time Fourier transform materialised every frame and handed back the lot: at the usual frame settings that is 8 KB per frame and 241,219 frames — about 2 GB for the frame array alone, per channel, before the inverse transform's accumulators and the signal copies. The render sat at 4.5 GB and ran at roughly 1.5× real time, against about 40× on a one-minute clip. It was swapping.

The fix keeps the whole-file commitment for the *signal* and drops it for the *spectrogram*. `processStft` does the same work through a fixed-size overlap-add buffer: once the frame starting at a given sample has been added, that sample can receive no further contributions, so exactly one hop's worth of slots retire per frame and are immediately reused. Memory becomes O(frame size) regardless of file length. The dynamic EQ and the denoiser were both holding a full spectrogram for no reason, both are per-frame with carried state, and both were converted.

Two details separate a refactor from a rewrite here. The output is **bit-identical**, and that is asserted rather than hoped for: the tests compare the streaming path against the old materialising path at three frame sizes, with and without a modification applied. It can be bit-identical because frames still arrive in increasing order, so each output sample accumulates its contributions in exactly the same sequence as before. And the one place where behaviour did change is stated plainly rather than buried; a fallback noise estimator that wants the quietest fifth of frames per bin, which needs a value per bin per frame, now subsamples above 20,000 frames. A noise floor is a stationary property, so a few thousand frames spread across a recording estimate it as well as all of them; below the cap nothing changes at all, which covers the entire corpus.

The dereverberator is the exception that proves the rule. It still materialises its whole spectrogram — about 2 GB on a file that long — because it genuinely needs every frame of a bin at once. Bounding *that* means processing in blocks, which is a behaviour change rather than a refactor: the room estimate would start adapting over the file instead of being fitted once. That is a real design question, so it is recorded as a limitation rather than quietly changed.

### Lazy sample-rate conversion

The obvious design is to convert everything to a canonical 48 kHz at the top, run the whole chain there, and convert back at the end. It is simple and it is wrong.

Sample-rate conversion is not free. Even a very good resampler — and `src/dsp/resample.ts` is a proper Kaiser-windowed sinc design, over 90 dB round-trip SNR — is a filter, and running two of them over material that did not need it is pure loss. It is the digital equivalent of bouncing through an extra generation of tape for no reason.

So a stage declares `requiredSampleRate` only if it genuinely needs one, and as it stands none of them do. The leveller does not, because its K-weighting coefficients are derived analytically for whatever rate the file happens to be (`src/dsp/kweighting.ts` computes them from the filter's design frequencies rather than hard-coding the 48 kHz numbers printed in the standard). De-click does not, because the AR model order is chosen per sample rate. The one component with a genuine fixed rate is the DeepFilterNet model backend, such models are trained at 48 kHz and nothing else may be fed to them, and rather than force the whole chain to 48 kHz for its sake, I have the backend report itself unavailable at any other rate and let the classical suppressor take over. Two sample-rate conversions to reach a denoiser is not obviously better than the denoiser that needs neither, and that is not a trade the corpus can settle, so the code declines to guess and says which backend it used.

The runner in `src/pipeline/pipeline.ts` collects the requirements of the *enabled* stages only. If nobody asks, no conversion happens, and a 44.1 kHz file goes through untouched. If someone asks, conversion happens exactly once on the way in and once on the way out. If two stages disagree, the runner throws rather than silently resampling between them — no stage needs that yet, and guessing would hide the cost.

What it buys is a guarantee that can be tested: **a fully bypassed chain is byte-identical to its input.** The harness's `bypass-null` case asserts exactly that, and it is a surprisingly sharp test: it catches anything that quietly touches the signal.

That guarantee, incidentally, immediately exposed a pre-existing bug. See the next section.

---

## Part 2: The evaluation harness, and why it was built before anything else

I spent phase 1 — before de-click, before EQ, before any of the processing that actually makes the tool useful — building something that does not process audio at all.

### The problem it solves

Suppose you write a de-esser. How do you know it works?

The naive answer is: listen to it. And you must listen to it — nothing here replaces that. But listening has three failure modes that matter enormously when you are building a chain of eight stages.

First, **you cannot hear small regressions.** If a change to the EQ makes the de-clicker 0.4 dB worse at repairing clicks in loud passages, you will not hear it. Three months and four stages later, you will hear the accumulation, and you will have no idea which change caused it.

Second, **you cannot hear what you are not listening for.** You sit down to evaluate the denoiser, you listen to the noise, the noise is gone, you are pleased. You do not notice that the segment levels drifted by 1.5 LU because the denoiser changed the loudness measurement the leveller depends on.

Third, **you cannot A/B honestly against yourself.** You know which render is the new one. Everybody's new render sounds better.

So: `pnpm eval` renders a corpus of test material through the pipeline, measures the result with about thirty objective metrics, and checks each metric against bounds the test case states in advance. It exits non-zero when a bound breaks. It is both a tuning instrument and a regression gate. The corpus is 26 cases as this is written, up from 18 when the chain was six stages — most of the growth is the new stages each bringing a case where they should act and a case where they should decline.

Two of those 26 need the DeepFilterNet weights on disk, which a fresh checkout does not have. They are reported as **skipped**, by name, with the reason — not silently dropped. The alternative is worse than it looks: a suite that quietly omits the checks it cannot run prints a clean sweep of ticks and tells you nothing about the two things you most wanted to know. A check nobody ran must not look like one that passed. On a machine without weights the run ends `24/24 cases passed … 2 skipped`, and the two skipped lines are printed in dim text above it.

### Synthetic speech that can be broken on purpose

The corpus is in `src/eval/signals.ts` and it is *synthetic* — not recordings. That is a deliberate and slightly counter-intuitive choice, and the reason is in the commit message: **everything is seeded, so a metric that changes always means the code changed.**

There is no `Math.random` anywhere in it. A small deterministic pseudo-random generator (mulberry32, seeded with a fixed number) drives every noise sample, every click position, every jitter in the pitch contour. A run today and a run next month produce bit-identical material and therefore bit-identical numbers. When a metric moves, you know it was you.

The second reason is that you can degrade synthetic material *in known ways*. The corpus can add noise at an exact requested SNR, scatter exactly 40 clicks at known sample positions, convolve with a synthetic room impulse response of known RT60, add a resonance at exactly 2.5 kHz, or drift the segment levels by exactly 25 LU. And crucially it retains the **clean reference** (the same material without the degradation), so metrics can ask "how far is the output from what it should have been" rather than merely "does the output look plausible".

Real recordings are still supported: any `.wav` dropped into `eval/fixtures/` is picked up as an extra case automatically. A fixture on its own cannot carry a reference, so it gets the subset of metrics that do not need one, but there is a trick that recovers the rest. Take the first sixty seconds of the recording, add broadband noise at a known SNR, and run the denoiser on *that*: now the recording itself is the clean reference, and every reference-based metric is available on real speech. Those degraded-fixture cases are where the model backend's quality is actually bounded, for reasons that will become clear in the denoise section.

That trick has since been pointed at a second stage, and for a reason that turns out to be the most important thing in this document. The de-clicker now gets two fixture cases of its own: the excerpt with forty clicks injected at half its peak (did they get repaired, and was anything *else* touched), and the excerpt completely untouched (the stage should do almost nothing). Both exist because the synthetic voice turned out to be unfit for judging this particular stage — not slightly unfit, but wrong in the direction that made the stage look good while it was quietly damaging every real recording it saw. That story is in "The bugs the harness missed", below, and again in Part 4.

One more piece of determinism, added late and easy to miss. Every case now **pins the denoise backend by name** rather than taking whatever the machine can run. Without that, installing model weights in your home directory would silently change numbers on cases that have nothing to do with the model, and a colleague without the weights would see a different corpus from yours. The corpus must not depend on what happens to be sitting in `~/.audio-leveller`. Same instinct as seeding the noise: a number that changes must mean the code changed.

The synthetic generator went through a significant rewrite in phase 3, which shows how the harness improves under pressure. The original generator summed 16 harmonics of a fundamental. Perfectly reasonable-sounding approach; completely unfit for spectral work. Sixteen harmonics of a 110 Hz voice ends at 1.8 kHz — above that, the "speech" was digital silence dressed up as signal, so any stage being judged on what it did above 1.8 kHz was being judged against fiction. It also held pitch nearly constant, so the long-term average spectrum resolved a razor-sharp harmonic comb that no real voice produces. And it used one fixed vowel, leaving a 13 dB valley between the first and second formants — deep enough that a resonance deliberately injected into that valley read as *filling a hole* rather than as a peak, and any stage evaluated against it learned exactly the wrong lesson.

The replacement does proper **source-filter synthesis**, which is how speech actually works: a glottal pulse train (a Rosenberg pulse per pitch period, which has a smooth shape and therefore a −12 dB/octave spectral slope like real glottal flow) passed through peaking filters at the formant frequencies, with a first-difference at the end to model lip radiation (+6 dB/octave, turning the source's −12 into the ≈−6 dB/octave tilt that measured speech actually shows). It moves between five vowels with crossfades, and it has prosody: pitch declination across the utterance, intonation, and cycle-to-cycle jitter. I tried cascaded two-pole resonators first, the textbook formant model, and threw them out: four in series roll off at −48 dB/octave past the top formant, which puts you back at a spectral cliff.

None of this makes it sound like speech. I only need it to *behave* like speech for the measurements the pipeline takes.

### Metrics that can say a stage made it worse

The design rule, stated at the top of `src/eval/metrics.ts`: *every metric must be able to say a stage made things worse, not just better.* A harness that only measures improvement will happily report that a denoiser which eats consonants is working perfectly.

The metrics that matter:

| metric | what it catches |
| --- | --- |
| `segmentLufsError` | did every speech segment land on target |
| `outputPeakDbfs` / `outputTruePeakDbfs` | did the limiter ceiling hold, including between samples |
| `segmentSnrGainDb` | did a stage move speech and noise apart within a segment |
| `snrGainDb` | the whole-file version: what levelling *costs* in noise |
| `siSdrGainDb` | did the chain damage the signal relative to a clean reference |
| `outputClickResidualDb` | how audible the worst surviving click is |
| `spectralFlatteningDb` | did the EQ actually reduce colouration |
| `decayShorteningMs` | did the dereverb shorten the room's tail |
| `floorReductionDb` | did the expander push the floor down between words |
| `loudnessRangeReductionLu` | did the compressor actually narrow the programme's spread |
| `compressorMaxReductionDb` | how deep the compressor went, against its own cap |
| `programmeLossDb` | how much of the *voice* a denoise backend cost |
| `changeDb` | did a stage touch the audio at all (`−inf` means bit-identical) |

The last four of those are new with the last round of work, and three of them are a different *kind* of metric: they are lifted out of the stage's own report rather than measured from the output audio. That is a deliberate widening of what the harness is allowed to assert on. A stage's account of what it did belongs in the corpus alongside the measurements taken from the audio — the denoiser measuring that it cost 10 dB of programme loudness is the same finding as the audio being 10 dB quieter, caught one layer earlier and stated in the vocabulary of the thing that did it. It also means a bound can name a specific stage's behaviour on a full-chain case, where an audio-only measurement would be the sum of everything.

Two of the original metrics need real explanation. Getting either wrong produces a harness that lies to you confidently.

**SI-SDR** — scale-invariant signal-to-distortion ratio — is the general "how damaged is this" number. You take the clean reference `s` and the processed estimate `x`, find the scalar `α` that best fits `x` as a scaled copy of `s`, and then report the ratio of the energy in `α·s` to the energy in the leftover `x − α·s`, in dB:

```
α = ⟨x, s⟩ / ⟨s, s⟩
SI-SDR = 10·log₁₀( ‖α·s‖² / ‖x − α·s‖² )
```

`⟨a, b⟩` is the dot product — multiply the two signals sample by sample and sum. `‖·‖²` is energy, the sum of squares. The scale-invariance (dividing out `α`) is the point: this pipeline changes overall gain on purpose, and a metric that scored a level change as damage would be useless. High SI-SDR means "the output is a scaled copy of the reference plus very little else". 20 dB is good; 1 dB means the signal has been destroyed.

The subtle decision is *which* reference. **SI-SDR is scored against the clean reference put through the same chain**, not against the raw clean reference. The leveller changes segment levels deliberately; if you scored against the raw reference, that intended change would count as damage and the number would be meaningless. Comparing processed-degraded against processed-clean isolates what the *degradation* did from what the *chain was asked to do*.

**`segmentSnrGainDb` versus `snrGainDb`** is the other one, and it is genuinely counter-intuitive. Signal-to-noise gain is the natural transparency check: nothing except a denoiser should change the distance between the programme and the noise floor. But measured across the whole file, that is false. Levelling pulls quiet segments up, and boosting a quiet passage boosts its noise floor with it, so the whole-file programme-to-floor distance genuinely shrinks. Bounding it would be asserting something untrue about what levelling does.

Measured *within a single segment*, where one gain applies to speech and its own noise together, SNR is invariant to levelling. So `segmentSnrGainDb` — each speech segment against the noise in the pause leading into it, median across segments — is the number worth asserting on, and `snrGainDb` is kept as a diagnostic. It is the honest measure of what levelling costs in noise, and it is the reason the denoiser belongs *before* the leveller in the chain.

Even `segmentSnrDb` needed care in the implementation. The noise sample has to be taken from a short window at the very *end* of the pause, not the middle. The leveller ramps its gain linearly across each silence, only reaching the following segment's gain at the end of it, so sampling the noise earlier measures it at a gain the following speech never receives, which reads as a phantom SNR change proportional to the difference between adjacent segments' gains. The `level-drift` case, where adjacent segments differ by ~18 dB, has a relaxed bound with that exact explanation written into it.

#### And then a new stage made that bound untrue

This is the honest version of what usually happens when a test fails after a feature lands. For most of this project's life I had a shared expectation called `SNR_PRESERVED`, applied to every clean-ish full-chain case, asserting that segment-local SNR moves by no more than ±1 dB. Its justification was exactly the argument above: levelling applies one gain per segment, which moves that segment's speech and its own noise together, so the distance between them cannot change. It also quietly did a second job — if the denoiser ever stopped backing off on clean sources, this is where it would show.

The downward expander makes the first argument false, and not by accident. An expander deliberately attenuates the quiet parts; the quiet parts *inside* a segment are the gaps between the words; so a segment's speech and the noise in its own gaps no longer move together. That is the mechanism, not a side effect. Measured across the default-chain cases it moves segment-local SNR by 10 to 13 dB, on floors sitting 37 to 40 dB below the programme.

I had two options and only one of them was legitimate. The illegitimate one is to relax the bound until it passes, which is the failure mode described above and the reason every bound carries a `because`. The legitimate one is to notice that the bound's *reason* had expired and write a new bound with a new reason. So I replaced `SNR_PRESERVED` with `SNR_EXPANDED`, `min: 6, max: 14`, and its `because` is now a statement about the expander: it must engage on a floor this close to the programme, and it must stay inside its own range cap. The upper edge is set by `rangeDb`, the expander's 12 dB attenuation limit, plus the little the leveller contributes. So if the cap ever stopped holding, the bound would still fail. It is a real assertion about a real mechanism, not a snapshot of the number that came out.

And the second job the old bound was doing was not dropped, because that would have been a silent loss of coverage. Checking that the denoiser backs off on clean sources now lives on two cases of its own, `clean-denoise` and `clean-denoise-onnx`, and it is asserted as **bit-identity through the denoise stage alone** — `changeDb` of `−inf`. That is strictly stronger than the SNR window it replaced: an SNR window says "the denoiser did not change the balance much", where bit-identity says "the denoiser did not run".

### Bounds with reasons, not snapshots

Every expectation in `src/eval/cases.ts` carries a `because` field, and the runner prints it when the bound fails. This is not documentation politeness. It is the mechanism that stops the harness rotting.

The failure mode of a test suite that snapshots current behaviour is well known: a bound fails, you look at it, you have no idea why it was set to 1.5, the new number is 1.7 and looks fine, you relax the bound. Repeat forty times and the suite asserts nothing. As the file's header puts it: *a bound you can't justify in a sentence is a bound that will be quietly relaxed the first time it fails.*

So instead the bounds read like this. The click-audibility bound on the `clicky-stage` case:

> *no repair may poke meaningfully above the peaks already present around it (±10 ms) — that
> is what makes a click audible. The bound sits just above the theoretical floor: at a pause
> site the residual of even a perfect repair is the unknowable noise realisation that was
> under the click, which lands at ~0 dB on this peak-vs-local-peak measure. Repairs currently
> reach −1.2 dB; a missed click reads +40.*

Note what that does. It states the physical floor, states where the current implementation sits relative to it, and states what failure looks like on the same scale. A future person looking at a broken bound has everything they need.

Note also that some bounds have **upper** limits on improvement, not just lower ones. The denoiser's SNR expectation is `min: 6, max: 14` — because over-delivering means it is reaching past what the source supports, which is exactly where artefacts come from. A denoiser that reports 25 dB of reduction on a 20 dB-SNR source is eating the signal.

One recent bound was wrong on its first outing, and the mistake is instructive rather than careless. I wrote the expander's own case, `floor-expand`, by copying the levelling rationale, so it asserted that segment-local SNR must not move. Which, for a stage whose entire purpose is to move segment-local SNR, would have been asserting that the expander does not work. Written down like that it is obvious; written as one line copied from the case above it, it looked like diligence. The replacement gives the case a genuine clean reference: the same programme generated from the same seed with the floor pushed to −100 dBFS, so the speech is sample-identical and the only difference is the noise. SI-SDR against that reference can then answer the question that actually matters, did it remove noise or did it remove words, and it reads +1.20 dB, so: noise.

### The other half: blind listening

Everything above is the argument for measuring. The counter-argument — the one the top of this part conceded and then walked past — is that metrics only ever answer the question you thought to ask. A number can tell you the chain did what it was told. It cannot tell you the result sounds right. "Sounds right" is a property of a listener, and a listener is not something the harness has.

So I built a second instrument, deliberately designed to defeat the person using it. `listen/` is a small blind A/B app: a local web page, nothing uploaded, no account. The workflow is:

1. **Render candidates.** Whatever you want to compare: the CLI with different flags, a commercial service's export, a DAW bounce, the same file with one stage bypassed.
2. **Write a spec** naming the variants, some time windows, and which variants appear in each trial.
3. **Build a session.** The builder cuts every window from every variant, **loudness-matches** them, **shuffles** them, and writes them under names that leak nothing — `t03_B.wav`. The mapping from `B` back to a variant goes into a `key.json` the app refuses to show you until you ask.
4. **Listen.** One trial per screen. A/B/C/D switch clips *gaplessly at the same playback position*, which is the only way to hear a small difference — stop, reload, and press play again and you are comparing your memory of one clip against another. Score each clip, tag artefacts, pick a winner.
5. **Reveal.** Per-variant means, picks, tag counts, CSV export.

Two details in there are doing more work than they look.

**The loudness match is on the loudest 400 ms, not the integrated loudness.** This was originally integrated, which is the obvious choice and is wrong. Two clips with different dynamics can share an integrated value and still sound plainly unequal, because the ear weighs the loud syllables far more than the quiet ones. Measured on an actual session, the unlimited variant's loud syllables sat about 2.5 dB above the others and its peaks 7 dB above, at matched integrated loudness. A clip that is even slightly louder reliably wins a preference test, so an unmatched comparison is not measuring what you think it is measuring — it is measuring the level error. This is the same fact every mix engineer knows about A/B'ing with the bypass button, formalised.

**Rebuilding a session keeps its shuffle.** Add a variant, rebuild, and the trials you have already scored stay valid rather than silently becoming answers to different questions.

The reason this sits in the middle of a part about *measurement* is that the harness and the listening app fail in opposite directions, and you need both. The corpus is exhaustive, repeatable, and only sees what someone wrote a metric for. The ear sees everything and remembers nothing, cannot be repeated, and lies to you the moment it knows which render is yours. Nothing in the next two sections would have been found by either instrument alone.

### The bugs the harness caught

All of these are in the commit messages.

**The WAV round-trip lost one LSB per sample.** The byte-identical bypass guarantee exposed this immediately. The decoder converted 16-bit integers to floats by dividing by 32768; the encoder converted back by multiplying by 32767. Those are different numbers. Every decode-encode round trip therefore shifted every sample very slightly toward zero. A vanishingly small error, inaudible on one pass, but present on *every sample of every file* and compounding across renders. It exists because signed PCM is asymmetric: the range is −32768 to +32767, so 32767 looks like the right scale for the write side. It is not; the fix is to scale both sides by 32768 and clamp, accepting that only true full scale gets clipped, which is inherent to the format. Nobody would have found this by listening. The bit-identity assertion found it on day one.

**The de-clicker fired on glottal pulses.** When the synthetic speech generator was rewritten to be more realistic (phase 3), the de-clicker suddenly started detecting clicks in clean speech at about two per second. The generator had not got worse; it had got *right*. Voiced speech is driven by a glottal pulse every pitch period: impulsive excitation, five to fifteen milliseconds apart. That is precisely what an impulsive-outlier detector is designed to fire on. The old generator's smooth harmonic sum had no such pulses, so the bug had been invisible. Shortening the analysis blocks, so the robust threshold could rise over a run of pulses, brought the synthetic case back to nearly nothing, and I wrote the `clean-declick` case to guard it.

That fix was wrong, and the corpus certified it for months. See below.

**The dynamic EQ attenuated 92% of clean speech.** I had set the threshold at a sensible-sounding 6 dB above the local spectral envelope. Measured on clean material: 92% of all time-frequency cells were being attenuated. At 92% it is reshaping the entire recording rather than suppressing anything, and it would have sounded like a slightly dull, slightly squashed version of the voice, which is very easy to rationalise as "smoother". Raising the threshold to 12 dB brought it to 5% of cells while delivering the *same* benefit on the actual resonance case. All the extra activity was cost with no benefit. Why the threshold has to be so much higher than intuition suggests is explained in Part 3.

**WPE destroyed dry speech.** At the pipeline's usual STFT frame size, running the dereverberator over a *dry* recording brought it back at 1 dB SI-SDR, which, on the scale above, means annihilated. The `dry-dereverb` case exists to catch exactly this, and it asserts bit-identity rather than approximation.

**A conjugation sign error in the WPE normal equations.** The filter was solving for the wrong thing and subtracting more than the room had added. This one is invisible from the outside (the output is still audio, still recognisably speech), and was found by the reverberant case failing to improve when it obviously should have.

**Denoising clean audio made it measurably worse.** Running the full 12 dB of reduction over material whose noise floor already sat 35 dB down *lowered* SI-SDR against the clean reference. There was no noise worth removing, so the processing was the only thing that changed anything. This led directly to the adaptive backoff described in Part 3, and, nicely, turned the existing "levelling must not change SNR" bounds into a test of the backoff itself, which is a stronger check than the one it replaced.

**The click-audibility metric was wrong twice.** Not the code under test: the *metric*. The first version normalised the residual at each click site by the whole file's RMS, which punished good repairs in loud speech (global RMS is dragged down by all the pauses) and forgave bad ones in silence. The second normalised by local RMS, which still had a false floor: AR interpolation necessarily replaces the *unpredictable* part of a signal with a different realisation, so in a pause the residual is the room's own noise, and a peak measured against an RMS sits crest-factor above it even when the repair is perfect. The third version compares residual **peak** against local reference **peak** within ±10 ms — like for like — because a spike is a click precisely when it pokes above what is already there.

Follow that progression. Two-thirds of the work on that metric was discovering what "a good repair" even means, and the answer turned out to have a hard physical floor: at a pause site, the residual of a *perfect* repair is the noise realisation that was under the click, which nothing can reconstruct. So the bound sits at +2 dB rather than a fantasy −40, and the harness is honest about what is achievable rather than aspirational about it.

**The harness was enrolling its own output as test material.** This one is entirely the harness's fault. Real recordings are picked up automatically from `eval/fixtures/`, which is the whole point of that directory. But the pipeline writes its results *next to its input*, so processing a fixture in place leaves `<name>_processed.wav` and `<name>_roomtone.wav` sitting in the fixtures directory. And the next run scooped them up as fixtures in their own right. The chain was being graded on audio it had produced itself. That is a feedback loop with a corpus's job title. Worse, one of those files is a room-tone bed: pure noise floor, no speech, and therefore something no amount of levelling can bring to the loudness target, so it contributed a permanent failure that had nothing to do with anything. The fix is four characters of regular expression, `/_(processed|roomtone)$/`, and the interesting part is not the fix but that the harness's own conveniences can be the thing that lies to you.

### The bugs the harness missed

The section above is the sales pitch. This one is the correction to it, and it is more useful.

Every bug listed above was found by the corpus. The corpus is synthetic, seconds long, and generated by a program. So it can only find bugs that survive being synthetic, and only bugs that appear within a few seconds. When a real twenty-one-minute recording was finally put through the chain and listened to blind, four things came out at once, and none of them had ever registered as anything but green.

**The de-clicker was destroying real speech, and the corpus said it was fine.** This is the serious one. On the real recording, the de-clicker "repaired" **21,591 clicks** — about nineteen a second, spaced at the voice's pitch period. It was the glottal-pulse bug from the section above, never actually fixed: shortening the analysis blocks had been enough to satisfy the synthetic voice and nothing more. In blind listening this turned out to be the source of the distortion in the chain. I had blamed the limiter.

Why the corpus missed it is the opposite of the obvious guess. The synthetic voice is not *less* impulsive than a real one — it is **several times more** impulsive. Its glottal source is a Rosenberg pulse, which stops dead at closure, where a real glottis has a return phase. So on synthetic material the voice's own excitation towered over everything, the threshold rose to clear it, and clean speech passed. On a real voice, whose pulses are gentler, the same threshold sat neatly *between* the pulses and flagged every one of them.

I had built a corpus that is not merely unrepresentative but **anti-representative**: it was harder than reality in exactly the dimension being tested, so passing it was evidence of nothing. And it had a second consequence that took a while to see. The synthetic click cases injected clicks at 0.9× the waveform peak, which sounds aggressive, and on a real voice would be. On *this* voice a 0.9× click barely stands out from the excitation — so once a correct fix landed, those cases started failing, and they were failing for the right reason. They now inject at 2× with a stated explanation, and the real sensitivity check moved to the fixture cases, on real speech.

Part 3 has the fix; the shape of it belongs here. what separates a click from a glottal pulse is not its size against the noise floor (both are large), but its size against *its own neighbours one pitch period away*. A pulse has company; a click stands alone. On the real recording that took 21,591 repairs down to 433, and on the eval fixture 749 down to 16, while injected clicks at half the waveform peak are still repaired 30 out of 30.

**The limiter's instant attack was audible.** With the gain stepping down in a single sample, the waveform gets a sharp corner in it, which is, fairly literally, what clipping is. On the corpus the limiter barely engaged and the corner never appeared. On real material at 8.7 dB of reduction it was obvious. The fix is the backward-pass lookahead ramp described in Part 1.

**The chain fell over on length.** The stack overflow and the 4.5 GB spectrogram, both in Part 1. A corpus of multi-second fixtures cannot find a bug whose trigger is 65,000 array elements or 2 GB of frames, and no amount of adding cases to it would have helped. The bugs are not in the space the corpus samples.

**The room-tone bed admits breaths.** Clips for the room-tone bed are scored for loudness with penalties for clicks and swells, and a breath defeats all three at once: only moderately louder than the floor, broadband rather than impulsive, and long enough to read as a steady level. It needs a term for spectral shape, since a breath is noise but tilted differently from room tone. The synthetic corpus has no breaths in it, so this could not have been caught, and it is now an open issue with labelled real material being collected against it.

My honest summary: the harness catches regressions superbly, and validates novel behaviour only as far as its material is real. Both instruments were needed, in a specific order: the corpus kept the chain from rotting while it was built, and the ear found the things the corpus was structurally incapable of seeing. A metric can only be as good as the material you point it at, and *synthetic material is a model of speech, with all a model's assumptions baked in and invisible.*

### A test that passed for the wrong reason

One more, in a different key, because it is about the unit tests rather than the corpus and it is a clean example of a category worth being paranoid about.

The model backend verifies its weights by SHA-256, and because a DeepFilterNet export is three graphs plus a config rather than one file, it verifies *every* file; a model is the whole set or nothing, since an encoder that matches paired with a decoder that does not is not a partially usable model, it is an unknown one. There is a test for exactly that: break the pin on the *third* file only, and check that verification fails naming that third file.

It passed. It was also meaningless. `verifyModel` returns the *first* failure it finds, and on any machine without the weights installed the first failure is `enc.onnx: not found`. So the assertion never reached the thing it was testing. The test passed on precisely one machine in the world, the one that happened to have run `pnpm fetch-model`, and it went red the moment CI ran it.

I made the test hermetic instead: build stand-in files in a temporary directory, pin them to the hashes they actually have, and, this is the load-bearing part, **assert that the intact set verifies before breaking one pin.** Without that first assertion, "fails naming `df_dec.onnx`" could still be true for a reason having nothing to do with the third file being checked. A test that only ever asserts a failure cannot tell you whether the failure is the one you meant.

The general lesson is the same one the whole harness is built on, applied to itself. A green tick is evidence only if you know what would have made it red.

### Using it

```bash
pnpm eval                                # run the corpus, non-zero exit on a broken bound
pnpm eval --verbose --case noisy         # every metric for one case
pnpm eval --baseline eval/baseline.json  # what moved since the baseline
pnpm eval --wav /tmp/out                 # dump each case's input and output to listen to
```

The workflow it exists for: run it, note the numbers, build a stage, run it again with `--baseline` and see exactly what moved. As the runner's header says: *"sounds better" is not evidence; a click residual that fell 45 dB while SI-SDR held steady is.*

And `--wav` matters. The harness does not replace listening; it tells you where to point your ears.

---

## Part 3: The stages, in order

### Stage 1 — De-click

#### Three samples that are simply wrong

Digital dropouts, vinyl ticks, a bad cable connection. Three or four samples that are simply wrong. Your normal tools are useless: a compressor cannot catch something three samples long, and an EQ cannot remove it because it is broadband by construction: a sharp transient occupies every frequency at once. What you actually want is what iZotope RX's De-click does: find the damaged samples and *replace them with what should have been there*.

#### Speech is predictable, and a click is not

Over a short window — a few tens of milliseconds — speech is well described by an **autoregressive (AR) model**. That means each sample can be predicted from the samples just before it by a fixed weighted sum:

```
x̂[n] = −(a₁·x[n−1] + a₂·x[n−2] + … + a_p·x[n−p])
```

`x[n]` is the sample at time `n`, `x̂[n]` is the model's prediction of it, `p` is the **model order** (how far back it looks — 32 here), and `a₁ … a_p` are the coefficients. The minus sign and the naming are a convention that makes the algebra later much cleaner: the code stores `a[0] = 1` and defines the **prediction error** or **residual** as

```
e[n] = Σ(k = 0 to p) a[k]·x[n−k]
```

which, because `a[0] = 1`, is just `x[n] − x̂[n]` — actual minus predicted.

Why should speech obey such a model? Because of what the vocal tract physically is. It is a tube with resonances. A resonance is precisely a system whose current state depends on its recent past: that is what makes it ring. An all-pole model is the mathematical description of a set of resonances. So fitting an AR model to a window of speech is, quite literally, estimating the formant structure of that instant. It is the same maths that underlies vocoders and the LPC speech codecs that made early digital telephony possible.

#### Fitting the model

First, what "the window" is, because everything below depends on it. The model is not fitted once to the file. The de-clicker walks each channel in blocks of **10 ms** and fits a fresh set of coefficients to every block — an hour of stereo audio produces something like seven hundred thousand models, not one. That is the whole point. A single model of an entire file would describe nothing, because the thing it is estimating, the formant structure of an instant, changes completely every few tens of milliseconds. (10 ms is also rather shorter than the 20–50 ms that AR speech modelling conventionally uses, and the reason for that is the glottal pulse problem two sections down.)

Two details that matter later. Each fit sees its block plus `p` samples on either side, so the model is not starved of context at the block edges. And channels are fitted and repaired independently, because damage usually hits one side only, and a joint model would let the intact channel steer the repair of the broken one.

With that settled: you want the coefficients that minimise the total squared prediction error over the window. Setting the derivatives to zero gives a system of equations whose entries are the signal's **autocorrelation** — the signal correlated with time-shifted copies of itself:

```
r[m] = Σ_i x[i]·x[i−m]
```

`r[0]` is the window's energy. `r[1]` is how similar the signal is to itself one sample delayed, and so on up to `r[p]`. A strongly resonant signal has large autocorrelation at the lags corresponding to its resonant periods, which is exactly the information the model needs.

The resulting system has a special structure (the matrix is Toeplitz (constant along its diagonals) because `r` only depends on the *difference* of two indices), and the **Levinson–Durbin recursion** solves it in `p²` operations instead of `p³`. It builds the solution order by order, at each step computing a **reflection coefficient** and updating. The name comes from acoustic-tube models, where those coefficients are literally the reflection ratios at boundaries between tube sections of different diameters. The vocal tract analogy is not a metaphor. `src/dsp/lpc.ts` implements it, and bails out if a reflection coefficient strays outside (−1, +1), which signals that the recursion has gone unstable on a degenerate window.

One small but real detail: `r[0]` gets multiplied by 1.0001 and has a tiny constant added. This is called **white noise correction** or ridging, and it stops the model from chasing an exact fit to one window's noise and from producing a singular system on a silent window. It is the same instinct as not letting a compressor's detector have infinite gain.

#### Detection

A click owes nothing to the samples around it; a broken cable produced it, not the vocal tract. So it violates the model badly and leaves a large spike in the residual `e[n]`. Threshold the residual and you have a click detector.

A naive version of this is quite bad, for a reason that matters. One corrupt sample at position `m` does not just make `e[m]` large. The predictor keeps *feeding on* the bad value for the next `p` samples, so `e[m]` through `e[m+p]` are all polluted. Thresholding the forward residual alone therefore flags a run of about 32 samples for a 3-sample click — and "repairs" 29 samples of perfectly good audio.

The fix in `src/dsp/declick.ts` is **two-sided detection**. Run the same model backwards as well:

```
forward:   e_f[n] = Σ a[k]·x[n−k]
backward:  e_b[n] = Σ a[k]·x[n+k]
```

A corrupt sample at `m` pollutes the forward residual over `[m, m+p]` and the backward residual over `[m−p, m]`. Where **both** are large is exactly the damage and nothing else:

```
forward large:   [m₁, m₂ + p]
backward large:  [m₁ − p, m₂]
both:            [m₁, m₂]        ← the actual burst
```

That two-sided test is the difference between a de-clicker that dulls your consonants and one you never notice.

#### Robust statistics

How large is "large"? You need a scale for the residual. The obvious choice is its standard deviation, and it is the wrong one, for a reason that will be familiar from setting a compressor threshold by looking at an average level. The clicks are part of the data you are measuring. A few enormous values inflate a standard deviation dramatically, and the clicks then hide *underneath* their own inflated threshold.

So the code uses the **median absolute deviation** instead: take the absolute value of every residual sample, take the median, multiply by 1.4826. That constant makes MAD agree with the standard deviation for Gaussian data, so `thresholdSigma: 6` still means "six sigma" in the usual sense. The point is that a median does not care about outliers at all: you could make the clicks a thousand times louder and the median would not move. That is what "robust" means.

#### Three refusals

The stage is built to decline rather than guess, in three escalating ways.

Bursts longer than 2 ms are dropped: a genuinely long impulsive event is more likely to be a real transient than damage. Crucially, detections are **merged first and length-filtered second** — merging with the length cap applied first would carve a door slam into a string of maximum-length "clicks" and repair every one of them.

Any analysis block where detections cover more than 5% of the samples is discarded whole. Clicks are rare by definition. A block peppered with over-threshold runs is a transient the model does not fit, and interpolating chunks of it would rewrite real audio.

And if detection covers more than 2% of the entire file, the stage refuses to touch the file at all and says so in its report. That means the threshold is wrong for this material, and doing nothing is far safer than rewriting 2% of every channel on a bad hypothesis.

#### The glottal pulse problem, and the fix that wasn't

The first solution here was well-argued, measured, guarded by a test. And wrong.

Voiced speech is a train of glottal pulses, one every pitch period — 5 to 15 ms apart for adult voices. Those pulses are impulsive excitation. An impulsive-outlier detector will fire on them happily, and it did.

The original fix was to shorten the analysis block. The model is refitted and the threshold re-estimated per block, and that block was cut to **10 ms**, much shorter than the 20–50 ms that AR speech modelling conventionally uses. The reasoning: over a 50 ms block, four or five glottal pulses are a small minority of ~2400 samples, so they sit well above the median and read as outliers; over a 10 ms block, comparable to one pitch period, the pulse **dominates its own statistics**, the median absolute deviation is computed largely from the pulse itself, the threshold rises above it, and the pulse stops being an outlier relative to a block that is mostly pulse. Measured on the corpus: 50 ms blocks gave about two false positives per second of clean speech, 10 ms blocks gave none, and both still caught every injected click.

Every sentence of that is true about the synthetic voice and false about a real one. On a real 19-minute recording the stage repaired **21,591 clicks** — nineteen a second, at the pitch period. The shortened block reduces the problem; it does not cure it. Why the corpus certified the fix anyway is in "The bugs the harness missed", and it is because the synthetic voice's Rosenberg pulse stops dead at closure and is therefore *more* impulsive than a real glottis, not less.

#### The fix that works: a pulse-train veto

The insight is that the discriminating quantity was never the pulse's size against the residual floor. Both a click and a glottal pulse are large there: that is what makes this hard. What separates them is **size against their own neighbours one pitch period away**:

> A click towers over the excitation around it. A glottal pulse has neighbours its own size.

So a candidate is vetoed when the residual anywhere from 2.5 to 20 ms away on either side — one to two pitch periods, covering 50 to 400 Hz — reaches 0.3× its own peak. At that ratio a click must be about 10 dB more impulsive than the voice's own excitation around it to survive; one that is not is being masked by that excitation anyway, so leaving it alone costs nothing audible.

Two sources of "neighbour" are consulted, because each alone has a blind spot. Other *candidates* are checked, since glottal pulses are candidates — but only the cycles that happened to cross threshold. So the raw residual **envelope** is checked too, which sees every cycle. And that in turn needs one guard: a click corrupts the AR model of its own block and inflates that block's residual everywhere, so bins from the candidate's own block are excluded. Without that exclusion a loud click would hide behind the pollution it caused, veto itself, and survive.

The results, and note that the two corpora disagree about what "good" means: on the real recording, 21,591 repairs → **433**; on the eval fixture, 749 → **16**, with the transparency measure going from −34 dB to −49 dB; injected clicks at half the waveform peak still repaired 30 out of 30, at 0.3× still 28 out of 30. On the synthetic corpus, a 0.9× click is now (correctly) left alone, because against that unnaturally spiky voice it is not impulsive enough to be a click. The synthetic cases therefore inject at 2×, with that explanation written into the case, and the honest sensitivity test moved to the fixture cases on real speech.

#### Repair

Once you know which samples are damaged, you replace them with the values that make the signal fit the model best. This is **Janssen interpolation**.

You are minimising the total squared prediction error `J = Σ e[n]²` over the unknown samples, holding the surrounding known samples fixed. Differentiating with respect to each unknown sample and setting to zero gives, for each unknown position `m`:

```
Σ_j ra[|m − j|] · x[j] = 0        over every j within p of m
```

where `ra` is the autocorrelation of the *coefficient vector itself*: `ra[k] = Σ_i a[i]·a[i+k]`. The whole system therefore collapses to `p+1` distinct numbers. Split the sum into unknowns (left-hand side) and known neighbours (moved to the right-hand side, negated), and you have a small dense linear system, solved by Gaussian elimination with partial pivoting.

What this actually does, in ears rather than algebra: it **reconstructs the resonance that was there**. A linear bridge or a crossfade across the hole leaves a dull spot where the formant briefly stopped ringing. The AR interpolation continues the ringing from both sides and meets in the middle. Across a few milliseconds of speech it is inaudible.

Two extra details. The detected burst is **dilated by two samples** on each side before repair, because real clicks decay. The first sample or two tower over the threshold and the tail ducks under it, so repairing only the detected run leaves the tail behind.

And the repair runs **twice**. The first pass uses the model that did the detecting, which was fitted on a block *containing the click*. In quiet audio that is a serious problem: three samples at click amplitude carry orders of magnitude more energy than 10 ms of noise floor, so the model's autocorrelation is dominated by the click, and the "model of the speech" is really a model of the click. The repair inherits that error. So after the first repair, with the click now gone, I refit the model on the repaired neighbourhood and interpolate the gap again, this time against a model of the actual signal.

#### The trade-offs

De-click is conservative by design: three refusal mechanisms, the pulse-train veto, an under-corrected repair, and a false-positive rate held near zero on clean material at the cost of missing quiet clicks. That is the right bias for a tool that runs automatically on material nobody is going to audition frame by frame, and the veto sharpens the bias deliberately, since a click quiet enough to be vetoed is a click quiet enough to be masked. Results on the corpus: `segmentLufsError` on the `clicky` case 7.93 → 0.32 LU, SI-SDR −20.7 → +29.3 dB, worst repair −1.2 dB against local peaks with the bound at +2.

One thing this stage deliberately is not: a *mouth* de-clicker. A lip smack or tongue click is a real acoustic event a few milliseconds long, not three broken samples, and three of the mechanisms above reject it independently — the 2 ms burst cap drops it outright, the 5% block-density guard drops it if it fragments into shorter runs, and the pulse-train veto drops it because it sits within 20 ms of speech louder than itself. None of that is a tuning accident. Every one of those guards is there to stop the stage rewriting real audio, and a mouth click looks like real audio to all of them, because it is. Removing mouth noise needs a detector built on different evidence, and it is listed under what has not been attempted.

One standing instruction, written into the source: **judge this stage on real recordings.** It is the one place in the project where the synthetic corpus is known to point the wrong way.

---

### Stage 2 — Denoise

#### Hiss, hum, and the traffic outside

Hiss, hum, a laptop fan, traffic, room air. Steady, broadband, and sitting underneath everything. Your ear tunes it out after a few seconds; your listener's ear does not, because they have not been in the room.

#### Getting to the spectrum: the STFT

Denoising is the first thing in the chain that works in the frequency domain, so this is where the **short-time Fourier transform** enters. It is in `src/dsp/stft.ts` and it is used by the denoiser and the dynamic EQ, so it is worth understanding properly.

The idea is exactly what a spectrogram display does. Chop the signal into overlapping frames, window each frame, FFT each frame, and you have a picture of how the spectrum changes over time. Process the frames however you like, inverse-FFT them, and overlap-add them back into a waveform.

The two subtleties are both about not introducing artefacts of your own.

*Windowing.* You cannot just chop the signal into rectangular chunks. An FFT implicitly assumes its input repeats forever, so a chunk that starts and ends at different values has an implied discontinuity, which smears energy across the whole spectrum; this is **spectral leakage**, and on a spectrum analyser it is why a pure sine looks like a mountain rather than a spike. The fix is to multiply each frame by a **window** that tapers to zero at both ends. The Hann window — a raised cosine — is the standard choice.

*Perfect reconstruction.* If you window on analysis, process, and overlap-add, you need the windows to sum back to a constant or you get amplitude ripple at the frame rate. And if you window only on analysis, a frame whose spectrum you have modified no longer tapers to zero at its edges, so the discontinuity lands right in your output as a click at every hop.

The arrangement used here is the standard one for magnitude modification: a **square-root Hann window on both analysis and synthesis**, hopping a quarter of the frame length (75% overlap). The product of the two windows is a full Hann window, and Hann at 75% overlap sums to a constant. So the overlap-add divides that constant out exactly. The requirement, stated in the file header, is that with the spectrum left untouched, analysis followed by synthesis returns the input sample for sample, *so that any difference heard afterwards is the processing and not the transform.* That property is unit-tested.

(Two more details in the implementation: the signal is padded by a full frame at each end, so the first and last samples get the same overlap treatment as the middle rather than being attenuated by the window taper alone; and the normalisation is accumulated per output sample rather than assumed constant, so the taper at the very start and end divides out correctly too.)

Defaults: 1024-sample frames, 256-sample hop. At 48 kHz that is 21 ms frames every 5.3 ms, and 46.9 Hz per bin.

#### Estimating the noise

You cannot remove noise until you know what it looks like. The best possible source is the pauses: the moments where there is no speech, so whatever is there is by definition the thing you want gone. The silence analysis has already found them, so the denoiser averages the power spectrum over frames lying wholly inside detected pauses. That is a direct measurement of the target.

When there are not enough pauses (fewer than four usable frames), it falls back to **minimum statistics**: for each frequency bin independently, take the quietest fifth of all frames and average those. The logic is that even in continuous speech, any given frequency band is momentarily unoccupied fairly often, so the quiet end of each bin's distribution is mostly noise. It is more fragile — in genuinely continuous speech the quietest frames still contain speech — which is why the report says which method was used, so the caller can be more conservative.

That fallback is also the one place in the streaming rework of Part 1 where behaviour genuinely changed, and it is worth seeing why. Taking a *percentile* per bin means keeping every frame's value for that bin, which is a number per bin per frame — a gigabyte on a twenty-minute file, and the one quantity in the stage that cannot be folded into a running accumulator. Above 20,000 frames it now subsamples. The justification is the same property the method already assumes: a noise floor is stationary, so a few thousand frames spread across a recording estimate it as well as all of them do. Below the cap, which is the whole corpus and the whole unit suite, nothing changes, and the frame count used was already in the report.

#### Spectral subtraction and the Wiener gain

Now the core. For each time-frequency cell you have an observed power `Y` (magnitude squared) and an estimated noise power `N`. You want an estimate of the clean speech power `S`.

The crudest version — **spectral subtraction** — is exactly what it sounds like: `S ≈ Y − N`, then scale the bin by `√(S/Y)`. It works, and it sounds terrible, for a reason worth naming. `Y` fluctuates randomly frame to frame even for stationary noise. Subtracting a fixed `N` from a fluctuating `Y` leaves scattered bins that happen to survive while their neighbours are zeroed — and an isolated surviving bin in an otherwise-empty region is a **tone**, appearing and disappearing at the frame rate. That is **musical noise**: a shimmering, warbling, underwater artefact that is unambiguously worse than the steady hiss it replaced. Everyone has heard it; it is the sound of a noise reduction plugin pushed too far.

The better formulation is the **Wiener gain**. Define the a-priori SNR of a bin:

```
ξ = S / N        (speech power over noise power in that bin)
```

and apply the gain

```
G = ξ / (1 + ξ)
```

Look at the shape of that. When the bin is mostly speech, `ξ` is large, `G → 1` and the bin passes untouched. When the bin is mostly noise, `ξ` is small, `G → ξ`, which is small. It transitions smoothly through 0.5 at `ξ = 1`. It is the optimal linear estimator in the least-squares sense, and it is essentially a **per-bin downward expander with a very soft knee**. A soft-knee gate on a thousand frequency bands at once. That is the mental model.

The obvious problem: `ξ` involves `S`, the clean speech power, which is precisely what you do not know. The naive substitution uses the *posterior* SNR (`γ = Y/N`, how much louder this bin is than noise alone) giving `ξ̂ = max(γ − 1, 0)`. And this is where musical noise creeps back in, because that estimate is computed from a single frame and is therefore extremely noisy.

The **decision-directed** estimator (Ephraim & Malah) is the fix, and the file calls it, fairly, *the single biggest difference between "denoised" and "underwater"*:

```
ξ̂[k, t] = α · (Ŝ[k, t−1] / N[k]) + (1 − α) · max(γ[k, t] − 1, 0)
```

Read it as: this bin's a-priori SNR is mostly (α = 0.98) what the *previous frame's cleaned output* actually contained in this bin, plus a little (2%) of this frame's raw instantaneous reading. It is a one-pole smoother on the gain trajectory — precisely a compressor's release time constant, applied per bin. Ninety-eight percent weight on history is a long release, and that is what stops the gain flickering frame to frame and turning residual noise musical.

#### Three more things, all of which cost raw reduction and all of which exist to prevent musical noise

*A gain floor.* No bin is ever attenuated by more than a set amount. So residual noise is left as a quiet, natural version of the original rather than as a field of holes with tones scattered in it. This is a genuinely important design point in the codebase: **the floor literally is the reduction target.** Asking for 12 dB of noise reduction sets the floor at −12 dB. Which is why there is no wet/dry control; a global blend would dilute the speech along with the noise, whereas the floor only ever binds where a bin is noise-dominated. That is a better answer to "make it less aggressive" than a mix knob.

*Two passes over the file.* Estimating the noise profile now happens in its own scanning pass before the processing pass, rather than reading frames out of a spectrogram that was going to be kept anyway. That costs a second Fourier sweep and saves holding every frame: the trade the streaming rework in Part 1 makes everywhere.

*Noise over-estimation.* The noise profile is multiplied by 1.5 before subtraction. Slightly over-estimating trades a little speech dulling for markedly less musical noise, and a profile measured from pauses is a slight under-estimate of what sits under speech anyway.

*Gain smoothing across frequency.* The computed gains are averaged over ±2 neighbouring bins before being applied. An isolated surviving bin is exactly what musical noise sounds like, so let its neighbours pull it back down.

#### Knowing when not to run

The eval corpus forced a design change here. Running the full 12 dB over material whose floor already sat 35 dB down *lowered* SI-SDR — there was no noise worth removing, so the processing was the only thing that changed. So the stage now measures the actual programme-to-floor distance and scales the reduction accordingly: a source at 20 dB SNR gets the full 12 dB, one at 35 dB gets none, tapering in between. And when the taper reaches zero, the stage returns the **input signal object itself**, so a clean recording comes out of this stage bit-identical rather than merely similar.

That 35 dB threshold now belongs to the *backend* rather than to the stage, which is a small refactor with a real idea in it. The threshold is the point past which the cure costs more than the disease, and that point is not a property of denoising in general. It is a property of a particular denoiser's transparency. 35 dB was derived from the classical suppressor, which has no idea what speech is and reshapes it whether or not there was anything to remove. A model that decides frame by frame whether a frame is worth touching is a gentler instrument and can honestly claim a higher number. So the interface lets a backend state its own, and the stage uses it when offered.

The stage also measures the noise floor in the pauses before and after and puts both in the report. *A denoiser that claims 12 dB and delivers 3 is a bug you cannot see any other way.*

#### The two backends

There are two kinds of noise reduction worth having and they are not interchangeable, so both sit behind one interface (`src/models/backend.ts`).

The **classical spectral suppressor** described above knows nothing about speech. It removes what is *stationary*, hiss, hum, fan, traffic, and it cannot invent detail, because it only ever attenuates. It is always available and always works.

A **trained model** (DeepFilterNet and relatives) knows what speech looks like. It can remove non-stationary noise the classical method cannot touch; a door slam, a keyboard, a cough. The cost is that it can *hallucinate*: generate plausible speech detail that was never there. For intelligibility that is fine. For a podcast where the speaker's actual voice is the product, it is not, and the roadmap notes that generative enhancers will default to conservative settings for exactly this reason.

The model backend **now runs**. This was for a long time the largest asterisk in this document: the inference path had been written and unit-tested around, but had never once executed, because the machine it was written on could not reach the hosts that serve the weights. It has now been run, and finding out what the export actually is changed the design in several places. The next few sections are that story, because it is a good one: almost everything interesting about running a trained model turned out to be *outside* the model.

#### What you actually get when you download a model

Not a denoiser. The DeepFilterNet3 ONNX export is **three graphs** — an encoder and two decoders — plus a `config.ini` stating the transform they were trained through. The encoder consumes normalised spectral features and emits an embedding and a local-SNR estimate; one decoder turns that embedding into a gain mask over 32 ERB bands; the other turns it into a set of complex filter taps for the lowest 96 bins. Everything else — the transform itself, the ERB filterbank, the running feature normalisation, the application of the mask, the deep filter, the resynthesis — is not in the model at all. It lives in the calling code, and every piece of it has to match what the network was trained through, or the output is subtly wrong rather than obviously broken.

So `src/models/deepfilternet.ts` is a port of upstream's Rust (`libDF`), deliberately written with no ONNX dependency at all so that all of it can be unit-tested without any weights present. That split is the same instinct as `runChunked` taking the inference as a callback: the index arithmetic, the part that can be wrong in a way no ear could localise, is testable with a stand-in that is not a neural network.

A few of the pieces are the kind of thing you would never guess had to be copied exactly.

*The window is not the pipeline's window.* The rest of the chain uses a square-root Hann at 75% overlap. This model was trained through a **Vorbis power-complementary window** at 50% overlap, which satisfies the Princen–Bradley condition and so also gives perfect reconstruction — a different correct answer to the same problem, and the one the network's ears were built around.

*The ERB band widths are ported including their quirks.* The band-edge calculation has a carry that repays bins borrowed by a minimum-width floor, and a final band that absorbs the leftover so the widths sum exactly. Reimplementing that "cleanly" would shift every band edge by a bin or two and quietly invalidate every trained weight in the mask decoder.

*The normalisation constant is ported including its rounding.* The running feature normalisation uses `exp(−hop/(sr·τ))`, and upstream rounds that to three decimal places. At 48 kHz that makes the coefficient exactly 0.99 rather than 0.99005. The whole feature sequence the model was trained on is conditioned on that rounding, so it is copied rather than corrected. Fixing an upstream rounding error is, here, breaking your input.

#### A mixed-radix FFT had to be written

DeepFilterNet3 works on a 960-sample transform — 20 ms at 48 kHz, a sensible choice for speech and a number the model does not get to change. But 960 is 2⁶·3·5, and the FFT in `src/dsp/fft.ts` was radix-2 only: it factors the transform into repeated halvings, which requires a power of two, and it throws on anything else. That is the right implementation for a project that only ever measures spectra of frames it chose itself, and the wrong one the instant something else picks the frame size. So `createFftPlan` is a plain recursive Cooley–Tukey that handles any size whose prime factors are 2, 3 and 5: split the input into `r` interleaved subsequences, transform each, combine with twiddle factors. The twiddle table and scratch buffers are built once per plan so a plan reused across every frame of a file allocates nothing per frame. It is tested against a direct DFT at several sizes, because a wrong FFT does not crash. It returns plausible numbers.

#### Two guesses about the export were wrong, and both would have shipped silently

These are the interesting ones.

*There is no recurrent state on the graph boundary.* The obvious mental model of a streaming neural denoiser is a frame-at-a-time loop threading hidden state from one call to the next, and the code was originally shaped for that. It is wrong. The GRUs are *inside* the graph, and they unroll over whatever time axis they are handed in a single call. You feed it a long span of frames and it recurs internally. The graph is also exactly causal — verified by growing the amount of future context and watching the output not move, which is a nice example of testing a property of something you cannot see inside. What follows is that splitting a file into chunks is not a correctness question but a memory question: ONNX Runtime holds a whole span's activations, at roughly 210 MB per 1000 frames. Chunking at 1000 frames with a discarded 1000-frame warm-up prefix costs about 0.15 dB of SI-SDR against a whole-file run, which is what makes it an acceptable trade rather than a compromise. Crossfading the chunk seams was tried and removed: it moved nothing, because the residual difference is spread through the chunk rather than concentrated at the join.

*The lookahead is the caller's job.* The PyTorch model shifts its own input by `conv_lookahead` frames before running. The ONNX export does not include that shift. So the output of model frame `t` describes spectrum frame `t − 2`, and it is the calling code's responsibility to line them up. Get it wrong and — this is the part worth sitting with — **the mask still works.** It is simply 40 ms early. It arrives before the sound it was computed for, which smears onsets and takes the front off consonants, and it *sounds like a mediocre denoiser rather than like a bug*. There is nothing to notice, no crash, no silence, no obvious artefact; just a slightly worse tool that you would spend a week trying to tune. The measurement is unambiguous once you go looking: 19.9 dB SI-SDR at a lookahead of 2, and about −2 dB at 0, 1 or 3. Twenty decibels of quality riding on a two-frame index offset that nothing in the file format tells you about.

#### And a departure from upstream's own runtime

The ERB gain mask is only valid above the deep filter's range. In training, the masked spectrum is kept *only* for the bins the deep filter does not cover (the low bins are always overwritten by the filter), so the network was never given any reason to emit sensible gains below that boundary, and it does not. On clean speech the bands below bin 96 come back at about 0.25, a flat −12 dB, while the bands above sit correctly near unity. Upstream survives this because in the usual path the deep filter overwrites those bins anyway. But its stage-selection logic has a branch, local SNR between two thresholds, where the mask runs and the deep filter does not, and in that branch the junk gains reach the output. Confining the mask to the bins it was trained for takes the same fixture from 3.8 dB to 25.4 dB SI-SDR, and on *clean* input from 6.6 dB to 51.6. Not a tuning improvement: the difference between a denoiser and a speech destroyer.

#### Verification is per file, not per archive

Weights are never bundled — they are large and carry their own licences — so `pnpm fetch-model` downloads the export from the upstream project's own repository, deliberately not from one of the several community mirrors that carry the same bytes without the same provenance. It checks the archive's SHA-256, extracts it, and then checks the SHA-256 of **every file inside it** against a separately pinned hash, verifying before installing so a bad download never lands half-written in the model directory. Hashing the tarball alone would have been less code and worth less: it would not catch a single swapped decoder inside an otherwise-correct archive. A model is the whole set or nothing, since an encoder that verifies paired with a decoder that does not is not a partially usable model; it is an unknown one. And an empty expected hash is treated as a *failure*, not a pass; a registry entry nobody has pinned yet must not act as a wildcard that accepts whatever happens to be at that path.

The runtime itself, `onnxruntime-node`, is an **optional dependency**. It is about 100 MB of native code, and the classical backend is a complete working denoiser without it. If it is absent, the model backend reports itself unavailable with the reason, and the chain runs as it always did.

#### Results, including the bad one

On real speech at 20 dB SNR — a fixture recording with known noise added, denoise stage alone, scored against the clean original — the two backends separate cleanly and not in the direction the raw noise numbers suggest:

| backend | noise removed | SI-SDR change | programme loudness cost |
| --- | --- | --- | --- |
| classical | 10.7 dB | **−0.2 dB** | 0.20 dB |
| model | 8.2 dB | **+5.0 dB** | 0.05 dB |

Read that carefully, because it is the whole argument for the download. The classical suppressor removes *more* noise and ends up *further from the clean recording*. It is trading signal for quiet, which is exactly what the SI-SDR metric was built to catch and exactly what your ear will eventually call "processed". The model removes less and gets closer.

And now the bad result, which is the more interesting one. **On this project's synthetic corpus the model destroys the programme.** Handed `noisy-20db` it attenuates the voice by 10 dB — bounded only by the requested reduction, which is the difference between a quiet render and an empty one.

The temptation is to read that as a port bug. It is not. It is a statement about the corpus, and it is the single sharpest illustration in this project of something Part 4 is entirely about. The synthetic material is speech-*shaped*: a harmonic stack with formants, prosody and a syllable-rate envelope. That is more than enough to evaluate a spectral suppressor, because a suppressor only ever asks **what here is stationary**, and the answer to that question does not depend on whether the non-stationary part is a person. A trained model asks a completely different question: **am I hearing speech?** And about this material, DeepFilterNet3's answer is no, and it is entitled to that answer. It has never heard a Rosenberg pulse train in its life.

Two things follow, and both are in the code rather than in this paragraph.

*There is no synthetic quality bound on the model, because there could not be an honest one.* A bound saying "the model must score well on the synthetic corpus" would be measuring the corpus's synthetic-ness, and passing it would mean the model had become less discriminating. The model's quality bounds live on the degraded-fixture cases instead, where the reference is a real recording.

*The stage gained a programme-loss guard.* After a backend runs, the stage measures the gated programme loudness the backend cost, the same gated measurement the leveller uses, so pauses do not drag it down, and if that exceeds `maxProgrammeLossDb` (3 dB), it **throws the output away**, returns the input untouched, and says in its report exactly why. The limit is set above what noise removal alone can explain: at 6 dB SNR the noise is about a fifth of the total power, so removing all of it costs roughly 1 dB of measured loudness, and 3 dB leaves room for worse sources than that. On real recordings the model costs under 0.7 dB, so the guard is free. On material it misreads, it turns a wrecked render into a declined stage.

Note which way round the danger runs. A classical suppressor fails by doing too little: you hear the noise it left. A trained model fails by *confidently rewriting*, and a confident rewrite of a voice can be perfectly pleasant to listen to and completely wrong. The guard exists because the failure mode of the better tool is the harder one to notice.

This is also what makes the per-backend clean threshold safe. The model states 45 dB where the stage's default is 35, which lets it engage on recordings the classical suppressor would decline — justified by costing 0.00 dB of programme on a real 38.5 dB recording and returning 42.8 dB SI-SDR forced onto clean material, where the classical backend on the same material returns 28.4. Raising a threshold is only responsible when something else catches the case where the raise was wrong, and here the guard is that something: on synthetic speech the higher taper does let the model run, and the guard is then what stops it.

The corpus asserts both safeguards. `ood-denoise-onnx` asserts the guard fires on material the model misreads — bit-identical output, and *a real number there would mean a model is free to decide the speech is the noise and have that reach the file*. `clean-denoise-onnx` asserts the two compose: a backend permitted by the taper to run on clean material still cannot get speech damage into the file.

One more measurement that stayed a measurement rather than becoming a preference. The model's own local-SNR gate — which declines to touch frames it judges clean enough — I tried it both ways. Removing it made things worse: keeping it is what makes the model transparent, because on clean speech it declines on 82% of frames, and that is 58.0 dB SI-SDR against 44.5 without. A model with an off switch it uses four times out of five is doing the same thing every other stage in this pipeline does.

Results on the corpus: on the full default chain, `segmentSnrGainDb` reads +23.2 dB on `noisy-20db`, +9.3 dB on `noisy-6db` (where SI-SDR also goes positive), and +14.2 dB on `quiet`. But note that these are now the denoiser and the expander *together*, since both move that number by design, and they are separable only in the stage reports (`floorReductionDb` is the expander's alone). The denoiser's own contribution is bounded unconfounded on the fixture cases. On clean material the stage still does nothing at all, and that is now asserted as bit-identity on `clean-denoise` and `clean-denoise-onnx` rather than inferred from an SNR window.

---

### Stage 3 — Dereverb

#### Why a room is hard to remove

The recording sounds like a room. Not a nice plate on the vocal, an actual untreated bedroom with parallel walls. You cannot EQ it away, because the room is not a frequency response — it is a *time* response.

#### What reverberation is, mathematically

A room takes the sound the speaker produces and adds delayed, attenuated copies of it: the direct sound arriving first, then discrete early reflections off nearby surfaces, then a dense exponentially-decaying tail. The whole thing is a **convolution**: the recorded signal is the dry voice convolved with the room's impulse response.

```
y[n] = Σ_k h[k] · x[n−k]
```

`x` is the dry voice, `h` is the room impulse response (what you would record if you fired a starter pistol in the room), and `y` is what the microphone captures. Every sample of the output is a weighted sum of the recent past of the input, which is the same shape as the FIR filters in Part 0, because a room *is* a filter. It is a very long one: a 0.6-second RT60 at 48 kHz is nearly 29,000 taps.

The **late** part of `h` (everything past the first few tens of milliseconds) is what makes a recording sound distant. The direct sound and early reflections actually carry intelligibility and sound natural; you do not want to remove them.

#### The idea behind WPE

One observation makes weighted prediction error work: the late reverberation at time `t` is a linear function of the signal's own past. It literally *is* the past, delayed and scaled. So if you can predict the current frame from frames a short delay back, whatever the prediction accounts for must be reverberation, and you subtract it.

Working per frequency bin in the STFT domain, for bin `k`:

```
d[t] = y[t] − Σ(i = 0 to L−1) conj(g[i]) · y[t − Δ − i]
```

- `y[t]` is the complex STFT value of the observed signal at frame `t` in this bin.
- `d[t]` is the desired output — direct sound plus early reflections.
- `g[0…L−1]` are the complex prediction filter coefficients for this bin, which is what you are solving for.
- `L` is the number of taps (20 by default), so the filter reaches back 20 frames.
- `Δ` is the **prediction delay** (3 frames), and it is the whole safety mechanism: the filter is not allowed to look at the most recent Δ frames at all. Without it, the filter would happily predict, and therefore cancel, the *speech itself*.
- The conjugate on `g` is the standard convention for complex correlation, and getting it wrong is exactly the bug that was found here.

#### The "weighted" part

Ordinary least squares would fit `g` by minimising `Σ|d[t]|²` with all frames counting equally. That is wrong for this problem, and the reason is neat: the loudest frames are precisely the ones where the direct sound is strongest and reverberation matters least. Least squares would concentrate its effort on the frames you care about least.

So each frame is weighted by the inverse of the *desired* signal's power there:

```
weight[t] = 1 / λ[t],     λ[t] = |d[t]|²
```

Frames that are mostly reverberant tail have small `λ`, so they get large weight and dominate the fit. That is the "weighted prediction error" in the name.

Of course `d[t]` is the output, which you do not have before you have the filter. So the whole thing is **iterated**: start with `λ[t] = |y[t]|²` (the observation's own power), solve for `g`, compute `d`, re-estimate `λ` from `d`, solve again. Three iterations by default. This is an expectation-maximisation-flavoured alternation, and it is the same structural idea as the decision-directed SNR estimator in the denoiser — estimate a quantity you need from the output you last produced.

Solving for `g` at each iteration means building the weighted correlation matrix `R` (taps × taps, complex) and cross-correlation vector `r`, and solving `R·g = r`. The **normal equations**, complex-valued, by Gaussian elimination with partial pivoting in `solveComplex`. This is done for every frequency bin, for every iteration. It is why the stage is slow.

Two stabilisers, both discovered by things going wrong:

*Diagonal loading.* `R` is near-singular wherever the signal is quiet, and an unregularised solve there produces enormous filters that subtract far more than the room put in. Adding a small constant to the diagonal (`1e-4` of the mean power, times the frame count) bounds the solution. This is ridge regression, and it is the same instinct as the white-noise correction in the AR fitting.

*A floor on the weighting power.* The `1/λ` weighting is the whole idea, but near-silent frames would otherwise carry effectively infinite weight and drag the fit to a filter that subtracts wildly. And because each iteration re-estimates `λ` from its own output, that failure *runs away* rather than settling. The floor sits at 1/1000 of the mean power.

#### The frame size is load-bearing

Frame size turned out to matter more than anything else here, and it belongs in Part 4 as well.

WPE's core assumption is that the *desired* signal is uncorrelated across the prediction delay: that `d[t]` and `d[t−Δ]` have nothing to do with each other, so anything the filter can predict must be room rather than voice. Speech violates that assumption badly at short delays, because speech is **periodic**. A voiced sound repeats at the pitch period, 5 to 15 ms.

At the pipeline's usual 1024/256 setting, a delay of two frames reaches back about 10 ms — roughly one pitch period for an adult male voice. So the filter looks back exactly one glottal cycle, finds the signal highly predictable (of course it does, it is periodic), predicts the voice's own harmonic structure, and subtracts it. Measured: **a dry recording came back at 1 dB SI-SDR.** Destroyed.

At 4096/1024 with a delay of three frames, the filter reaches back 64 ms — past the pitch period, past the vocal tract's own ringing, leaving only the room's tail to predict. The same dry recording comes back at 20 dB.

I chose the settings by sweeping frame size, delay and tap count against **both a reverberant recording and a dry one**, because — as the commit says — *damage to material that never needed treating is the failure that matters.*

#### Knowing when not to run

Even at 20 dB SI-SDR on dry material, that is not transparent enough to run unconditionally. So the stage measures reverberation **blind** and passes dry material through untouched.

The blind measurement is in `src/dsp/reverbtime.ts` and it is nicely simple: build a broadband energy envelope in 5 ms frames, find local maxima that are followed by a sustained fall (those are moments where speech stopped), and measure how long the energy takes to drop 20 dB from each. Take the median. That is a T20 measurement, the same idea as a room acoustics measurement, without needing to fire a starter pistol in the room. Dry speech measures around 35 ms: that is the talker's own articulation, not a room. A noticeably live room lands past 150 ms. The engagement threshold sits at 90 ms.

When the stage declines, it returns the **same signal object**, so `dry-dereverb` asserts bit-identity rather than approximation.

Note also where that measurement lives: in `src/dsp/`, not with the eval metrics, *because the stage needs it at runtime, and a stage reaching into the test fixtures for a measurement would be exactly backwards.*

#### Be honest about this one

Single-channel WPE is a modest tool. On the corpus it takes about **12% off the measured decay** and gains about **1.3 dB of SI-SDR** against the dry reference. That is a real improvement and it is nothing like the dramatic dereverberation you may have seen demonstrated. Those results are almost always **multi-channel**, where the spatial information — the fact that direct sound arrives at different microphones at different times while diffuse reverberation does not — does most of the work. With one microphone you do not have that information and no amount of cleverness creates it.

What WPE does offer, and the reason it earns its place over a trained enhancer, is that **it cannot invent speech**. It only ever subtracts a linear prediction. Its failure mode is leaving reverberation behind, not fabricating words that were never said. For a podcast where the speaker's actual voice is the product, that is the right trade.

It is also the slowest stage in the chain by a wide margin — roughly 0.6× real time, because it solves a dense complex 20×20 system per bin per iteration, 2049 bins × 3 iterations per channel.

It has since had one round of optimisation, worth 1.7×, and the interesting thing about it is that both wins were the *same shape* as the one that made the dynamic EQ slow: a value recomputed inside an inner loop that could have been read once. The correlation accumulator is a 20×20 double loop per frame, and it was fetching the real and imaginary parts of the tap history *inside* it — 840 accessor calls per frame, each an array-of-arrays indirection through a closure, to read the same twenty complex numbers over and over. Reading them into two small flat arrays first is 1.5× on its own. Then: the correlation matrix is **Hermitian**, so half of it was being computed twice. Accumulating only the upper triangle and mirroring the rest is another 1.2×, and it is *exact* rather than approximate — IEEE multiplication commutes and negation is lossless, so the mirrored entry carries the same bits the full loop produced. Both changes are bit-identical, verified sample-for-sample against the previous implementation, and the eval baseline did not move.

What that did not fix is memory. Dereverb still materialises its entire spectrogram, about 2 GB on a twenty-minute file, because WPE genuinely needs every frame of a bin at once, as described in Part 1. Bounding it means block-wise processing, which changes behaviour rather than just cost: the room estimate would begin adapting over the file instead of being fitted once. That is a design decision, so it is recorded as a limitation rather than made silently.

---

### Stage 4 — Corrective EQ

#### Colouration you stopped hearing an hour ago

A room mode makes the voice boxy at 240. The microphone has a presence peak that reads as harsh. The desk the mic is clamped to resonates. These are constant — present throughout the recording — which is exactly what a static EQ is for.

#### Measuring what to fix: the LTAS

The **long-term average spectrum** is the average power spectrum over a long stretch of material. Take an FFT of each window, square the magnitudes, average across all windows. That average washes out the individual sounds (which vowel, which consonant), and leaves the systematic colouration: the room, the mic, the voice's own broad character.

`src/dsp/ltas.ts` implements it, with two decisions that matter more than the mechanics.

*Speech frames only.* Averaging over the whole file lets the pauses' noise floor drag the spectrum down between syllables, so the curve stops describing the voice and starts describing a blend of the voice and the room ambience. The caller passes the speech ranges, the silence analysis already knows them, and only frames wholly inside them are averaged.

*Fractional-octave smoothing on a log grid.* Raw FFT bins are linearly spaced: at 8192 points and 48 kHz they sit 5.9 Hz apart, all the way up. That is a wildly inappropriate scale for this question. Near 100 Hz, 5.9 Hz is a substantial musical interval; near 10 kHz, hundreds of bins fit inside a single semitone. Ears work in octaves; this is why your EQ plugin has a logarithmic frequency axis. So the LTAS is resampled onto a log-spaced grid (12 points per octave, 50 Hz to 16 kHz) where each point averages the power across a constant *fraction* of an octave — one third by default, the resolution of a graphic EQ.

There is one more parameter, `minBandwidthHz: 120`, and it is the single most interesting line in the file. It is discussed in Part 4.

#### The target curve, and why it is not a target curve

The obvious design is to compare the measured LTAS against a reference "good voice" curve and correct the difference. This is what spectral-match plugins do, and it is rejected here for a good reason: an absolute curve would **impose one announcer's timbre on every voice**. A deep voice and a bright voice have genuinely different long-term spectra, and both are correct.

Instead the target is *the LTAS smoothed much more broadly* — 1.5 octaves, averaged in power. The deviation to correct is:

```
deviation(f) = LTAS(f) − broadTarget(f)
```

Think about what that quantity is. The 1.5-octave-smoothed version follows the voice's overall tilt — its actual character — but cannot follow anything narrower than about an octave and a half. So resonances and notches show up as deviations, and the broad shape does not. You are correcting the recording toward *a smoother version of itself*, which removes what the room did while leaving what the speaker is.

This is precisely what a good engineer does with a spectrum analyser: not "match this curve" but "what is sticking out that shouldn't be".

#### Fitting filters to the deviation

`fitCorrectiveEq` in `src/dsp/eq.ts` is greedy: find the largest remaining deviation, place a peaking filter that cancels most of it, subtract that filter's *analytic* magnitude response from the running deviation, repeat.

Subtracting the analytic response rather than re-measuring matters: the code computes the actual `|H(f)|` in dB of the biquad it just designed, at every grid point, so the next band sees the true residual including the skirts of the previous band. Bells overlap; ignoring that gives you five bands that collectively over-correct.

Q is estimated from the shape of the deviation: walk outward from the peak until the deviation falls to half its value, and convert that width in octaves to Q by the standard relation

```
Q = √(2^BW) / (2^BW − 1)
```

where `BW` is the bandwidth in octaves. Wide bump, low Q; narrow spike, high Q. Exactly what you would do by ear, sweeping a bell to find the width of the problem.

#### The constraint set is what separates corrective from auto-EQ'd

The fitter is a few dozen lines. The constraints are the design:

- **At most five bands.** A forest of filters is a spectral match, and spectral matches sound processed.
- **Cut-biased gain limits.** Cuts capped at 6 dB, boosts at 3. Cutting a resonance is nearly always safe — you are removing energy that the room added. Boosting a notch dredges up whatever lives down there, which is usually noise and room, because a notch is often a cancellation and you cannot un-cancel something by turning it up.
- **No boosting into a poor noise floor.** The stage separately computes the LTAS of the *pauses*, on the same grid. If the speech-to-noise ratio at a grid point is below 15 dB, a boost there is forbidden — *boosting a band whose SNR is poor is buying timbre with noise.*
- **Q clamped to 0.7–4.** Wide enough to correct, never surgical. A surgical notch on a voice is audible as a hole.
- **Deviations under 2 dB are left alone entirely.** Small wiggles are what voices sound like.
- **Under-correction.** Bands remove 80% of the deviation they target, not 100%. This is how human engineers EQ, and full cancellation makes the greedy fit ring and pump between bands.

There is a small bug worth mentioning because of how it manifested: the band budget was originally consumed by *attempts* rather than *placed bands*. So a few notches early in the sweep that the fitter declined to fill (because of the SNR gate) ate the whole budget of five, and there was nothing left for the actual resonances further up. The fix is to remove a gated grid point from play and continue, only counting bands that were genuinely placed. Classic off-by-one-in-spirit bug: correct code, wrong accounting.

#### The rumble filter

Separately from the bells, the stage decides whether a high-pass is warranted. It compares energy well below the voice (20 Hz to the candidate corner, 80 Hz) against the fundamental region (120–300 Hz). Speech has no business carrying energy an octave below its own fundamental. When the sub-bass comes within 12 dB of the fundamentals, something non-vocal (traffic, handling noise, HVAC, a nearby road) is down there, and a Butterworth high-pass at 80 Hz earns its place. On clean material it stays off, *because an always-on filter is not "corrective".*

And a detail that is a nice illustration of a theme: **the rumble decision reads the raw PSD, not the smoothed log grid.** The grid's minimum-bandwidth floor deliberately blurs the bottom octaves, and that same blur smears a voice's fundamental down into the sub-bass region — enough to trip this test on perfectly clean speech. Two different questions need two different resolutions, which is why the `Ltas` object carries the unsmoothed PSD alongside the smoothed curve.

#### The voicing curve, which is a different job

The stage now does a second thing after the corrective fit, and the two are deliberately kept apart: a fixed **voicing**. A tonal tilt applied identically to every file. Correction is fitted per recording and removes what *that* room and *that* microphone added. Voicing is the same on every file and is a **taste**, not a measurement of anything about the recording in front of it. Mixing the two would be the spectral-match mistake in a new outfit, and it would make the report unreadable: you could no longer tell which of the bands you were looking at was a decision about this recording. So the voicing bands ride *after* the corrective cascade, they are listed separately in the report, and the corpus pins `voicing: "neutral"` on the cases that exist to measure the fitter so those bounds keep meaning what they say.

The default voicing is called `warm` and it is **+1 dB below 130 Hz and −1 dB across 2.5–5 kHz**. In the code that is two peaking filters: +1.0 dB at 95 Hz with a Q of 0.7, wide enough to read as weight rather than as a bump on somebody's fundamental, and −1.1 dB at 3.4 kHz with a Q of 0.9, which is where a close microphone in an untreated room puts its hardness. That is the whole curve. It is on by default because it is what this project is trying to sound like, and it is *safe* to have on by default precisely because at ±1 dB it is small enough that the corrective fit is still doing the substantive work.

The instructive part is how much smaller the curve got. I recovered the curve by measurement, from a 24-minute before/after pair of the same recording through a commercial mastering service, and I got it wrong the first time in an instructive way. Measured on the loudest frames, that service's 5–6.5 kHz region reads about **−3 dB**. Three decibels of broad cut right where sibilance and harshness live: that is a de-harshing dip, unmistakably, exactly the sort of thing you would put in a "voicing" preset and feel good about.

Then measure it again on a restricted set of frames — only those where *that particular band* sits 25 dB above its own noise floor — and the same region reads **−0.2 dB**.

The dip was not EQ. It was **per-band noise suppression**. Here is why the two are so easy to confuse: even a loud vowel has very little genuine 6 kHz content. Most frames of most speech have that band sitting near its own noise floor, which is precisely where a suppressor is working hardest. So averaging over all frames measures mostly the suppressor's behaviour and reports it as tone. Restricting to the frames where there is real signal in that band measures the tone, and the tone is essentially flat.

Baking that −3 dB in would have made every recording through this pipeline duller, permanently, for no reason anyone could hear — and it would have been very hard to argue with afterwards, because it came from a measurement of a service everybody likes the sound of. What survives the correction is the ±1 dB above and nothing else outside ±0.6 dB. Which is itself the finding, and it is worth stating as a result rather than a footnote: **that service's sound is almost entirely its dynamics and its suppression, not its tone.** Two-thirds of the value of that whole measurement exercise was learning that the thing everyone assumes is an EQ curve is not one.

Results: `spectralFlatteningDb` +4.35 dB on the `boxy` case, +1.22 dB on clean material, and the `clean-eq` case bounds that second number both above and below, because *an auto-EQ that always acts is not corrective*. Both of those cases run with the voicing off, so they measure the fitter alone. The voicing has a case to itself, `warm-voicing`, and the only honest bound on a taste is that it stays small: output SI-SDR must hold above 12 dB, because a voicing that scores below that has stopped being a tilt and become a filter. Worth being straight about one wrinkle there, since this document has a section for exactly this sort of thing: on that case the reference is a clone of the input and SI-SDR is scored against the reference *put through the same chain*, so both sides come out identical and the metric reads `+inf`. The bound passes without currently constraining anything. It would bite the moment the voicing became input-dependent, which is precisely what it must never become. But as a measurement of the curve's gentleness today, the number doing real work is the one the whole-chain cases see, which is that the voicing moves spectral flatness by about 0.1 dB.

---

### Stage 5 — Dynamic EQ (and the de-esser)

#### Resonances that come and go

The static EQ handles colouration that is present throughout. What it cannot fix is the kind that comes and goes: a vowel that rings on one particular note, a sibilant that spikes at 7 kHz, a plosive that booms. Cut those statically and you dull the whole recording to fix a problem that occurs 3% of the time. You need a filter that reacts.

#### Compare the spectrum with a blurred copy of itself

Soothe and its relatives run on this rule. For each frame, compare the spectrum against a **smoothed version of itself**, and pull down whatever protrudes.

The reasoning is that anything narrow and loud relative to its own frequency neighbourhood is a resonance almost by definition, while the broad spectral shape (which is the voice) passes through untouched. It is the same "correct toward a smoother version of yourself" principle as the static EQ, applied per frame instead of over the whole file.

The elegant consequence: **it needs no threshold in dB**. There is no "set the threshold to −18" to get wrong. The comparison is relative, so it works across voices, across levels, across mic distances without tuning. That is a real advantage over a conventional de-esser, which needs re-tuning every time the singer moves.

#### The mechanics

In `src/dsp/dyneq.ts`, for each frame and each bin `k`:

```
level[k]     = 10·log₁₀(|X[k]|²)                       the bin's level in dB
envelope[k]  = mean over neighbours j of level[j]       the local reference
excess[k]    = level[k] − envelope[k] − threshold[k]
target[k]    = excess > 0 ? −min(maxReduction, excess · ratio) : 0
```

`ratio` is 0.5 — remove half the excess above threshold, exactly a 2:1 compressor. `maxReduction` is 8 dB, so a genuinely loud note is tamed rather than erased.

One implementation detail is deliberate and easy to get wrong: **the envelope is a mean of dB values, not a dB of mean power.** Averaging in power would let a big peak lift its own reference and hide from the comparison; the loud bin would dominate the neighbourhood average it is being compared against. Averaging in dB is a geometric mean in linear terms, which is far less sensitive to one large member.

#### Attack and release

Gain that changes freely frame to frame modulates the signal at the frame rate, which is audible as flutter. So each bin's gain has an envelope follower across frames, with a one-pole coefficient:

```
coefficient = exp(−hopTime / timeConstant)
state = target + (state − target) · coefficient
```

Attack (2 ms) when the required attenuation deepens, release (60 ms) when it eases. Fast down, slow up: the standard compressor arrangement, for the standard reasons: catch the transient, let go gently.

#### The de-esser is not a separate device

Sibilance sits in a known band and is the most common complaint, so the threshold is simply 3 dB lower between 5 kHz and 9 kHz. That is the entire de-esser. Conceptually this is nice: sibilance is not a different problem from resonance, only a more common one in a predictable place.

Below 300 Hz the stage is disabled outright (the threshold is set to infinity). The fundamental and the first formant *are* the voice, not a resonance to be shaved.

#### The threshold, and the 92% bug

The default protrusion threshold is **12 dB**, which is much higher than intuition suggests. Six decibels above the local neighbourhood sounds like plenty for "this is sticking out".

At 6 dB, measured on clean speech, **92% of all time-frequency cells were being attenuated.** That is not a de-esser; that is a spectral bulldozer, and it would have sounded like a slightly dull, slightly lifeless voice that you could easily talk yourself into calling "smooth".

The cause is that the reference is biased low. The envelope is a dB-mean of a *fluctuating* spectrum, and the dB-mean of a fluctuating quantity sits well below its typical value — the same reason the geometric mean of a spread-out set of numbers is lower than the arithmetic mean. So most bins genuinely do read as protruding above their own dB-mean neighbourhood, by several dB, all the time.

Sweeping against both a clean recording and one with a sustained injected resonance: at 12 dB the stage touches **5%** of cells and delivers the *same* +4.9 dB SI-SDR benefit on the resonance that 6 dB did. All that extra activity was cost with no benefit.

The lesson, and the commit records it as such: **threshold picked by measurement, not by taste.**

The other cause of that 92% was the smoothing window, which is Part 4.

Results: +5.1 dB SI-SDR on the `ringing` case, about 5% of cells touched on clean speech. And because this stage cannot decline to act, it is per-frame by nature, with no "should I engage" decision available, its transparency has to be *measured* rather than arranged. The `clean-dyneq` case bounds absolute output SI-SDR at ≥15 dB with exactly that reasoning written into it.

#### A logarithm in the wrong place

For a while this stage was the chain's bottleneck by an absurd margin: 4951 ms of a 6.3 s render, about 79% of the total, against 344 ms for the two Fourier transforms it depends on. Profiling put 93% of its runtime in a single loop, and the cause is a mistake that is invisible in the algebra and obvious in the code.

Look again at the two lines above. `level[k]` is a logarithm. `envelope[k]` averages `level` over a smoothing window that is, on average, 84 bins wide. Written naively, the average is a loop over neighbours that computes each neighbour's level as it goes — so every bin's logarithm is recomputed once for each neighbour whose window covers it. That is **443 million `Math.log10` calls per minute of audio to produce 5 million distinct values.** Hoisting the conversion out of the inner loop is a 5.5× speedup by itself, and it is bit-identical: the same expression, evaluated once instead of 84 times.

Two smaller wins take it to 6.8× overall. The window average now comes from a **running prefix sum** — sum every level once, and any window is then two lookups and a subtraction instead of a loop. And a bin whose gain state is exactly zero, which is most of them, skips its `Math.pow`; that works because a settled bin sits at literal zero, which is exactly representable, so the comparison is safe in a way float comparisons usually are not.

The prefix sum is the one change that is *not* bit-identical — summing in a different order gives different rounding — so I measured it rather than assuming. Across 2.6 million samples exactly one differs, by 1.7 × 10⁻¹⁸: about 355 dB below peak, three orders of magnitude below what a float32 sample can even represent, and the corpus reported nothing moved. The stage is now 624 ms of a 2.1 s render, in line with its neighbours rather than dwarfing them, and `pnpm eval` went from 51 s to 31 s as a side effect.

---

### Interlude. The two level-domain stages, and where their numbers came from

Stages 6 and 7 are a downward expander and a compressor, and they arrived together because they were measured together. The measurement is worth one explanation before either stage, because it is the reason to believe any of the numbers in the next two sections, and because the exercise produced one finding that is more interesting than either stage.

**Why they exist at all.** The leveller (Stage 8, the thing this whole project grew out of) sets one gain per speech segment. That fixes the difference between a passage recorded close to the microphone and one recorded at arm's length, between yesterday's take and today's. It is the right tool for that and it does it transparently, because all its gain movement happens in the silences where there is no speech to hear it move.

What it cannot touch is the difference between the start and the end of a single sentence. And here is the number that justifies these two stages existing: measured on a commercial service's own processing, **its gain moves a median of 4.5 dB within one continuous utterance, against 3.3 dB between utterances.** More than half of what that processor does happens *inside* a phrase, in the places a segment leveller by construction cannot reach. If you level a recording and it still does not sound finished, that ratio is why.

**The measurement.** A 24-minute recording was put through a commercial mastering service and the before and after were aligned to the sample, correlation 0.991, which is what "aligned" has to mean before you difference two files, and then differenced across 14,441 frames. From that difference you can read off the processor's applied gain frame by frame, and from a scatter of applied gain against input level you can read off its curve. What came out:

- Its gain begins falling as input rises above about **2 dB under the programme loudness**, at a slope corresponding to about **1.7:1**.
- Its gain trajectory fits a first-order smoother with a **33 ms fall and a 168 ms rise**.
- Below about **27 dB under the programme** it expands, at roughly **2.8:1**.
- Its loudness range goes from 5.00 LU to 3.38 LU. (This chain manages 4.98 → 3.80 LU, which is in the same country.)

Those numbers were read off a measurement rather than chosen, which is the reason for doing it this way at all: "1.7:1 with a 33 ms attack" is then not a taste anybody has to defend.

**The shared machine.** A compressor and a downward expander are the same device pointed in opposite directions — measure the level, look up a gain on a static curve, smooth that gain over time, apply it — so both live in `src/dsp/dynamics.ts` and differ only in the curve and the two time constants. Three decisions in that shared machine carry the quality, and all three are the sort of thing that separates plugins you like from plugins you do not.

*The **gain** is smoothed, not the envelope.* You can put the time constants in either of two places: smooth the detector's level reading and then look up a gain, or look up a gain from the raw level and then smooth the gain. They are not the same. Smoothing the detector first makes the *effective* time constant depend on where you are on the curve (steeper part of the curve, faster apparent response), which is one of the things people are describing when they say a compressor "breathes". Smoothing the gain gives one time constant that means the same thing everywhere. It is also, measurably, what the reference processor does: its gain trajectory fits a first-order smoother *on the gain* to within a few hundredths.

*One gain for every channel.* The detector sums power across channels and a single gain is applied to all of them. Detecting and applying per channel would let a loud left channel duck while the right stayed put, which moves the stereo image sideways in time with the speech, an artefact that is subtle in isolation and horrible over an hour. Same reasoning as the limiter's shared gain-reduction curve in the level stage.

*No make-up gain.* A compressor normally needs one: pulling the peaks down makes everything quieter, so you put back what you took. Not here, because the level stage runs afterwards and normalises to an exact loudness target. Make-up gain would be a number the leveller immediately takes back out, so leaving it off costs nothing, and it buys something real, which is that **the compressor's report stays honest**. The gain it shows is the gain it applied, not that gain plus a cosmetic offset.

One more detail from the detector, which is a nice small instance of the theme that runs through this whole document. The envelope detector's window is 15 ms, and it is a boxcar, a plain sliding average of the last 15 ms of power, rather than a one-pole. The boxcar gives the detector a *definite* memory: a syllable leaves the window entirely instead of trailing an exponential tail into the next one. And the window cannot be much shorter, because at a 100 Hz fundamental a window under 10 ms would ripple at the pitch period and modulate the gain with it. Pitch again, deciding a time constant.

---

### Stage 6 — Downward expander

#### The gaps between the words

Between the words, there is nothing masking the room. During speech, the voice covers the floor; in the gaps, the floor is naked and it is the thing your listener hears as "recorded in a bedroom". This is the job you would reach for a gate to do, and it is exactly the job a gate does badly.

#### A compressor pointed downwards

The mirror image of a compressor. Above a threshold a compressor makes loud things quieter by some ratio; below a threshold an expander makes quiet things quieter by some ratio. The curve in `expanderCurve` is:

```
under      = threshold − level
reduction  = ratio_slope · under          (below the threshold, past the knee)
gain       = −min(reduction, range)
```

`level` is the detector's reading in dBFS, `threshold` sits 27 dB below the programme's integrated loudness, and `ratio_slope` is `ratio − 1`. So at a ratio of 2.8, every decibel further below the threshold buys 1.8 dB of extra attenuation. The knee is 8 dB wide and quadratic across it, continuous in both value and slope at each end, for the same reason a compressor's knee is soft: speech spends most of its time near the threshold, and a kink there is audible as the gain flicking on and off.

#### Why the threshold is relative

27 dB *below the programme loudness*, not an absolute dBFS number. An absolute threshold would expand a quiet recording into oblivion and miss a hot one entirely. Relative, it means the same thing on every file. The compressor's threshold is relative for the same reason.

#### The cap, which is the whole design

`rangeDb` is 12, meaning the expander may never attenuate by more than 12 dB no matter how far below the threshold the signal goes. The reference this was fitted to reaches about 40 dB at 43 dB under its programme. That difference is deliberate and it is the most important judgement in the stage.

An expander with unlimited range **is a gate**. And a recording gated to digital silence between words sounds broken in a way the noise never did: the room vanishes and reappears with every syllable, the noise floor pumping in and out of existence at the speech rate, which draws far more attention to itself than a steady floor ever would. It is the same problem the room-tone bed elsewhere in this project exists to solve, arrived at from the opposite direction; there, you are pasting room tone *in* so an edit does not drop out; here, you are declining to take it all *away* for exactly the same reason. Bounded attenuation keeps the room present, just further down.

#### The time constants are asymmetric the other way round from a compressor's

Opening (gain rising back to unity, because a word has started) takes 5 ms; closing (gain falling, pushing the floor down) takes 150 ms. Fast open, slow close. The asymmetry is chosen by which mistake is audible: a slow open clips the front of a word, which is immediately obvious and destroys consonants, while a slow close merely lets the floor linger a moment longer after the speech stops, which nobody notices.

#### Knowing when not to run

Like every other stage here, it declines. It measures the programme-to-floor distance — programme loudness minus the mean loudness of the detected pauses — and if the floor already sits more than 50 dB down there is nothing left worth pushing, so it returns the input signal object itself and comes out bit-identical. Expanding a recording that does not need it only risks chewing the quiet ends of words for no audible gain. The `clean-expand` case asserts that decline as bit-identity.

#### And it deliberately breaks a metric

The expander is what made `SNR_PRESERVED` untrue, as described in Part 2. It moves segment-local SNR by 10 to 13 dB *by design*, because it attenuates the gaps between words and those gaps are exactly where that metric samples its noise. Its own case, `floor-expand`, bounds `floorReductionDb` at ≥3 dB (the reference manages 3.6 dB of programme-to-floor on real material, so 3 is the floor of useful), bounds segment SNR gain between 6 and 14 dB (the upper edge is the 12 dB cap plus a little, so the bound still fails if the cap stops holding), and bounds SI-SDR against a genuine clean reference at ≥0. That last one is the check that it is removing noise rather than words, and it reads +1.20 dB.

Results: 7.3 dB of floor reduction on `floor-expand`, about 10 dB on the full-chain cases, and bit-identical output on a recording whose floor is already deep.

---

### Stage 7 — Compressor

**The problem.** Within one sentence, a speaker gets quieter as they run out of breath, leans back from the microphone, trails off at the end of a clause, then leans in for the next point. The leveller cannot see any of it: it is one gain per segment, and this moves *underneath* that gain. This is the difference between a recording that has been levelled and one that sounds mastered, and per the measurement above, it is more than half of what a commercial service is actually doing to you.

**The curve.** Standard downward compression, fitted to the reference:

```
over       = level − threshold
reduction  = (1 − 1/ratio) · over          (above the threshold, past the knee)
gain       = −min(reduction, maxReduction)
```

`threshold` is 2 dB below the programme's integrated loudness — note how low that is. This is not a peak catcher sitting up at −6 dBFS waiting for shouts; the knee is in the middle of ordinary speech, which is where it has to be if the point is to even out a phrase rather than to clip its loudest word. `ratio` is 1.7, gentle by any standard. `kneeDb` is 10, because the reference's own curve bends over roughly that range rather than turning a corner, and because with the threshold sitting in the middle of the speech a hard knee would have the gain flicking across it constantly. `maxReductionDb` is 12, a cap that exists so that a mis-set threshold cannot turn the stage into a fader — and, as the corpus bound puts it, *if this ever sits at the cap the curve is wrong, not the cap*.

Attack 33 ms, release 168 ms, both read off the reference. Fast down, slow up, the standard arrangement for the standard reasons.

**Why there is no look-ahead.** The detector is delay-free with respect to the output, so a transient arrives before the gain has finished moving. That is what an attack time *means*, and pre-delaying the signal to hide it would turn the compressor into a limiter — a different device with a different sound. The thing that catches what gets through is the true-peak limiter at the end of the level stage, which is one of the reasons that stage runs last.

**Knowing when not to run**, and this one needed a new measurement to answer. A compressor's job is to reduce **loudness range**, so loudness range is what tells it whether it has a job. `src/dsp/loudness.ts` gained an implementation of EBU Tech 3342: measure short-term loudness on 3-second blocks, gate them, and report the spread from the 10th to the 95th percentile of what survives, in LU. It answers the question integrated loudness cannot — whether a programme sitting on target does so evenly, or by averaging a shout and a mumble.

Speech that genuinely needs compression measures 5 LU and up. A heavily processed source can arrive at 2, and compressing that costs whatever life it has left and buys nothing, so below 3 LU the stage declines and returns the input untouched. `even-compress` (three spurts at identical levels) asserts that decline as bit-identity, *because a compressor with no off switch is a sound, not a tool*.

And the stage reports loudness range before *and* after, measured rather than assumed, for the same reason the denoiser reports the noise floor twice: a compressor that claims a ratio and delivers nothing is a bug only that number makes visible.

Results: on `uneven`, a corpus case where each phrase swells 12 dB from start to finish, which is precisely the unevenness one gain per segment moves *with* rather than against, loudness range falls by 3.4 LU and SI-SDR against the even reference improves by 2.8 dB. Deepest gain reduction 5.2 dB, comfortably clear of the 12 dB cap. On the full default chain it takes between about 0.6 and 3.2 LU out of the corpus cases, and its deepest reduction across all of them is 4.3 dB. A third of the cap, which is where you want it to sit.

---

### Stage 8 — Level

The project grew out of this stage, and it remains the most standards-bound.

#### Loudness: ITU-R BS.1770 / EBU R128

You know LUFS as the number streaming platforms normalise to. Here is what the number actually is.

*K-weighting.* Before measuring anything you filter the audio, because your ear does not weight all frequencies equally. K-weighting is two biquads: a high-shelf "head" filter of about +4 dB centred near 1.7 kHz (modelling the acoustic effect of a head in a sound field; the reason a listener hears high mids a little louder than a flat measurement would suggest), and an RLB high-pass at about 38 Hz (rolling off the very low frequencies that contribute little to perceived loudness). `src/dsp/kweighting.ts` derives the coefficients **analytically for any sample rate** using the same formulation as libebur128, rather than hard-coding the 48 kHz numbers printed in the standard. That is why the leveller can be rate-agnostic and needs no resampling.

*Mean square to loudness.* Loudness is then

```
L = −0.691 + 10·log₁₀( Σ_c G_c · z_c )
```

where `z_c` is the mean of the squared K-weighted samples in channel `c`, `G_c` is a per-channel weight (1.0 for left/right/centre, ~1.41 for surrounds), and −0.691 is the standard's calibration offset. It is a weighted RMS in dB, on filtered audio. That is all.

*Gating.* The important part, and the part that makes LUFS behave sensibly on speech. Measure in **400 ms blocks at 75% overlap** (so a new block every 100 ms). Then throw blocks away twice:

- **Absolute gate**: any block below −70 LUFS is discarded. That is digital-silence territory and should not drag the average down.
- **Relative gate**: compute the mean loudness of the surviving blocks, then discard any block more than 10 LU below *that*. Then average what remains.

The relative gate is what makes the measurement report *the loudness of the programme* rather than the loudness of the programme diluted by the pauses. Without it, a recording with long silences would measure quieter than an identical recording with the silences edited out, which is not what anyone means by loudness.

This gating also matters inside the harness. A bug found in phase 1: segment loudness was being measured **ungated**, which reads about 2 LU low on syllable-modulated speech, because the dips between syllables get counted. That would have shown up as a phantom levelling error in every result. Gated integrated loudness is what a LUFS target means, what the leveller measures to pick its gain, and what the generator normalises to; the metric now uses the same thing.

#### Silence detection

A short window — 100 ms, hopping 25 ms — slides across the file and each window's loudness is measured. The threshold sits between the estimated noise floor (the 10th percentile of window loudness) and the integrated loudness:

```
threshold = floor + fraction · (integrated − floor),   fraction = 0.25
```

"A quarter of the way from the floor up to the programme level." Runs of sub-threshold windows longer than 1 second become silence regions. There is also **Otsu's method** available as an alternative: the bimodal-histogram valley trick from image thresholding, which finds the split point that best separates two clusters. It is a nice option to have because it makes no assumption about where in the range the threshold should sit; it lets the data show you the valley between "speech" and "not speech".

#### Segmentation and gain

Segment boundaries are the file edges plus every silence **midpoint**. So a segment runs from the middle of one silence to the middle of the next. Each segment's gated integrated loudness gives a target gain of `target − measured` dB, clamped to ±30 dB.

Gain is held constant across each segment's speech and **ramped linearly across each silence** from the left segment's gain to the right's. If segment A was cut 3 dB and segment B boosted 2, the silence between them ramps −3 → +2 dB. All the gain movement happens where there is no speech to hear it move.

That is the core insight of the original tool, and the thing to hold on to is that it is **not** a compressor. A compressor rides gain continuously and changes the dynamics *within* each phrase, which is what makes heavily-compressed speech sound flat. This changes gain only between phrases, so within any phrase the speaker's own dynamics are completely untouched by *this* stage. It is what a careful engineer does with clip gain, done automatically.

That distinction survives the arrival of Stage 7 rather than being undermined by it, and the relationship between the two needs saying plainly. The chain now does both jobs, with different tools, on purpose: the compressor works inside a phrase at 1.7:1 and a 12 dB cap, which is a light touch by any standard, and the leveller does everything between phrases without touching dynamics at all. What is being avoided is doing *all* of it with a compressor — riding a single detector hard enough to reconcile a whisper and a shout thirty seconds apart, which is how you get speech that is even in level and dead in life. Split the job by timescale, and each half can be gentle.

#### The target, and why it moved

The default is **−18 LUFS**, and it used to be −23. That is a change of what the tool is for, not a tuning decision — the number moved because the job did. −23 is EBU R128, a broadcast *delivery* target, and it was exactly right while this was a Levelator-style tool handing material to someone else's chain — you leave headroom because the next person is going to use it. It is now the chain, delivering finished spoken word, where −18 is the ordinary target and the one the commercial reference this project is measured against uses. Broadcast delivery is one `--target -23` away.

Two consequences follow. The eval corpus reads the default rather than restating it, so the harness keeps testing what a user actually gets rather than fossilising a number. And one leveller unit test that asserted −23 as a literal (where it meant "the target") now reads the default, so it checks that the leveller *hits* its target instead of checking which target that is. The REAPER script is deliberately left at −23, because it writes a take-volume envelope inside a session rather than rendering a finished file, and so is still doing the old job.

#### The true-peak limiter

After levelling, some boosted segments will exceed the ceiling, so a feed-forward limiter with a ~5 ms lookahead attack and a ~50 ms one-pole release catches them, applying one gain-reduction curve to all channels so the stereo image does not shift.

The attack was originally instant, clamp the gain to whatever the peak requires, in one sample, and that was a bug you could hear, though not on anything in the corpus. A gain curve that steps down in a single sample puts a sharp corner into the waveform, and a corner is broadband: this is very nearly the definition of clipping, arriving by a different route. On the corpus the limiter barely engages and the corner never appears. On a real recording at 8.7 dB of reduction it was obvious enough to be mistaken, at first, for the source of all the distortion in the chain.

The fix exploits the whole-file design from Part 1. Rather than a delay line and the latency bookkeeping that lookahead normally implies, the gain curve is computed forward as before and then walked **backwards**, easing each sample's gain down toward its successor's so the required reduction is reached exactly when the peak arrives:

```
gain[i] = min( gain[i],  1 − (1 − gain[i+1]) · attackCoeff )
```

That is a one-pole running in reverse. Because the pass only ever *lowers* gain, the ceiling guarantee is untouched — no sample can end up louder than the forward pass allowed — which is the property that makes this safe to bolt on rather than a redesign of the limiter.

The detector runs on the **true peak**, not the sample peak, and this is where Part 0's second consequence of sampling comes back. Between two samples, the waveform a converter reconstructs can overshoot both of them. A signal measuring −1 dBFS sample peak can genuinely reach +0.5 dBTP coming out of a DAC — and, more practically, lossy encoders reconstruct the same inter-sample content and clip on it. A sample-peak ceiling of −1 dBFS routinely passes material that hits −0.3 dBTP.

The standard's answer, in BS.1770 Annex 2, is to oversample by at least 4× through a band-limited interpolator and take the peak of *that*. `src/dsp/truepeak.ts` implements the standard's 48-tap 4× design as four 12-tap polyphase phases: that is, four separate 12-tap FIR filters, each reconstructing the waveform at a different quarter-sample offset. Run all four, take the largest magnitude, and you have a good estimate of what actually happens between your samples.

The limiter needs this as a per-sample *envelope*, not just a single number, because the sample-domain gain curve has to respond to overshoots living between the samples it can touch, and the interpolated peak near sample `i` is also affected by the gain applied at `i−1` and `i+1`, so each local peak is spread to its neighbours.

The harness's `hot` case exists specifically for this: quiet crest-heavy speech boosted about 20 dB into the ceiling, where inter-sample overshoot is what decides whether the ceiling holds.

#### Room tone

The extra output. From each detected silence, the cleanest window is extracted; those clips are crossfaded together and looped into a bed at least 10 seconds long, written as `<name>_roomtone.wav`. It is for the editor: when you cut a breath out or splice two takes, you have matching room tone to lay under the join so the floor does not drop out.

The details are all about what "cleanest" means. Room tone is the *quietest steady* part of a silence, so the cleanliness score is loudness-first. A breath is simply louder than the floor. There are extra penalties for clicks (detected as a ~10 ms block whose peak towers over the *median* block peak; a robust statistic again, because noise is naturally peaky and a mean would be fooled) and for swells (high short-window RMS variation). Silences much louder than the cleanest are dropped entirely.

The looping has a nice touch: **every other repeat is time-reversed**, so identical material never crossfades into itself. Two identical copies crossfading add coherently and build up level in the middle of the fade, which reads as a pulse every loop; reversing alternate copies makes them incoherent and the crossfade behaves. And the whole bed is gained by the length-weighted mean of the speech-segment gains, so it sits at the same level as the processed voice.

---

## Part 4: The recurring theme — pitch is not a defect

Three independent bugs in three different files had the same root cause, which is reason enough to give it a part of its own.

### The setup

A voiced sound is periodic. The vocal folds open and close at the fundamental frequency `f₀` (say 110 Hz for an adult male), and that periodicity means the spectrum is not continuous. It is a **harmonic comb**: energy concentrated at `f₀`, `2f₀`, `3f₀`, `4f₀` … with genuine valleys in between. At 110 Hz, that is spikes every 110 Hz with troughs between them, and those troughs can be 20 dB deep.

Meanwhile the *thing you actually want to measure*, the room's colouration, the microphone's character, a resonance, is the **spectral envelope**: the smooth shape that the comb's peaks sit under. That envelope is what carries the formants, the boxiness, the harshness. It is what your ear extracts and what an EQ should act on.

So: any spectral analysis of speech has to be coarse enough to see the envelope and not the comb. Analyse too finely and the pitch itself looks like a spectrum full of peaks and notches, which is to say, it looks exactly like a defect to be corrected.

This is not a subtle failure. A tool that "corrects" the harmonic comb is cutting the speaker's harmonics and filling the gaps between them. It removes what makes the voice a voice.

### Where it bit

**The LTAS.** The corrective EQ smooths the spectrum to one-third of an octave, which is a perfectly sensible resolution: it is what a graphic EQ gives you. But a third of an octave at 100 Hz is only **23 Hz wide**, and the harmonics of a 110 Hz voice are 110 Hz apart. So down there, the "smoothed" curve was resolving individual harmonics, and an EQ fitted to it would chase peaks and troughs that are the *pitch*, not the room.

The fix is `minBandwidthHz: 120` — a floor on the smoothing bandwidth in Hz, so that the window never gets narrower than about 120 Hz no matter how low you go. Note the shape this produces: roughly constant bandwidth in Hz below 500 Hz, roughly constant in octaves above. That is, essentially exactly, the shape of the auditory filters in your own cochlea. The fix converges on the biology, which is a good sign that it is the right fix rather than a fudge.

**The dynamic EQ.** Same disease, worse symptoms. Its per-frame reference is a half-octave neighbourhood — half an octave at 400 Hz spans 336 to 476 Hz. For a 110 Hz voice, that window holds **barely one harmonic**. So every harmonic read as a peak towering above its own neighbourhood (because its neighbourhood was mostly the valleys either side of it), and the stage attenuated the voice's harmonic structure rather than any resonance. That is the other half of the 92%-of-cells measurement. The fix is `minSmoothingHz: 400`, the same idea as the LTAS's Hz floor.

**WPE, differently.** Here the axis is time rather than frequency, but the culprit is the same periodicity. WPE assumes the desired signal is uncorrelated across the prediction delay. Speech at a delay of one pitch period is *maximally* correlated: that is what periodic means. So a filter allowed to look back 10 ms found the voice highly predictable, predicted it, and subtracted it. Pushing the reach out to 64 ms — past the pitch period and past the vocal tract's own ringing — leaves only the room.

**And the rumble detector, as a fourth variant.** Once the LTAS gained its Hz floor to avoid resolving the comb, that same blur smeared the fundamental *downward* into the sub-bass region, enough to trip the rumble test on clean speech. The fix was to have the rumble decision read the raw unsmoothed PSD instead. Two different questions, two different appropriate resolutions, from the same measurement object.

### The fifth variant, which is about a corpus rather than a window

The four above are all the same shape: a measurement window chosen without asking what a voice actually looks like at that scale. The newest instance is not about a window at all, and that is what makes it worth adding. The theme turns out to be more general than "pick your resolution".

The model denoise backend, run against this project's synthetic corpus, attenuates the voice by 10 dB. The corpus is a harmonic stack with formants, prosody, jitter and a syllable-rate envelope. It was built — carefully, and after a rewrite that fixed several versions of exactly the problem described above — to behave like speech for every measurement the pipeline takes. And DeepFilterNet3 listens to it and decides it is noise.

The instinct is to call that a failure of the port or a failure of the corpus. It is neither. It is two tools asking two genuinely different questions of the same material:

- **A spectral suppressor asks: what here is stationary?** That question can be answered correctly about material that is speech-shaped, because stationarity is a property of the signal's statistics and does not care whether a person produced it.
- **A trained model asks: am I hearing speech?** That question cannot be answered correctly about material that is speech-shaped-but-not-speech, and the model's "no" is not a mistake. It is the model working. A model that said yes to a synthesised harmonic stack would be a *worse* model; it would be one that could not tell the difference, which is the entire faculty you downloaded it for.

So this is the same lesson as the four above, one level up. There, the mistake would have been to measure a voice at a resolution where its pitch looks like a defect. Here, the mistake would be to evaluate a speech-recogniser-adjacent tool on material whose speech-ness was never the property being simulated. In both cases the failure is silent and plausible: nothing crashes, an answer arrives, and the answer is a confident measurement of the wrong thing.

And in both cases the fix has the same shape too — decide what question you are asking, and pick the instrument that can answer *that* question. Which is why there is no synthetic quality bound on the model, why the model's quality is bounded on real recordings degraded with known noise, and why the stage carries a programme-loss guard that does not care which backend produced the damage. "Pitch is not a defect" and "synthesis is not a voice" are the same sentence with the scope widened.

### The sixth variant, which is the theme turning round and biting

The five above are all versions of the same warning: the pitch is there, do not mistake it for damage. The sixth is the same fact arriving from the opposite direction, and it is the most expensive one in the project, so it goes last.

The de-clicker fires on impulsive outliers in the prediction residual. Glottal pulses are impulsive outliers in the prediction residual: this is variant one all over again, in the time domain rather than the frequency domain: the pitch, mistaken for a defect, in the most literal way available. It was noticed early, a fix was written, the fix was measured on the corpus, and a case was added to guard it. Everything the first four variants teach you to do was done.

And the fix was still wrong on real speech, by a factor of fifty, for months.

The reason is the fifth variant compounding the first. The synthetic voice's glottal source is a **Rosenberg pulse**, which stops dead at the moment of closure. A real glottis has a return phase, it closes over a few hundred microseconds, not instantly, and that difference makes the real excitation measurably gentler. So the synthetic voice was not a slightly-imperfect model of a real one in this dimension; it was *harder than reality in exactly the dimension being tested*, and a threshold tuned to clear its spiky pulses sat comfortably between the softer pulses of every real voice. The corpus said zero false positives. Reality said nineteen a second.

That is the failure mode worth taking away, because it is the one the other five do not prepare you for. A corpus that is easier than reality gives you optimistic numbers, which you will eventually notice. A corpus that is *harder than reality in the wrong dimension* gives you a passing test that actively certifies a broken stage, and there is nothing in the number to tell you.

The eventual fix, described in Stage 1, is the theme resolved rather than avoided. You cannot separate a click from a glottal pulse by how big it is, because the whole difficulty is that both are big. You separate them by asking whether the thing has **neighbours one pitch period away**. The discriminator is the periodicity itself: pitch is not a defect, and here it is not even a nuisance — it is the evidence.

### The lesson

There is no single correct resolution for analysing speech. There is a correct resolution *for each question*:

- "What is the room doing to this voice?" — coarse enough to skip the comb entirely. Envelope only.
- "Is there sub-bass that isn't the voice?" — fine enough to separate 40 Hz from a 110 Hz fundamental, so raw bins.
- "Is this frequency ringing right now?" — coarse in frequency, fine in time.
- "Is this sample damaged?" — no frequency analysis at all; a time-domain model over 10 ms. But "damaged, or just voiced?" *is* a pitch question, answered by looking one pitch period either side.
- "Is this a voice at all?" — not a resolution question. No window setting makes synthetic material answerable, which is why that question has to be asked of a real recording or not asked.

And the sneaky part is that the wrong resolution does not produce an error or a crash. It produces a perfectly plausible measurement of the wrong thing, and a stage that acts on it confidently. The dynamic EQ at 6 dB threshold with a half-octave window was not broken in any way a programmer would notice. It ran, it produced audio, the audio sounded fine-ish. Only a harness that could say "you are attenuating 92% of everything" caught it.

Which brings us back to Part 2. And, in the sixth variant, back to Part 2's limits. The harness caught four of these six. It certified the fifth as a failure it could not interpret, and it certified the sixth as a pass when the stage was mangling every real recording it touched. The instrument that found that one was a pair of ears listening to twenty-one minutes of an actual voice, blind, not knowing which render was which.

---

## Part 5: What is unfinished, unverified, or weak

I would rather be straight about this than write a roadmap.

### The model backend now runs — and that changes what the caveat is rather than removing it

For most of this project's life the honest statement here was that the inference path had never executed once. It has now been run end to end, on real audio, and it beats the classical backend by 5.2 dB of SI-SDR on real speech at 20 dB SNR. What remains true, and matters more than the old caveat did:

- **Its correctness rests on a port, and ports drift.** Everything around the three graphs — the transform, the window, the ERB band edges, the running normalisation and its copied rounding, the lookahead alignment, the deep filter — is a hand translation of upstream's Rust. It is unit-tested without weights, which catches slicing and index errors, and the end-to-end SI-SDR numbers say the whole assembly is right *today*. If upstream changes its export, the checksums will refuse to load it, which is the desired outcome; but nothing here tracks upstream's own changes to the surrounding DSP.
- **The lookahead lesson is a standing warning.** A two-frame index offset that nothing in the file format announces is worth 20 dB of quality and does not announce itself when wrong. Any future change to framing anywhere near that code should be checked against the measurement, not by ear.
- **It is unverified on anything but 48 kHz**, because it declines to run at anything else.
- **It has never been evaluated for hallucination.** The whole argument for preferring WPE over a trained dereverberator, made in the dereverb section, is that a linear subtraction cannot invent speech. A trained denoiser can. The evidence gathered so far says this one is conservative — it declines on 82% of frames on clean speech, and its programme-loudness cost on real material is under 0.7 dB — but "conservative on the material tested" is not "cannot fabricate", and no test in this project would detect a plausible-sounding fabrication. The programme-loss guard catches *deletion*, not invention.

The backend's *absence* remains loud rather than silent, which was true before and still is: `unavailableReason` states exactly what is missing, the stage's report records which backend ran and which were skipped and why, and the classical suppressor takes over so a user without weights still gets a working denoiser rather than a stage that quietly does nothing. Weights are never bundled. They carry their own licences, they are large, and a local-first tool should let the user decide what to download.

### The compressor and expander are fitted to exactly one reference

Every number in those two stages came from a single 24-minute before/after pair, of one voice, through one commercial service, at one moment in that service's development. That is a genuine measurement and it is far better than picking numbers by taste, but it is n = 1. A different service, or the same service next year, might sit somewhere else entirely, and nothing here establishes that 1.7:1 at 2 dB under programme is *right* rather than merely *what that one processor did*. The two deliberate departures from the reference (the 12 dB expander cap and no make-up gain) are argued rather than measured, which is the honest description of them.

The corpus checks that both stages act when they should and decline when they should, and that neither exceeds its cap. It does not and cannot check that the curve is the correct curve.

### Dereverb is slow, and it is the one stage whose memory still scales with file length

It has had one round of optimisation worth 1.7×, described in Part 3, but it still solves a dense complex 20×20 system per frequency bin per iteration, three iterations, 2049 bins, per channel, and it is still the chain's dominant cost when it engages. There is more available: the correlation matrix is Hermitian, so a Cholesky factorisation would roughly halve the solve, and the bins are completely independent of each other, so they are embarrassingly parallel.

The memory is the sharper problem. Every other spectral stage now streams, so the chain's footprint is flat in file length; dereverb still materialises its whole spectrogram, about 2 GB on a twenty-minute file, because WPE genuinely wants every frame of a bin at once. Fixing that means block-wise processing, which is a behaviour change, the room estimate would adapt over the file instead of being fitted once, and possibly a better one for a recording where someone moves. It has not been tried.

### Dereverb is also just modest

Restating from Part 3 because it matters for expectations: ~12% off the decay, +1.3 dB SI-SDR. Single-channel dereverberation is a weak tool. The dramatic results you have heard are multi-channel. This will not rescue a recording made in a stairwell.

### Almost everything is still tuned against synthetic material

This remains the biggest caveat, though it is now a sharper one than it used to be. The corpus is speech-*shaped*, not speech. It has a glottal source, moving formants, prosody, jitter and syllable-rate amplitude modulation — which is enough to exercise everything the *classical* pipeline measures — but it is not a person talking into a microphone in a room. Real speech has consonants with genuinely different statistics (plosives, fricatives, nasals), breaths, mouth noises, lip smacks, variable mic distance, and rooms that are not exponentially-decaying noise.

What has changed is that the limit of that approach is no longer a suspicion. It is now two measured facts, and the second is worse than the first.

The first: a trained model handed this corpus decides it is not hearing speech and removes 10 dB of it. Part 4 argues that this is the corpus being honest about what it is rather than the corpus being broken, and that argument holds.

The second is the one that should change how much you trust the rest of this document. **The claim that "every stage except the model backend is a tool whose question the corpus can answer" was itself false, and it took a real recording to find out.** The de-clicker's question (is this sample damaged, or is it the voice's own excitation) is exactly the kind of question the corpus was supposed to be able to answer, and it answered it wrongly for months, in the direction that made a badly broken stage look perfect. The full account is in Part 4's sixth variant; the short version is that the synthetic glottal source is *more* impulsive than a real one, so passing the synthetic transparency test was evidence of nothing.

That does not invalidate the corpus, which is still the reason nothing else in the chain has rotted. It does mean the correct statement is weaker than the one this section used to make: the corpus reliably catches *regressions*, and validates *novel behaviour* only as far as the material happens to be representative in the dimension that stage cares about, which is not knowable in advance. Every remaining synthetic-only bound in the list below should be read in that light.

### Room tone still admits breaths

The room-tone bed scores candidate clips for loudness with penalties for clicks and swells, and a breath defeats all three at once: only moderately louder than the floor, broadband rather than impulsive, and long enough to read as a steady level. It needs a term for spectral shape, since a breath is noise but tilted differently from room tone. There are no breaths anywhere in the synthetic corpus, so this was never going to be caught by the harness; it is tracked as an open issue, and labelled real material is being collected against it with the annotator described in Part 2.

### Long recordings were unexercised until recently, and one class of bug lives only there

Two failures, an `Math.max(...array)` spread that overflows V8's argument limit at about 65,000 elements, and a spectrogram that reached 4.5 GB, were invisible to every test in the suite because fixtures are seconds long and recordings are minutes long. Both are fixed (Part 1), and there is now a regression test that builds 150,000 frames at a low sample rate so it costs a megabyte rather than a gigabyte. But the general hazard remains: nothing runs a twenty-minute file in CI, so the next bug of that shape will also be found by a person rather than by the suite.

The `eval/fixtures/` mechanism exists precisely so real recordings can be dropped in and picked up as extra cases, and it is now doing real work: the degraded-fixture cases are the only place the model's quality is asserted at all. Extending that treatment to the rest of the chain is still the highest-value next step available. Some specific things worth watching for when it happens:

- The de-clicker's remaining thresholds are still corpus-tuned even though its pulse-train veto is now calibrated on real speech. Creaky voice is the obvious hazard: creak is impulsive *and irregular*, so its pulses may not have the evenly-spaced neighbours the veto looks for, which is precisely the configuration that would read as a burst of clicks. Nothing in the corpus contains creak.
- The dynamic EQ's 12 dB threshold was swept against synthetic material with a single injected sine resonance. Real sibilance is broadband and noisy, not a tone.
- The rumble detector's 12 dB margin has only ever seen a synthetic 38 Hz sine standing in for traffic.
- The synthetic room impulse response is exponentially-decaying noise. Real rooms have discrete early reflections and frequency-dependent decay (high frequencies die first), neither of which is modelled. The file is honest about being a model rather than a measurement.
- The expander's 27-dB-under threshold and 12 dB cap have only met synthetic floors, which are stationary broadband noise. Real room tone has structure — a hum, an air-conditioning tone, traffic that comes and goes — and an expander riding a floor that moves is a different problem from one riding a floor that does not.
- The compressor's detector has never seen a plosive. A 15 ms boxcar on a real "p" is a very different reading from a 15 ms boxcar on a synthetic syllable envelope.

### One bound is currently vacuous

As noted in the EQ section, `warm-voicing` asserts output SI-SDR ≥ 12 dB, but on that case both the input and the reference take the same path through the same fixed curve, so the metric reads `+inf` and the bound cannot fail. It is not wrong, just not yet earning its place; a case that applied the voicing to *degraded* material, or compared against the unvoiced render, would be the version that measures something.

### Not yet attempted at all

: multi-band or spectral levelling, breath removal, plosive reduction, mouth-click removal as distinct from digital clicks, speaker separation, and anything to do with music beds. All of these are things Auphonic does.

---

## Appendix: poking at it

```bash
pnpm test            # the Vitest suite — 236 tests
pnpm eval            # the corpus, with bounds — 26 cases, 2 of which need model weights
pnpm typecheck
pnpm start           # build and launch the Electron window
pnpm fetch-model     # download and verify the DeepFilterNet3 weights
pnpm listen          # the blind A/B listening app
pnpm listen:session  # build a blinded session from a spec
```

Dropping a `.wav` into `eval/fixtures/` adds several more cases beyond those 26, and after everything in Part 4 you should read that as the important half rather than the optional extra: it is where the de-clicker's sensitivity, the de-clicker's transparency, and the model backend's quality are all actually asserted.

`pnpm fetch-model` is the only step between a fresh checkout and the model backend running. It downloads the export from upstream's own repository, verifies the archive's hash, verifies every file extracted from it, and installs into `~/.audio-leveller/models`. Without it, the two ONNX cases in the corpus report as skipped and the denoiser falls back to the classical suppressor — everything else works exactly as before.

```bash
node dist/cli.js input.wav [output.wav] [options]
```

| option | meaning |
| --- | --- |
| `--only <a,b>` | run only these stages, in chain order |
| `--bypass <a,b>` | run the chain but bypass these |
| `--target <lufs>` | target loudness for the level stage |
| `--report <file>` | write the full JSON report |
| `--json` | print JSON instead of the text summary |
| `--list-stages` | list available stages and exit |

The tuning loop the tool is built around: render the same file with a stage on and off, compare the two reports, and listen to the two outputs.

```bash
node dist/cli.js take1.wav /tmp/with.wav    --report /tmp/with.json
node dist/cli.js take1.wav /tmp/without.wav --bypass dyneq --report /tmp/without.json
```

With eight stages this is more useful than it was with six, and the two newest are the ones most worth doing it with. `--bypass expand,compress` against the full chain is the clearest way to hear the difference between a recording that has been levelled and one that sounds mastered, and `--only compress` isolates a stage whose whole justification is a thing that happens inside a phrase.

And when you change something in the DSP, the other loop:

```bash
pnpm eval --save-baseline /tmp/before.json
# ... make the change ...
pnpm eval --baseline /tmp/before.json
```

which prints exactly what moved. That is the instrument this project is really built around.
