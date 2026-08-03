# Evaluation fixtures

Drop real `.wav` recordings in this directory and `pnpm eval` will pick them up
automatically, one case per file, named `fixture:<filename>`.

Real material has no clean reference, so the reference-based metrics (SI-SDR,
click residual) are skipped. What still applies:

- `lufsError` — did the programme land on target
- `outputPeakDbfs` — did the limiter ceiling hold
- `snrGainDb` — did the chain move speech and noise apart, and in which direction
- `inputFloorLufs` / `outputFloorLufs` — where the noise floor sat before and after

Useful material to keep here: a noisy room, a reverberant room, something with
clicks or mouth noise, a quiet talker, a loud talker, and one recording that is
already clean and well-levelled (the transparency check — the chain should
barely touch it).

Files here are ignored by git: they are usually large, and often not yours to
redistribute.
