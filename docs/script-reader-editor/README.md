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
  delivery plan with a de-risking spike. Its **Decisions** section records
  the answers to the original open questions (2026-09-27).

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
- **[mockups/retakes.html](mockups/retakes.html)** — the retake review, as
  take comping. One card per group: the script words as a ruler, a comp lane
  showing which pass supplies each word, and one lane per pass, all aligned
  word by word rather than by time. Each group opens with the suggested comp
  (a small version of the plan's cover DP, running on the fake data), with
  the reasoning for each segment and each join written out. Drag across a
  lane to take those words from it; click a pass name to take all of it;
  click or <kbd>⇧</kbd>-click ruler words and press <kbd>1</kbd>–<kbd>9</kbd>;
  click a join and move it with <kbd>⌥←</kbd>/<kbd>⌥→</kbd>. Group 1 is the
  "whole sentence three times, then the second half three more" pattern;
  bulk apply skips stitched comps.

Styling follows `listen/src/aqua.css` from before the Rust rewrite (now the
`aqua` crate), so the screens read as part of the same application. The
mockups are design references only; no code from them carries over to the
AppKit app.
