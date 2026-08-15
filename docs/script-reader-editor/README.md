# Script-reader editor — design notes

Planning material for a possible future feature area: **editing audio by
editing its transcript, for the case where the reader is reading a known
source text**. Nothing here is implemented; this folder is documentation and
mockups only.

The premise is that knowing the script in advance turns three hard problems
into easy ones — transcription becomes forced alignment, "find the retakes"
becomes "find the backward jumps in the alignment path", and "which take is
best" becomes a scoring problem with real features. A tool that does not
exploit that everywhere has no reason to exist next to Descript.

- **[PLAN.md](PLAN.md)** — the plan. Problem statement, data model, the
  alignment options with their trade-offs and the recommendation, the retake
  algorithm, divergence classification, cut mechanics, UI, and a staged
  delivery plan with a de-risking spike.

## Mockups

Static HTML, no external requests, no build step — open them directly in a
browser. Fake data throughout, but the interactions that carry the idea work.

- **[mockups/editor.html](mockups/editor.html)** — the main editing surface.
  The script as a document with divergences marked inline, the alignment
  ribbon (script position against time; retakes are visible steps backwards),
  the waveform with cut regions, and the keyboard-driven issue rail. Click a
  marked word or an issue row to select it; <kbd>j</kbd>/<kbd>k</kbd> to move,
  <kbd>a</kbd>/<kbd>s</kbd>/<kbd>i</kbd>/<kbd>x</kbd> to resolve,
  <kbd>space</kbd> to play.
- **[mockups/retakes.html](mockups/retakes.html)** — the retake review. One
  card per group, passes as lanes over a shared *script-position* axis, the
  keeper highlighted with its reasoning written out in words. Click a lane to
  change which take is kept and watch the reasoning and the resulting text
  update; group 2 demonstrates a stitched cover, where the kept pass does not
  span everything the abandoned one did.

Styling follows `listen/src/aqua.css` so the screens read as part of the same
application.
