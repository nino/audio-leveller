---
name: pre-pr
description: Run before creating or publishing a pull request in this repo (including marking a draft ready). Runs the local checks and a /code-review of the branch diff, and fixes what it finds, before the PR goes up.
---

# Before you open a PR

Do these steps, in order, every time you are about to create a pull request or
mark a draft as ready for review. A PR that goes up without them costs a review
round that the steps below would have saved.

## 1. Commit everything

The review reads the diff of your branch against `main`, so commit all the work
that belongs in the PR first. Check with `git status` that nothing is left out.

## 2. Run the checks for the code you touched

Run the same checks CI runs for each area your diff touches
(`.github/workflows/ci.yml` is the source of truth if this list goes stale):

| area | commands |
| ---- | -------- |
| Rust (`crates/`, `apps/`, `xtask/`) | `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` |
| Tauri frontend (`ui/`) | `bun run typecheck` and `bun test`, from `ui/` |
| Node pipeline (`src/`, `test/`, `eval/`) | `pnpm typecheck` and `pnpm test` |
| REAPER script (`reaper/`) | `lua reaper/test_dsp.lua` |

If a change can move the processed audio, also run the evaluation corpus
(`cargo run --release --bin leveller-eval -- --baseline eval/baseline.json` for
Rust, `pnpm eval -- --baseline eval/baseline.json` for Node) and say in the PR
body what moved.

Fix every failure before going on.

## 3. Review the diff

Invoke the built-in `/code-review` skill on the branch diff against `main`
(run it with no target from the branch, or pass the branch name). Use effort
`high` for changes to DSP, the pipeline, or anything with concurrency or
file I/O; `medium` is enough for docs, config, and small UI changes.

Then work through every finding:

- **Fix** each finding that holds up when you trace it to a real input or
  caller. Commit the fixes.
- **Leave** a finding you judge wrong or not worth its code, and write one line
  on why in the PR body under a "Review notes" heading.

If the fixes were more than trivial, run step 2 again for the areas they
touched, and run `/code-review` once more on the new diff.

## 4. Publish

Push the branch and create the PR. In the PR body, state that the pre-PR review
ran, and list any findings you left with the reason for each.
