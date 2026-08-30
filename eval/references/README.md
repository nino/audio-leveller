# Reference masters

Real recordings, pinned by checksum in `manifest.json` and fetched with:

```bash
pnpm fetch-fixtures
```

The audio itself is not in the repository. It is hundreds of megabytes and it is
someone's actual voice, so what is tracked is the manifest: a URL and a SHA-256
per file. A fixture whose bytes can change without anyone noticing is worthless
as a regression baseline – "it sounded better before" is not something you can
check against a moving target. The fetcher refuses to install anything whose
hash does not match, and refuses to overwrite a local file that has drifted.

## Roles

The manifest gives every file a role, because the harness must treat them
differently:

| role | lands in | what it is |
| ---- | -------- | ---------- |
| `input` | `eval/fixtures/` | Unmastered material. Enrolled automatically as `fixture:<name>` cases. |
| `reference` | `eval/references/` | Somebody else's master of that same material. Never processed. |
| `archive` | `eval/references/` | Output from a past build, kept because it can no longer be regenerated. |

References deliberately do **not** land in `eval/fixtures/`. That directory is
scanned and everything in it is run through the chain, so a reference dropped
there would have Auphonic's master mastered a second time, and the resulting
numbers would mean nothing.

## The `chili-15` set

A 19-minute excerpt from a solo audiobook narration, in four versions: the raw
edit that the chain is given, Auphonic's master, a hand-built REAPER master, and
this chain's own output from before the de-clicker's pulse-train veto. One
voice, close-miked in an untreated room – the material this tool actually
exists to master, which is why it is the set worth pinning.

Auphonic's is the one to beat – it was preferred over both the DAW master and
ours in blind listening. The pre-veto output is kept as a floor rather than a
target: it is the audible record of the de-clicker repairing roughly nineteen
glottal pulses a second, and it cannot be regenerated now that the bug is
fixed.

## What is not here yet

Nothing in the harness reads the references automatically. Comparing our output
to a reference master needs a metric that says something meaningful about two
different masters of the same source – spectral distance, loudness and dynamics
statistics, or a perceptual measure – and choosing one is its own piece of work.
For now the value is that the files are pinned, so a future comparison has a
fixed thing to compare against, and the blind listening-test app can build
sessions from them by hand.
