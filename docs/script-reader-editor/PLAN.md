# Script-reader editor

Editing audio by editing text, for the case where the text was written first.

This is a plan, not an implementation. Nothing here is built yet.

## The problem

Narrating a script — a podcast read from notes, a book read aloud — produces a
recording that is *mostly* the script and occasionally something else. The
something else is always one of four things:

- a **misreading** — a word said differently from the one on the page,
- a **skip** — a word or line that never got said,
- an **addition** — an improvised aside, a filled pause, a repeated word,
- a **retake** — the reader stops, backs up a phrase or a sentence, and reads
  it again.

Everything a script reader does in an editor afterwards is dealing with those
four. Retakes are the bulk of the work by time: a bad take is not a defect you
can hear at a glance in a waveform, it is a stretch of audio that has to be
found, compared against the good take next to it, and cut out at both ends
without leaving a click or a hole in the room tone. Over an hour of narration
there are dozens.

Descript solves the general version of this: transcribe the audio, show the
transcript, let the user edit it, propagate edits to the audio. Its retake
feature ("Studio Sound" era, then "shorten word gaps" / "regroup takes") works
by noticing that two nearby stretches of transcript are similar. It has to
work that way, because it does not know what was supposed to be said.

**We do.** That single fact changes almost every decision, and if the design
does not exploit it everywhere then there was no reason to build this instead
of buying Descript.

## What "the script is known" buys

| general transcript editor | this |
| ------------------------- | ---- |
| open-vocabulary ASR; every word is a guess | **forced alignment**: the word sequence is given, only the *times* are unknown, which is a far easier and far more accurate problem |
| errors in the transcript look like errors in the read | the two are separable: a divergence with high acoustic confidence is a reading error, a divergence with low confidence is an alignment failure, and the system can say which |
| a retake is "these two bits of text look similar" — fuzzy, needs a threshold, misses retakes that were reworded | a retake is **a backward jump in script position**. It is a discrete, exactly-defined event on the alignment path, not a similarity score |
| choosing which take to keep is a UI problem | it is a scoring problem with real features: coverage of the script span, divergence count against the *known* text, fluency, lead-in silence |
| the transcript is the deliverable | the **script** is the deliverable, and it is a document the user already owns, with paragraphs, sentences, emphasis and structure that survive the round trip |
| accepting a change is editing a transcript | accepting a read can edit **the script** — the read is often better than the page, and the script converges on what was actually said |

There is a fifth thing that only falls out of knowing the text, and it is
probably the most valuable feature in the whole plan: **punch-in re-recording**.
When a line is unsalvageable, the user selects the script span, records it
again, and the system aligns the new audio to *that exact span* and splices it
in. There is no matching problem, because both sides are anchored to script
positions. In a general transcript editor this is a research problem. Here it
is bookkeeping.

The cost of the constraint is that this tool is useless on unscripted audio,
which is fine — it is not trying to be Descript, it is trying to be better than
Descript at one job.

## Concepts and data model

The types below are proposals in the spirit of `src/listen/types.ts`: plain
data, doc-commented with *why*, serialisable to JSON, shared between the Node
side and the browser. They would live in `src/reader/types.ts`.

The pipeline has four artefacts and it is worth naming them before the types,
because each stage's failure mode is different and mixing them makes the
failures hard to attribute:

```
script.txt  ──normalise──▶  ScriptDoc     (what should be said, tokenised)
take.wav    ──decode────▶  Emissions     (what the acoustics say, per frame)
            ──route─────▶  AlignmentPath (script position as a function of time)
            ──pin───────▶  Passes, Divergences, RetakeGroups
                        ──▶ Edl          (what to render)
```

### The script

```ts
/**
 * The source text, tokenised. `words` is the canonical coordinate system for
 * everything downstream: a "script position" is an index into this array, and
 * every alignment, divergence and retake is expressed in those indices.
 *
 * `raw` and `from`/`to` exist so the editor can render the author's text —
 * punctuation, capitals, line breaks — while alignment works on `spoken`.
 * Losing that mapping means the editing surface is a transcript rather than
 * the user's document, which is most of the point.
 */
export interface ScriptWord {
  index: number;
  /** As authored: "1984", "Dr.", "don't". */
  raw: string;
  /**
   * How it is expected to sound, lowercased, punctuation stripped, numerals
   * and abbreviations expanded. Several tokens when one written word is
   * several spoken ones ("1984" -> ["nineteen","eighty","four"]).
   */
  spoken: string[];
  /** Character offsets into `ScriptDoc.text`. */
  from: number;
  to: number;
  /** Paragraph index, for chunking long documents and for display. */
  para: number;
  /** Sentence index within the document; cut boundaries prefer sentence edges. */
  sentence: number;
  /**
   * This word is inside a k-gram that occurs again within `repeatWindow`
   * words. A "backward jump" that lands here is more likely to be the
   * aligner picking the wrong copy than a real retake, so retake detection
   * charges it extra. See "False positives" below.
   */
  repeatedNearby?: boolean;
}

export interface ScriptDoc {
  id: string;
  title: string;
  /** The text exactly as authored. Never rewritten in place; edits are diffs. */
  text: string;
  words: ScriptWord[];
  /** Applied normalisations, kept so a surprising alignment can be audited. */
  normalisations: { wordIndex: number; rule: string; from: string; to: string }[];
  language: string;
}
```

### The recording and the acoustic evidence

```ts
export interface Recording {
  id: string;
  /** Path relative to the project folder. Audio is never copied or rewritten. */
  file: string;
  sampleRate: number;
  channels: number;
  durationSec: number;
  /** sha256 of the audio data chunk — an EDL referring to changed audio is invalid. */
  audioSha256: string;
}

/**
 * Per-frame character posteriors from the CTC acoustic model, at the model's
 * own frame rate (20 ms for wav2vec2). Cached to disk next to the recording,
 * because computing them is the expensive step and every later pass — a
 * re-alignment after the script is edited, a second opinion on one region —
 * reads the same matrix.
 */
export interface Emissions {
  recordingId: string;
  modelId: string;
  frameSec: number;
  vocab: string[];
  /** [frames][vocab] log-probabilities, stored quantised. */
  logits: Float32Array;
  frames: number;
}
```

### The alignment path

```ts
/**
 * One aligned word: a script position, the audio it occupies, and how sure
 * the acoustics are. `spokenAs` is what the decoder actually heard, which is
 * only interesting when it differs from the script.
 */
export interface AlignedWord {
  scriptIndex: number;
  startSec: number;
  endSec: number;
  /** Mean per-frame log-probability of the aligned span; the confidence. */
  score: number;
  spokenAs?: string;
}

/**
 * The path is *monotone in audio* by construction and NOT monotone in script:
 * every backward step in `scriptIndex` is the signal retake detection reads.
 */
export interface AlignmentPath {
  recordingId: string;
  scriptId: string;
  words: AlignedWord[];
  /** Audio spans matched to nothing in the script (asides, noise, coughs). */
  unaligned: { startSec: number; endSec: number; heard?: string }[];
  /** Overall quality, for deciding whether to trust anything below. */
  meanScore: number;
  modelId: string;
}
```

### Passes, divergences, retakes

```ts
/**
 * A maximal run of the path with no backward jump: one continuous attempt at
 * one stretch of the script. Passes, not takes, are the unit the algorithm
 * works in — "take" is what a group of overlapping passes gets called once we
 * have decided they were attempts at the same thing.
 */
export interface Pass {
  id: string;
  audio: { startSec: number; endSec: number };
  script: { from: number; to: number }; // inclusive-exclusive word indices
  words: AlignedWord[];
  /** Filled in by the scorer; see "Choosing which take to keep". */
  metrics: PassMetrics;
}

export interface PassMetrics {
  /** Fraction of the group's script span this pass covers. */
  coverage: number;
  divergences: { substitution: number; insertion: number; deletion: number };
  /** Filled pauses and false starts per 100 words. */
  disfluencyRate: number;
  /** Words per second, and how far that sits from the reader's own median. */
  rateWps: number;
  rateZ: number;
  /** Silence immediately before the pass (s). A clean restart has a long one. */
  leadInSec: number;
  /** Gated loudness of the pass, and its offset from the file's programme. */
  lufs: number;
  lufsOffset: number;
  /** The last word is cut off mid-articulation by whatever follows. */
  truncatedTail: boolean;
  /** Mean acoustic confidence — a mumbled aside scores low. */
  meanScore: number;
}

export type DivergenceKind =
  | "substitution" // said a different word
  | "insertion" // said a word that is not in the script
  | "deletion"; // skipped a word in the script

export type DivergenceClass =
  | "misreading" // phonetically distant substitution — a real error
  | "minor" // a/the, toward/towards, contraction — usually fine
  | "disfluency" // uh, um, a stuttered repeat
  | "falseStart" // a word begun and abandoned
  | "aside" // an inserted run that is not a disfluency: improvised text
  | "skip" // a deletion run of two or more words
  | "uncertain"; // the acoustics do not support any confident reading

export interface Divergence {
  id: string;
  kind: DivergenceKind;
  klass: DivergenceClass;
  /** Script span involved (empty for an insertion). */
  script: { from: number; to: number };
  audio: { startSec: number; endSec: number };
  /** What was heard, best effort. */
  heard: string;
  /** 0..1. Low means "the aligner is unsure", not "the reader was wrong". */
  confidence: number;
  resolution: "open" | "acceptRead" | "keepScript" | "ignore" | "cut" | "reRecord";
}

/**
 * Passes that overlap in script position: the same material attempted more
 * than once. `keep` is a *cover* rather than a single pass, because a retake
 * often does not span everything the abandoned pass did.
 */
export interface RetakeGroup {
  id: string;
  script: { from: number; to: number };
  passes: Pass[];
  /** Ordered passes (and sub-spans of them) whose concatenation is the keeper. */
  keep: { passId: string; script: { from: number; to: number } }[];
  /** Human-readable justification, shown in the UI verbatim. */
  why: string[];
  confidence: number;
  resolution: "open" | "applied" | "rejected" | "manual";
}
```

### The edit decision list

```ts
/**
 * Non-destructive by construction: the EDL is the only thing edits write to,
 * and rendering it is a pure function of (EDL, source files). Deleting audio
 * is expressing a shorter clip list, never rewriting a WAV.
 */
export interface Clip {
  recordingId: string;
  startSec: number;
  endSec: number;
  /** Static gain, for matching two passes recorded at different distances. */
  gainDb?: number;
  /** Equal-power crossfade into this clip from the previous one. */
  fadeInSec?: number;
  fadeOutSec?: number;
  /** Provenance: which script words this clip is carrying. */
  script?: { from: number; to: number };
}

/** Room tone spliced into a seam, from the level stage's room-tone bed. */
export interface ToneFill {
  kind: "tone";
  durationSec: number;
  gainDb: number;
}

export interface Edl {
  projectId: string;
  items: (({ kind: "clip" } & Clip) | ToneFill)[];
  /** Every edit that produced this list, for undo and for an audit trail. */
  history: EditRecord[];
}

export interface EditRecord {
  at: string;
  /** "cutRetake" | "cutDivergence" | "trimGap" | "punchIn" | "manual" */
  op: string;
  /** What the user or the algorithm was reasoning about. */
  subject: { retakeGroupId?: string; divergenceId?: string; script?: { from: number; to: number } };
  automatic: boolean;
}

export interface Project {
  id: string;
  script: ScriptDoc;
  recordings: Recording[];
  alignments: AlignmentPath[];
  passes: Pass[];
  divergences: Divergence[];
  retakes: RetakeGroup[];
  edl: Edl;
  updatedAt: string;
}
```

Everything except `Emissions` and the audio is small JSON, which means a
project is a folder of text files next to the WAVs — the same shape as
`listening/sessions/<name>/`, diffable, and recoverable when the app changes.

## Alignment: the options, and the one to start with

The whole tool stands on the alignment. Every other feature is a
transformation of the path. So this decision deserves the space.

Requirements, from the repo's existing constraints: **offline**, **local**,
**license-compatible** (weights downloadable and redistributable-in-spirit, or
at least freely usable), **embeddable in Node/Electron without a Python
runtime**, and accurate enough that a cut placed from a word boundary lands in
the gap rather than in a consonant. That last one is quantitative: word
boundaries good to about ±30 ms, because the boundary is only a seed for a
silence search that refines it, but a seed 200 ms wrong searches the wrong
gap entirely.

### Option A — wav2vec2 / CTC forced alignment through ONNX Runtime

Run a character-level CTC acoustic model (wav2vec2-base-960h and friends) over
16 kHz audio, get a `[frames × vocab]` log-posterior matrix at 20 ms, then
Viterbi-align the known character sequence against it. This is the textbook
forced aligner and it is what torchaudio's alignment tutorial does.

- **Quality:** excellent timings. Frame-accurate to 20 ms, which is better than
  anything else on this list, and the confidence it emits is meaningful.
- **Offline / licensing:** `facebook/wav2vec2-base-960h` is Apache-2.0 and
  exports cleanly to ONNX. (`facebook/mms-300m-1130-forced-aligner`, the
  multilingual one everyone reaches for, is **CC-BY-NC** — not usable here.
  See the open questions.)
- **Effort:** *low, for this repo specifically.* The scaffolding already
  exists. `onnxruntime-node` is already an optional dependency,
  `scripts/fetch-model.mjs` already downloads-and-verifies a pinned archive,
  and `src/models/deepfilternet.ts` is already the pattern for "the DSP around
  a bare graph lives in TypeScript and is tested without weights present".
  wav2vec2 needs *far* less around it than DeepFilterNet3 did: resample to
  16 kHz (`src/dsp/resample.ts` exists), normalise the waveform to zero mean
  and unit variance, run, log-softmax, Viterbi. Call it 300 lines including
  the trellis.
- **Cost:** roughly 0.05–0.2× real time on Apple silicon CPU for the base
  model; a 3-hour book is minutes, not hours, and the emissions cache means it
  is paid once.
- **The catch:** plain CTC forced alignment is *total and monotone* — it must
  consume all the audio and all the text, in order. A recording with retakes
  and asides violates both. Feeding it a script it cannot satisfy does not
  produce a nice error; it produces a plausible-looking path that is silently
  wrong for minutes at a time. This is the single biggest technical risk in
  the project and it is why the pipeline below has three passes rather than
  one.

### Option B — Whisper (whisper.cpp) transcription, then text-to-text alignment

Transcribe open-vocabulary, get word timestamps, then align the transcript
word sequence to the script word sequence with Needleman–Wunsch.

- **Quality of the *matching*:** very good, and it handles retakes and asides
  naturally, because sequence alignment over text has no monotonicity problem
  the way acoustic alignment does.
- **Quality of the *timings*:** mediocre. Whisper's segment timestamps drift;
  its word timestamps (whether from token boundaries or the DTW-on-attention
  trick in whisper.cpp) are commonly ±100–200 ms and occasionally much worse.
  Cuts placed from those are cuts placed in the middle of words.
- **A subtler problem, and the reason this is not the primary:** Whisper is
  trained to produce *clean* text. It removes filled pauses, silently repairs
  stutters, and normalises numbers. The disfluencies it deletes are exactly
  the events this tool exists to find. A retake that Whisper transcribes as
  one fluent sentence is a retake this tool cannot see.
- **Offline / licensing:** excellent. whisper.cpp is MIT, the weights are MIT,
  ggml builds are small and there are prebuilt Node bindings. Nothing to argue
  about.
- **Where it earns its place:** *naming insertions*. When the reader improvises
  a sentence that is not in the script, a character-level CTC greedy decode
  produces something phonetically suggestive and orthographically awful
  ("ANDTHENOFCOURSTHEOTHERTHING"). That is enough to know an aside happened
  and where it is — it is not enough to show the user. Running Whisper on
  *just the unaligned spans* — seconds of audio, not hours — turns them into
  readable text for the "accept this read into the script" flow. Optional
  dependency, optional model, degrades to "unrecognised speech, 2.4 s".

### Option C — Montreal Forced Aligner

The best phone-level forced aligner there is, MIT-licensed, well documented,
and completely unshippable here: it is a Python/Kaldi/conda ecosystem with a
pronunciation-dictionary and acoustic-model download story of its own. Making
an Electron app depend on a conda environment is not local-first, it is
someone else's install problem. Worth using **offline, as ground truth**, to
score whatever we do ship — the same role the commercial before/after pair
played for the compressor.

### Option D — Vosk

Apache-2.0, Kaldi under the hood, genuinely offline, small models (~50 MB),
prebuilt Node bindings, per-word timestamps with confidences, and — the
interesting part — a **grammar mode** that constrains decoding to a supplied
word list or phrase set. Constraining the decoder to the script's vocabulary
is a cheap approximation of forced alignment and would sharpen recognition a
lot.

Against it: the small English models are noticeably weaker than wav2vec2, the
timings are word-level with no sub-word detail, the Node binding is a prebuilt
native `.node` (another binary to trust and to ship per-platform), and the
grammar mode is all-or-nothing — constrain to the script and an improvised
aside comes out as the nearest script words, which destroys divergence
detection. Reasonable fallback, wrong foundation.

### Option E — Apple Speech, on macOS

`SFSpeechRecognizer` with `requiresOnDeviceRecognition`, or the newer
`SpeechAnalyzer`/`SpeechTranscriber` API which reports per-token audio ranges.
Free, on-device, no weights to download, no license to read, and
`contextualStrings` biases it toward the script's proper nouns. But it needs a
Swift helper binary in the app bundle, it is macOS-only, the model is a black
box that can change under you between OS releases, and — like Whisper — it
tidies disfluencies. It is a nice *accelerator* on the platform this app
targets first, and a bad thing to build the data model around.

### The recommendation

**Start with A, keep B as an optional second opinion, use C offline as ground
truth.** Concretely: wav2vec2-base-960h through `onnxruntime-node`, fetched by
the existing hash-pinned script, with the classical fallback being "no
alignment, tell the user what is missing" — exactly how the denoiser's ONNX
backend behaves today.

And then solve the monotonicity catch with three passes rather than pretending
it is not there:

#### Pass 1 — decode

Greedy CTC decode over the whole recording. No language model, no script. Out
comes a noisy character stream with a time per character and a confidence.
This is deliberately *unconditioned*: it is the only stage that has not been
told what it is supposed to hear, which makes it the only honest witness when
the read and the script disagree.

#### Pass 2 — route

Align the decoded character stream to the normalised script with a
Needleman–Wunsch variant that permits **backward jumps in the script at a
fixed cost**. Standard NW is monotone in both sequences; adding a jump
transition — from any script position to any earlier one, cost `J` plus a
penalty for how far — is a small change to the recurrence and turns the
alignment into exactly the object we want: *script position as a function of
audio time, allowed to go back*.

Two implementation notes that matter. First, the full jump transition is
O(n²) in script length, which is fine for a chapter and not for a book — so
restrict jump targets to sentence starts and to positions within a window
(retakes back up a phrase or a sentence, essentially never a page), which
makes it linear again. Second, the cost `J` is the single knob that trades
missed retakes against false ones, and it should be *calibrated on the
labelled fixture*, not chosen — the same discipline the rest of this repo
applies to every threshold.

The output is a coarse path: for every audio region, which script region it is
attempting, and where the backward jumps are.

#### Pass 3 — pin

Now that each audio span is paired with a *short, known* script span, run
strict CTC forced alignment inside each pair independently. Short spans mean
the monotone-and-total assumption actually holds, the trellis is small, and
the resulting word boundaries are frame-accurate. Divergences within a span
come out of this stage's own edit script, with per-word confidences.

The three passes decompose the problem along its natural seam: *routing* is a
text problem and *pinning* is an acoustic one, and trying to do both at once
in one trellis is where naive forced alignment falls over.

## Retake detection

### Finding the events

From pass 2, the path is a sequence of (audio time, script position). Split it
at every backward jump: each maximal run with no backward jump is a `Pass`.
A recording of a clean read is one pass. A recording with eleven retakes has
twelve or more.

A backward jump is a *candidate* retake event, characterised by:

- `jumpWords` — how far back it goes. 0–1 words is a false start
  ("the exper— the experiment"), a few words is a phrase retake, a whole
  sentence or more is a restart. These want different default treatments.
- `gapSec` — silence at the jump. Real retakes almost always have one.
- `truncated` — the abandoned pass's last word is cut off mid-articulation.
  This is nearly conclusive evidence, and it comes free from pass 3: the
  forced alignment of the final word runs out of audio before it runs out of
  phonemes.
- `marker` — an explicit spoken marker in the gap ("sorry", "again", a tongue
  click, a clap). Cheap to spot with a small keyword list against the greedy
  decode, and worth a lot of confidence when present.

### Grouping passes into takes

Two passes are attempts at the same material when their script ranges overlap.
Build an interval graph over passes — edge when the overlap exceeds a fraction
of the shorter range (start at 0.3) — and take connected components. Each
component is a `RetakeGroup` whose script span is the union.

Connected components rather than pairwise matching is the right structure
because retakes chain: pass 1 covers words 100–160, pass 2 backs up to 140 and
runs to 175, pass 3 backs up to 168. No two of those look like "the same take"
pairwise across the whole group, but they are one editing decision.

### Choosing what to keep

The default is **the last complete pass**, and it is worth being explicit about
why, because "score all the takes and pick the best" is the tempting design and
it is wrong. The reader stopped re-reading when he was satisfied. That is a
judgement made in the room, with the text in front of him, by the person whose
recording it is. A scorer that overrules it because take 2 was 0.3 dB more
consistent is second-guessing the only ground truth available. "Complete" here
means: covers the group's script span to its right edge, and continues past it
into the following material without another backward jump.

The scores exist for the cases where that rule does not decide — and to
*explain*, which is most of their value. Every group shows its reasoning
verbatim in the UI ("kept pass 3 of 3 — last complete pass; 0 divergences;
1.4 s lead-in; level within 0.3 dB of programme"). The features are the
`PassMetrics` above:

- **coverage** — a pass that does not reach the end of the span cannot be the
  sole keeper.
- **divergences** — count against the *known* script, which is the measure a
  general editor cannot compute.
- **disfluency rate and false starts** — from the divergence classifier.
- **rate z-score** — against the reader's own median words-per-second in this
  recording. Both much slower and much faster than usual are bad signs; a
  careful re-read of a hard sentence is a genuine exception, which is why this
  is a soft feature and not a rule.
- **lead-in silence** — a long pause before a pass means a deliberate restart,
  and separately means there is somewhere clean to cut.
- **loudness offset** — measured with the existing BS.1770 machinery in
  `src/dsp/loudness.ts`. A pass 6 dB below programme is usually the reader
  muttering to himself, not a take.
- **truncated tail** — a pass whose last word is chopped is not a candidate
  keeper for the region containing that word.
- **mean acoustic confidence** — mumbling, turning away from the mic.

### Nested and partial retakes

A group's passes rarely cover the same span, so "keep pass 3" is often wrong in
a way that quietly deletes material: pass 1 covered 100–160, pass 3 only
150–175, and keeping pass 3 alone loses fifty words.

So the keeper is a **cover**, computed as a small dynamic program over script
word index across the group span:

```
best[i] = min over passes p covering i, with p.script.from <= i:
            best[p.script.from] + passPenalty(p) + spliceCost(p, previous, i)
```

where `passPenalty` is the weighted score above (later passes cheaper, so the
"last complete pass" default falls out as the degenerate case) and
`spliceCost(a, b, i)` is the acoustic cost of joining pass `a` to pass `b` at
script position `i` — cheap where both sides have silence around word `i`,
expensive mid-phrase, expensive when the two sides' levels or noise floors
differ. That last term is what stops the DP from producing an optimal-on-paper
Frankenstein splice in the middle of a word.

The output is `RetakeGroup.keep`: an ordered list of (pass, script sub-span).
Usually one entry. Sometimes two, and when it is two the UI has to say so
loudly, because a stitched take is the thing most likely to sound wrong.

### False positives, which are the whole risk

Auto-cutting is only acceptable if it is nearly never wrong, so the failure
modes deserve naming.

**Deliberate repetition in the source text.** "Very, very good." A refrain. A
list with an anaphora — "We will fight on the beaches, we will fight on the
landing grounds". Read correctly, these produce audio that legitimately
matches an earlier script position, and a naive backward-jump detector reports
a retake and cuts the rhetoric out of the speech. Three defences, all needed:

1. **Occam in the alignment.** `ScriptWord.repeatedNearby` is precomputed from
   the script alone: does this word's k-gram (k = 3 or so) occur again within
   a window? When a candidate backward jump lands in a repeated region and the
   forward continuation is consistent with *not* jumping, the no-jump
   interpretation is cheaper. This is a term in the pass-2 cost, not a
   post-filter, which matters — a post-filter has already lost the alternative
   path.
2. **Require acoustic evidence.** A genuine retake has a discontinuity. Demand
   at least one of: ≥250 ms of silence at the jump, a truncated final word, a
   filled pause, a spoken marker, or a level/pitch step across the join. A
   backward jump with none of those, in a region the script itself repeats, is
   reported as an anaphora, not a retake.
3. **Never auto-apply below a confidence threshold.** The review queue is the
   product. A tool that silently deletes a line of a book is worse than no
   tool, and the asymmetry is total: a missed retake costs a minute of manual
   editing, a wrong cut costs trust and possibly goes unnoticed into a
   published file.

**Alignment failure looking like a retake.** A stretch of bad audio (a truck,
a coughing fit) can drag the path anywhere. The defence is `meanScore`:
divergences and jumps inside a low-confidence region are labelled `uncertain`
and shown as "could not follow the script here", never as a reading error and
never as an auto-cut candidate.

**The reader reading a passage twice on purpose** — reading a quotation once
for context and once for emphasis, or recording alternate takes for later
choice. Undetectable in principle. It is a UI problem: reject the group, and
optionally mark the script span "expects repetition" so it stays rejected
after a re-align.

## Divergence highlighting

Pass 3 produces an edit script per aligned span; each operation becomes a
`Divergence`. Classification is a small rule cascade, and the rules earn their
keep by making the list *filterable* — a hundred raw divergences is noise, six
misreadings and two skips is a work list.

- **insertion** whose text is in the filled-pause set (`uh`, `um`, `er`, `mm`,
  a tongue click) → `disfluency`.
- **insertion** that is a prefix of the following script word → `falseStart`.
- **insertion** run of ≥ 3 words that is neither → `aside`. These are where
  Whisper gets called to produce readable text.
- **substitution** whose spoken form is phonetically close to the script word
  → `minor`. Phonetic distance without a pronunciation dictionary is a
  problem; a workable proxy is edit distance over a Soundex/Metaphone-style
  reduction plus a small hand-written equivalence list (a/the, toward/towards,
  ’s elisions, contraction expansions). Honest about it: this is the weakest
  rule here, and it is the one that decides how noisy the default view is.
- **substitution** otherwise → `misreading`.
- **deletion** run of ≥ 2 words → `skip`; a single deleted function word →
  `minor`.
- anything with confidence below threshold → `uncertain`, regardless.

**Confidence** is the mean per-frame log-probability over the divergence's
audio span, calibrated against the recording's own distribution (a
consistently quiet recording should not have every divergence marked
uncertain). The second signal is *margin*: how much better the reported
reading is than the script's own reading of the same audio. A substitution
where the script word scores almost as well is a coin flip and should say so.

### Presenting them

The script is the document; divergences are annotations on it, not a separate
list that happens to be sorted by time. Rendering:

- **read** words are plain, with a faint "recorded" tint so unrecorded material
  is visibly different.
- **substitution** — the script word underlined, the spoken word shown as a
  ghost above it. The ghost is the important part: the user needs to compare,
  not to be told there was a difference.
- **insertion** — a caret between words, expanding to the heard text on hover
  or selection.
- **deletion** — the script word struck through and greyed.
- **uncertain** — a dotted underline in a different colour entirely, because it
  is a statement about the tool, not about the read.

Four resolutions, all one keystroke:

| action | key | meaning |
| ------ | --- | ------- |
| **Accept the read** | `a` | The spoken version is fine — and, for substitutions and asides, *the script is updated to match*. The document converges on what was actually said, which is what a narrator wants when the line came out better than it was written. Recorded as a diff against `ScriptDoc.text`, never an in-place mutation, so the original is always recoverable. |
| **Keep the script** | `s` | The read is wrong and the text is right. Cutting cannot fix this, so it goes on the **punch-list**: a list of script spans to re-record. In v1 this is where punch-in recording starts. |
| **Ignore** | `i` | Cosmetic. Suppressed, and the classifier's threshold for that class can learn from it. |
| **Cut** | `x` | For insertions only — remove the audio. The default action for `disfluency` and `falseStart`. |

The punch-list is worth its own screen even in v0, because it is the output
that saves the most time: before an editor exists, a reader who gets a list of
"these eleven spans need re-recording, here they are with 3 s of context" has
already got most of the value.

## Cut mechanics

Two independent concerns: *where* to cut, and *how* to represent the cut.

### Where

The aligner's word boundary is a **seed, not a cut point**. It is accurate to
about 20 ms and it sits at the acoustic onset, which is inside the breath and
the coarticulation, not in the gap. Four refinements, in order:

1. **Silence search.** Around the seed, find the quietest point using a
   short-window loudness scan. `src/dsp/silence.ts` already does this — but its
   defaults (`windowSec: 0.1`, `hopSec: 0.025`, `minSilenceSec: 1.0`) are tuned
   for finding the *pauses between paragraphs* that segment levelling needs.
   Inter-word gaps are 50–200 ms. The scan wants `windowSec: 0.02`,
   `hopSec: 0.005` and no minimum-duration filter. That is the same code with
   different parameters, which is a good sign the abstraction is right, but it
   is worth stating that reusing `analyzeSilence` at its defaults here would
   simply find nothing.
2. **Sentence and phrase preference.** Given several acceptable points, prefer
   the one at a sentence boundary in the script; that is where a listener
   expects a discontinuity and will not notice one.
3. **Zero crossing.** Within ±2 ms of the chosen point, snap to a zero crossing
   with the same slope sign on both sides of the join. Free, and it removes the
   step that a crossfade would otherwise have to hide.
4. **Crossfade.** Equal-power, 5–15 ms — short enough not to smear a consonant,
   long enough to kill the click. `src/dsp/roomtone.ts` already builds
   equal-power crossfades (at 50 ms, for concatenating tone clips); the same
   helper, shorter.

### The two things that make an edit *sound* edited

**The room disappears.** Cutting a retake removes the words and the room
underneath them. If the two sides have even slightly different noise floors —
and they will, because the reader moved between takes — the join is an audible
step in the background. Defence: measure the floor on both sides (the
`scoreRange` machinery in `roomtone.ts` is already a "how clean is this range"
function), and when they differ by more than ~1.5 dB, or when the seam needs
to be lengthened, splice in a gain-matched patch from the room-tone bed the
level stage already builds. This is the one place where the existing room-tone
feature stops being a bonus output and becomes load-bearing.

**The rhythm goes wrong.** This is the bigger one and it is what makes most
Descript edits identifiable. Cut a retake and the gap between the preceding
and following words is now whatever the two cut points happened to leave —
usually far too short, occasionally far too long. Speech has a rhythm and the
ear is extremely good at it.

Knowing the script makes the fix easy: measure this reader's own gap
distribution in this recording, conditioned on the *syntactic* boundary the
script provides — within a phrase, at a comma, at a sentence end, at a
paragraph end. Then set the seam's gap to the median for that boundary class,
padding with room tone or trimming silence. A general transcript editor can
only guess at the boundary class; here it is in the source text.

### How

Everything is an `Edl`. Rendering is a pure function of (EDL, sources) and
happens on export. Consequences worth stating:

- **Nothing is destroyed.** Rejecting a retake decision six edits later is a
  list operation.
- **The mastering chain runs after the EDL renders**, not before, and this is
  the right order. The leveller measures per-segment loudness and segments the
  file at silence midpoints — segmentation of an *unedited* file describes a
  file that will not exist. So: render the EDL to `<name>_edited.wav`, then run
  the existing eight-stage chain on that, producing `<name>_edited_processed.wav`
  and its room-tone bed. Two files, two clearly separable failure modes.
- One future refinement, noted and deliberately not done in v0: passing the
  seam positions *into* the chain so silence detection can be told "this
  200 ms gap is a splice, not a pause". Until then, a splice that creates a
  gap longer than `minSilenceSec` will become a segment boundary, which is
  harmless but means the two sides get levelled independently. Actually
  usually a feature.
- **Gain matching before crossfading.** Two passes recorded minutes apart can
  differ by 1–2 dB. `Clip.gainDb` carries a static match derived from the two
  passes' measured loudness, applied before the fade, so the crossfade is
  between two things at the same level.

## UI and UX

### The main editor

One surface, and it is **the script**, not a transcript. Full-width reading
column, the user's own paragraphs and punctuation, set in a text face at a
readable size. Words carry state as described above. This is the difference
between a tool that feels like an editor and one that feels like a transcript
viewer, and it is worth the layout work.

Below it, pinned, a **transport strip**:

- The waveform of the whole recording, drawn from a multi-resolution peak
  cache — `listen/src/peaks.ts` already exists and already handles a
  20-minute file smoothly.
- Above the waveform, the **alignment ribbon**: script position plotted
  against audio time. A clean read is a rising staircase. Every retake is a
  visible step *backwards*. This is the single most useful picture in the
  application: it shows the shape of the session at a glance, where the hard
  passages were, how many attempts each took, and where the tool is confused
  (the path goes flat or erratic). It is also, pleasingly, a direct rendering
  of the data model rather than a designed visualisation.
- Cut regions shown as hatched spans that collapse when the edit is applied.

Right rail: the **issue list** — divergences and retake groups interleaved in
recording order, filterable by class, each row showing its confidence. This is
the work queue and the keyboard drives it.

### Keyboard

The workflow is "listen, decide, next", hundreds of times. It has to be
keyboard-complete, and the annotator at `/annotate` has already established
the house style (single letters, `⏎` to commit, `⇥` to step).

| key | action |
| --- | ------ |
| `space` | play/pause from the cursor; with an issue selected, play it with 1.5 s of pre-roll and post-roll |
| `j` / `k` | next / previous issue |
| `⇥` / `⇧⇥` | next / previous unresolved issue (skips what is decided) |
| `a` | accept the read (and update the script) |
| `s` | keep the script — add to the punch-list |
| `i` | ignore |
| `x` | cut |
| `1`–`9` | in a retake group, make take *n* the keeper |
| `⏎` | apply the group's decision |
| `⇧⏎` | apply every remaining high-confidence decision |
| `z` / `⇧z` | undo / redo |
| `⌥←` / `⌥→` | move the audio cursor a word |
| `/` | filter the issue list |

### Retake review

A dedicated screen, because it is a comparison task and the main editor's
one-column-of-text layout is wrong for it.

One card per group. Inside the card, a shared horizontal axis of **script
position** (not time — that is the whole trick, and it is why the takes line
up visually at all), with one lane per pass. Each lane shows its waveform
thumbnail, its duration, its divergence chips against the script, and its
score reasons. The keeper is highlighted; clicking a lane makes it the keeper
and the "why" text updates to explain the new choice, including when it
disagrees with the default.

Below, two things: the resulting text with the cut applied, and a **preview
join** button that plays exactly the 3 seconds around the splice. Judging a
cut requires hearing the cut, not the take.

Two affordances that matter more than they look:

- **Explain the default in words, always.** "Last complete pass" is a
  defensible reason. A number between 0 and 1 is not.
- **Bulk apply, with a floor.** "Apply all above 0.9 confidence" turns forty
  retakes into four reviews, which is the difference between a tool that gets
  used and one that does not. It must never be the *only* path, and the floor
  must be visible and adjustable.

## Delivery

### v0 — the reading report (read-only)

**Nothing is edited. Nothing is cut. Nothing is rendered.** v0 aligns a
recording to a script and tells the user what happened, and that alone is
worth having: a punch-list of spans to re-record, and a map of where the
retakes are, is most of a session's editing decisions made.

- `src/reader/` — normalisation, the three-pass aligner, pass splitting,
  divergence classification, retake grouping and scoring. Pure, no Electron,
  unit-tested, like `src/dsp/`.
- `pnpm align script.txt take.wav --report out.json` — a CLI, in the shape of
  the existing one, printing what it found and how sure it is.
- A `/read` route in `listen/`, reusing `peaks.ts` and `player.ts`: the script
  with divergences highlighted, the alignment ribbon, the issue list, the
  punch-list. Aqua-styled, same as everything else there.
- Model fetching through `scripts/fetch-model.mjs` with a pinned hash, and the
  same "say exactly what is missing and decline" behaviour the ONNX denoise
  backend has.

Read-only is a deliberate constraint, not a lack of ambition. It means v0 can
be wrong without costing anything, which is the only way to find out how good
the alignment actually is.

**Build this first, before any of the above: the labelled fixture.** Record
one 10–15 minute read of a known script with retakes and misreadings made
*deliberately and logged as they happen*. Annotate it in the existing
`/annotate` editor. Then bound alignment quality in `eval/` the way every
other stage in this repo is bounded — word-boundary error in ms, retake
recall, retake precision, divergence F1 — with the bounds stated as reasons,
not snapshots. Two things follow from the repo's own history here: the
synthetic corpus cannot evaluate this (a trained model asked whether it is
hearing a voice will say no about synthetic speech, and it will say something
even less useful about synthetic *reading*), and a metric written after the
fact tends to be a metric that flatters the implementation. The fixture is a
day of work and it decides whether option A survives contact.

### v0.5 — cut the retakes

Auto-detect, review, apply, export. The EDL, the cut refinement, the
crossfades, the room-tone seams, the gap-rhythm fix, undo/redo. Output is
`<name>_edited.wav`, which the existing chain then processes unchanged.

The success criterion is not "it cuts" — it is a blind listening test, using
the app that already exists: retake-cut renders against manual edits of the
same material, scored for audible splices. `listening/specs/` is already the
right shape for that, and it will find the room-tone and rhythm bugs that no
metric will.

### v1 — text editing edits audio

Deleting words in the script deletes the audio. Accepting a read rewrites the
script. And **punch-in recording**: select a script span, record it, the new
audio is aligned to that exact span and spliced with the same cut machinery.
Needs audio input in the Electron app, which does not exist yet, and a way to
match the new recording's level and tone to the old one — where the existing
EQ stage's long-term-average-spectrum fitter is suddenly the right tool for a
completely different job (fit the new take's spectrum to the old take's).

### v2 — projects

A book is many chapters, many sessions, many recordings, recorded over weeks
with different room tone and different distances to the microphone. Chapter
assembly, per-chapter export through the chain, consistency checks across
sessions, breath-aware cuts once the breath detector the annotator is
collecting data for exists, Whisper for naming asides, and a
reader-specific pronunciation lexicon built from confirmed alignments — which
makes the phonetic-distance rule in the divergence classifier, the weakest
part of this plan, considerably less weak.

### The de-risking spike, in order

1. Record and label the fixture. (Half a day. Do it first.)
2. Export wav2vec2-base-960h to ONNX; run it from Node on 60 s of the fixture;
   print per-word timings from a plain forced alignment against the true text
   of that 60 s. Compare against hand-marked boundaries. **If the median
   boundary error is not under ~30 ms, stop and reconsider the whole option.**
3. Greedy decode plus the jump-permitting alignment over a 5-minute stretch
   with known retakes. Measure recall and precision of the backward jumps.
   Calibrate `J`.
4. Only then build anything with a UI.

Steps 2 and 3 are a few days and they settle the largest risk in the project.
Everything downstream — the DP over covers, the cut refinement, the editor —
is ordinary work whose difficulty is known.

## Open questions and risks

**Language.** wav2vec2-base-960h is English-only, and the obvious multilingual
forced-alignment model (`mms-300m-1130-forced-aligner`) is CC-BY-NC, which
this project cannot use. If German narration is in scope, the alignment
recommendation needs revisiting *before* anything is built — the candidates
would be a German wav2vec2 fine-tune with a permissive licence (several exist,
of varying quality), Whisper (multilingual, MIT, but with the timing problems
above), or Vosk's German model. This is the question with the largest blast
radius on the plan.

**Model size and cost over a book.** A 10-hour audiobook is 10 hours of
inference plus re-alignments after script edits. The emissions cache makes
re-alignment nearly free, but the first pass is not, and the UI needs to be
honest about it (a progress report per chapter, resumable).

**What is a script, as a file?** Plain text is the easy answer and probably the
right v0 answer. Markdown, EPUB and Word all carry structure worth keeping
(chapters, emphasis, footnotes that must *not* be read) and all need a parser.
Related: does the reader want the tool to edit the script file in place when
he accepts a read, or to emit a diff? If the script is a book with a publisher,
in-place is wrong.

**Text normalisation is unglamorous and load-bearing.** "1984", "Dr.", "£12.50",
"i.e.", "3–4". Every one of those, unexpanded, becomes a false divergence, and
false divergences in the first ten minutes of use are how a tool loses its
user. There is no dependency to add here (the constraint says none), so it is
hand-written rules, per language, and it should be tested as its own unit.

**Where does the app live?** v0 as a `/read` route in `listen/` is much the
fastest path and reuses the peak cache, the player and the Aqua chrome. But
`listen/` is a dev tool served by the Vite dev server, and this is a product.
The v1 decision — promote it into the Electron app, or promote `listen/` into
something shippable — should be made consciously rather than by drift.

**How much automation does Nino actually want?** The plan assumes review-first
with opt-in bulk apply. If the real preference is "cut everything and let me
listen to the result", the confidence calibration matters much more and the
review UI matters much less. Worth deciding early; it changes what v0.5 is.

**The asymmetry of a wrong cut.** Stated once more because it should govern
every threshold: a missed retake costs a minute of manual work, a wrong cut
can silently remove a sentence from a published recording. Every default
should be biased accordingly, and the eval bounds should weight precision
above recall.

**Punch-in matching.** Re-recorded audio weeks later will not match — room,
distance, voice, time of day. Splicing it in is easy; making it inaudible is
the hard part, and it may turn out that punch-in only works within a session.

**Multi-take sessions.** Some readers record a whole chapter twice rather than
retaking phrases. That is a different problem (align two full recordings to
one script and choose per sentence) which this data model actually supports —
`Project.recordings` is a list and the DP over covers does not care which
recording a pass came from — but it is not v0, and pretending otherwise would
be scope creep.
