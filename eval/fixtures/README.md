# Evaluation fixtures

Drop real `.wav` recordings in this directory and `pnpm eval` will pick them up
automatically, one case per file, named `fixture:<filename>`.

A pinned set can be fetched rather than hunted for:

```bash
pnpm fetch-fixtures
```

That reads `eval/references/manifest.json`, downloads each file, and verifies it
against a checksum tracked in the repository — see `eval/references/README.md`.
Reference masters from that manifest land in `eval/references/` instead, because
anything sitting in *this* directory gets processed, and mastering somebody
else's master produces numbers that mean nothing.

Each file also produces two **denoiser** cases, `fixture:<name>:spectral` and
`fixture:<name>:onnx`. Those add broadband noise at 20 dB SNR to the first 60
seconds and score the result against the recording itself — which supplies the
clean reference real material otherwise lacks. They are the only place a
*trained* denoiser can be judged, because the synthetic corpus is speech-shaped
rather than speech and DeepFilterNet3 does not accept it as a voice. The `onnx`
pair is skipped when the weights are absent (`pnpm fetch-model`).

The chain's own outputs — `<name>_processed.wav` and `<name>_roomtone.wav`,
which it writes next to its input — are ignored here. Otherwise processing a
fixture in place would enrol the results as fixtures on the next run, and the
harness would end up grading the chain on its own output.

Real material has no clean reference, so on the plain `fixture:<name>` case the
reference-based metrics (SI-SDR, click residual) are skipped. What still
applies:

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
