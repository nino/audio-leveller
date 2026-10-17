# Audio Leveller

A local, Auphonic-style processing chain for spoken-word recordings: drop a
`.wav` on the window and it writes `<name>_processed.wav` and
`<name>_roomtone.wav` next to it. Nothing is uploaded. The README is the long
explanation of every stage; `docs/rust/architecture.md` maps the Rust crates
and the decisions behind them.

## Where the project is

The tree holds several generations of the app. Know which one you are changing.

- **Rust (`crates/`, `apps/`, `xtask/`)** is the current implementation and
  what ships. New DSP and pipeline work goes here.
- **Tauri (`apps/leveller-tauri` + `ui/`)** is replacing the AppKit interface.
  New UI work goes here, not into AppKit. The frontend is React + TypeScript
  built with **Bun** (not Vite, not pnpm). App state stays in Rust:
  `leveller-ui`'s `Model` decides what each input means, and the webview draws
  the `View` it serialises (`crates/leveller-ui/src/view.rs`).
- **AppKit (`apps/audio-leveller`, `apps/listen`, `crates/aqua`)** still builds
  and ships until the Tauri apps reach parity, then gets deleted. Fix bugs
  there only when asked.
- **TypeScript/Electron (`src/`, `test/`, `listen/`, the root `package.json`)**
  is the original implementation. It is no longer released but CI still runs
  its tests and eval. Leave it alone unless the task is about it.
- **`reaper/`** is a standalone Lua ReaScript with its own test.

## Rust layout

Dependencies point strictly downward: `leveller-dsp` depends on nothing in the
workspace, `leveller-ui` never mentions AppKit or Tauri, and `aqua` never
mentions a stage.

| crate | what it holds |
| ----- | ------------- |
| `leveller-dsp` | the maths: no IO, no platform |
| `leveller-wav` | RIFF/WAVE read and write, byte-exact on a round trip |
| `leveller-pipeline` | `Signal`, the `Stage` trait, the runner, the memoised `Analyzer` |
| `leveller-stages` | the eight stages, `DEFAULT_CHAIN`, the parameter schema, presets |
| `leveller-io` | read a file, run the chain, write the outputs |
| `leveller-model` | DeepFilterNet3 on `tract` (pure Rust, no ONNX Runtime) |
| `leveller-corpus` / `leveller-eval` | synthetic test material, and the evaluation harness |
| `leveller-listen` / `leveller-audio` | listening-test data model, and the gapless player |
| `leveller-ui` | each app as state and messages, with no window |
| `apps/leveller-cli` | two binaries: `audio-leveller` (the CLI) and `leveller-eval` |

The chain order in `DEFAULT_CHAIN` is deliberate (declick → denoise → dereverb
→ eq → dyneq → expand → compress → level); the module docs in
`crates/leveller-stages/src/lib.rs` give the reasons. Don't reorder it without
a reason that beats those.

## Commands

| area | checks |
| ---- | ------ |
| Rust | `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` |
| Tauri frontend | `bun install`, `bun run typecheck`, `bun test`, all from `ui/` |
| Node pipeline | `pnpm install`, `pnpm typecheck`, `pnpm test` |
| REAPER | `lua reaper/test_dsp.lua` |

The full Rust workspace only builds on macOS: the AppKit crates need it, and on
Linux the Tauri crate needs GTK and `leveller-audio` needs ALSA. On a Linux
machine (including cloud agent sessions) run the checks on everything else:

```bash
EXCL="--exclude audio-leveller-app --exclude listen-app --exclude aqua --exclude audio-leveller-tauri --exclude leveller-audio"
cargo clippy --workspace --all-targets $EXCL -- -D warnings
cargo test --workspace $EXCL
```

CI runs the whole workspace on macOS, so it still catches what that skips.
`audio-leveller-tauri` embeds `ui/dist` at compile time, so run `bun run build`
in `ui/` before building it.

Other commands:

```bash
cargo run --release --bin audio-leveller -- <file.wav>        # process a file and print what each stage decided
cargo run --release --bin leveller-eval -- --baseline eval/baseline.json   # the evaluation corpus
cargo xtask fetch-model       # DeepFilterNet3 weights into ~/.audio-leveller/models
cargo xtask fetch-fixtures    # pinned real recordings into eval/fixtures
cd ui && bun run tauri dev    # the Tauri window with hot reload
```

Dev builds compile dependencies at `opt-level = 2` because the DSP is unusably
slow otherwise; run the eval in `--release`.

## The evaluation harness

`leveller-eval` runs a seeded synthetic corpus and fails when a bound breaks.
Each case states bounds *with reasons*, not snapshots of current output. When a
change moves the audio:

- Run the eval against `eval/baseline.json` and report what moved in the PR.
- Only update the baseline (`--save-baseline eval/baseline.json`) when the
  movement is intended, and say so in the PR.
- Never loosen a bound to make a run pass. If a bound is wrong, change it and
  its stated reason together.
- Cases that need model weights or fixtures report as **skipped**, not passed.
  Don't treat a skip as a pass.

`eval/fixtures/` and `eval/references/` are git-ignored apart from their
READMEs and the manifest; never commit recordings.

## Conventions

- **Commit messages** are one plain sentence in the imperative, sentence case,
  no prefix, no trailing full stop: "Port the dynamic EQ", "Ask the model
  backend about the fixture's rate, not a hard-coded one".
- **Comments and docs explain why**, in full sentences: the decision, the
  alternative that was rejected, and what would break. Match the existing tone
  of module docs. Don't add comments that restate the code.
- **Stages decline when they have nothing to do** and say so in their report.
  A new stage needs that decision, a report, and eval cases with bounds.
- **DSP stays testable without a disk or a device.** Keep IO in `leveller-io`
  and platform code in the apps.
- **Bit-identity**: a fully bypassed chain must return the input
  byte-for-byte. Don't resample or re-encode on a path that doesn't need it.

## Pull requests

Before creating any pull request, or marking a draft ready for review, follow
the `pre-pr` skill (`.claude/skills/pre-pr/SKILL.md`): run the checks for the
code you touched, run `/code-review` on the branch diff, and fix what it finds.
