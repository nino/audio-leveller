# Audio Leveller — REAPER script

A native, **non-destructive** version of the leveller for REAPER. Instead of
rendering a new file, it writes a **take volume envelope** that normalises each
speech segment to −23 LUFS, ramping the gain across the silences. Nothing is
overwritten — it's all automation you can tweak or bypass afterwards.

Because it reads the take's audio through REAPER's own decoder, it works with
**any format REAPER can play** (WAV, AIFF, FLAC, MP3…), not just WAV.

## Install

1. In REAPER: **Actions → Show action list → New action → Load ReaScript…**
2. Choose `reaper/audio_leveller.lua`.
3. (Optional) assign it a keyboard shortcut or toolbar button.

## Use

1. Select one or more media items.
2. Run the **audio_leveller** action.
3. Each item gets a take volume envelope. A summary (per-segment loudness and
   gain) prints to the ReaScript console.

To preview the analysis without touching any envelopes, open the script and set
`DRY_RUN = true` near the top of the `runInReaper` function — it will only print
the segments and gains.

## Options

Edit `M.defaults` at the top of the script:

| option          | default | meaning                                             |
| --------------- | ------- | --------------------------------------------------- |
| `targetLufs`    | `-23`   | target loudness per speech segment                  |
| `maxBoostDb`    | `6`     | cap on boost, to fit REAPER's envelope range (see below) |
| `minSilenceSec` | `1.0`   | minimum gap length to count as silence              |
| `windowSec`     | `0.1`   | analysis window / gating block unit                 |
| `fraction`      | `0.25`  | silence threshold: `floor + fraction·(integrated−floor)` |
| `maxGainDb`     | `30`    | clamp on per-segment gain                            |

### Boost range / clamping

REAPER limits how far a **volume envelope** can boost — set globally in
**Preferences → Envelope Display → "Volume envelope range"** (often only +6 dB;
the volume *fader* goes to +24 dB, but the envelope doesn't). A quiet recording
would need bigger boosts than that to reach −23 LUFS, so the points would clamp
at the ceiling.

To avoid this, the script never boosts more than `maxBoostDb`: if reaching the
target needs more, it **lowers the whole target uniformly** so the loudest
segment's boost is exactly `maxBoostDb`. Every segment stays equally leveled
(the whole thing just sits a few dB lower) and nothing clamps — you make up the
level with the track fader. The console prints the adjusted target when this
happens.

For a louder result: raise **both** the REAPER preference *and* `maxBoostDb`.
For guaranteed no-clamping regardless of the preference, set `maxBoostDb = 0`
(cuts only — everything drops to the quietest segment).

## What's been verified — and what to check in REAPER

The **leveling maths are a validated port** of the standalone tool. Run

```bash
lua reaper/test_dsp.lua      # or: pnpm test:reaper
```

and it builds a synthetic multi-segment signal, applies the computed envelope,
and confirms each segment lands within 1 LU of −23 — matching the Node tool's
gains to within ~0.1 dB.

The **REAPER API glue** (reading the take audio, creating/writing the envelope)
could not be tested outside REAPER, so on first run please sanity-check:

- **Envelope creation** — the script creates the take volume envelope via the
  action ID `TOGGLE_TAKE_VOL_ENV = 40693` ("Take: Toggle take volume
  envelope"). If no envelope appears, find that action in the Action list, note
  its command ID, and update the constant.
- **Timing** — envelope point times are mapped as `sample/fs/playrate` from the
  item start. If your items are at playrate 1 (typical recordings) this is a
  straight `sample/fs`. Check the ramps line up with the silences.
- **Level** — envelope values are written through `ScaleToEnvelopeMode`, so the
  automation should read the intended dB values on the envelope.

Use `DRY_RUN = true` first to confirm the analysis looks right, then switch it
off to write envelopes. Everything runs inside a single undo block, so a single
Ctrl/Cmd-Z reverts a whole run if something looks off.
