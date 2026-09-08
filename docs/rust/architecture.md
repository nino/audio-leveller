# The Rust rewrite: how it is put together

The project is being rewritten from TypeScript/Electron to Rust with a native
AppKit interface. This is the map of where things live and, where a decision
could have gone either way, why it went the way it did.

## Crates

```
crates/
  leveller-dsp       the maths. No IO, no platform, no allocation policy beyond
                     "return what you made". Everything else depends on it.
  leveller-wav       RIFF/WAVE, hand-rolled, because re-encoding a file must
                     give back the bytes that came in.
  leveller-pipeline  Signal, the Stage trait, the runner, the memoised Analyzer.
  leveller-stages    the eight stages, the chain, the parameter schema, presets.
  leveller-model     DeepFilterNet3's frontend and the ONNX session behind it.
  leveller-io        reading a file, running the chain, writing the results.
  leveller-eval      the synthetic corpus, the metrics, the bounds.
  leveller-listen    the listening-test data model, sessions, annotations, peaks.
  aqua               the Aqua look: palette, drawing primitives, NSCell
                     subclasses, the window. macOS only.
  leveller-ui        what each app *is*, as state and messages, with no window
                     in sight — so it can be tested without one, and so a GTK
                     or Win32 shell can be added without touching it.
apps/
  audio-leveller     the drag-and-drop leveller. Ships as an .app bundle.
  listen             the blind listening-test and annotation app.
  leveller-cli       the command line, and the evaluation harness.
```

Dependencies point strictly downward. `leveller-ui` never mentions AppKit;
`aqua` never mentions a stage or a pipeline.

## Decisions

**rustfft rather than the hand-rolled transform.** The TypeScript carries its
own mixed-radix FFT, restricted to prime factors of 5 and below. rustfft has no
such restriction, is considerably faster, and is checked here against a direct
DFT — which is the only honest reference for a transform anyway. Bit-identity
with V8's arithmetic is not a goal; agreement with the definition is.

**Stock NSControls with custom NSCell subclasses, not a hand-rolled widget
tree.** Overriding `drawBezelWithFrame:inView:` on an `NSButtonCell` replaces
the pixels and keeps everything else: the key-view loop, key equivalents,
Space and Return, the accessibility role, label, value and actions, the focus
ring. A custom view hierarchy would mean rewriting all of that by hand and
getting the accessibility subtly wrong. Custom views are for the things AppKit
has no equivalent of — the brushed-metal ground, the gel traffic lights, the
waveform.

**A real titled NSWindow with a painted title bar.** Not a borderless window:
the system keeps drawing the shadow, the corner mask, the resize edges, the
Mission Control thumbnail and the window's accessibility element. The title bar
is `titlebarAppearsTransparent` with the standard buttons hidden and a 28pt
view of our own on top — which is exactly what the Electron version does, so
the look is known to come out right.

**Platform isolation lives at the model boundary, not behind a drawing trait.**
`leveller-ui` holds the state and the messages; the platform crate builds
native controls from it and sends messages back. A drawing abstraction would
force every backend through the lowest common denominator and would have made
the AppKit path worse to make a hypothetical GTK path possible.
