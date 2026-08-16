/**
 * Renderer logic. Loaded as an ES module (`<script type="module">`) so its
 * top-level declarations live in module scope and don't collide with globals
 * such as the `window.leveller` bridge that the preload script exposes.
 */

interface SegmentReport {
  start: number;
  end: number;
  loudnessLufs: number;
  gainDb: number;
}

interface RoomToneReport {
  clips: { start: number; end: number }[];
  instances: number;
  gainDb: number;
  durationSec: number;
}

/** The `level` stage's own report. */
interface LevellerReport {
  sampleRate: number;
  integratedLufs: number;
  thresholdLufs: number;
  segments: SegmentReport[];
  silences: { start: number; end: number }[];
  limiterGainReductionDb: number;
  roomTone: RoomToneReport;
}

interface StageReport {
  name: string;
  enabled: boolean;
  elapsedMs: number;
  report: unknown;
}

/** The per-stage reports the inspector knows how to summarise. */
interface DeclickReport {
  detected: number;
  repaired: number;
  aborted: boolean;
}
interface DenoiseReport {
  backend: string | null;
  reductionAppliedDb: number;
  reductionAchievedDb: number;
  skippedEntirely: boolean;
}
interface DereverbReport {
  decayBeforeMs: number;
  decayAfterMs: number;
  skipped: boolean;
}
interface EqReport {
  bands: { freq: number; gainDb: number; q: number }[];
  rumbleFreq: number | null;
  skipped: boolean;
}
interface DynEqReport {
  maxReductionDb: number;
  activeFraction: number;
}

interface PipelineReport {
  sourceSampleRate: number;
  workingSampleRate: number;
  channels: number;
  durationSec: number;
  resampled: boolean;
  input: { integratedLufs: number; peakDbfs: number };
  output: { integratedLufs: number; peakDbfs: number };
  stages: StageReport[];
  extras: string[];
}

interface PipelineProgress {
  stage: string;
  index: number;
  total: number;
  overall: number;
}

interface ProcessResponse {
  ok: boolean;
  result?: {
    inputPath: string;
    outputPath: string;
    roomTonePath: string | null;
    extraPaths: Record<string, string>;
    report: PipelineReport;
  };
  error?: string;
}

interface LevellerBridge {
  getPathForFile(file: File): string;
  processFile(inputPath: string, bypass?: string[]): Promise<ProcessResponse>;
  onProgress(callback: (progress: PipelineProgress) => void): () => void;
}

// The preload script exposes this on window. Typed access without turning this
// file into a module (so it compiles to a plain browser script).
const leveller = (window as unknown as { leveller: LevellerBridge }).leveller;

console.log(`renderer loaded; bridge present = ${!!leveller}`);

const drop = document.getElementById("drop") as HTMLDivElement;
const statusEl = document.getElementById("status") as HTMLElement;

let busy = false;

function setStatus(html: string, cls = ""): void {
  statusEl.hidden = false;
  statusEl.className = `status ${cls}`;
  statusEl.innerHTML = html;
}

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

/** "1 segment", "2 segments", "1 silence", "3 silences". */
function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

const signed = (n: number): string => `${n >= 0 ? "+" : ""}${n.toFixed(1)}`;

/** The level stage's report, if that stage ran. */
function levellerReportOf(report: PipelineReport): LevellerReport | null {
  const stage = report.stages.find((s) => s.name === "level" && s.enabled);
  return (stage?.report as LevellerReport | undefined) ?? null;
}

/**
 * One line saying what a stage actually did. The point of the inspector is to
 * make each stage's decision visible, so "declick 42 ms" is not enough — the
 * question a user has is whether it found anything.
 */
function summarise(stage: StageReport): string {
  if (!stage.enabled) return "bypassed";
  const r = stage.report;

  switch (stage.name) {
    case "declick": {
      const d = r as DeclickReport;
      if (d.aborted) return "declined — too much of the file looked like clicks";
      return d.repaired === 0 ? "no clicks found" : `${d.repaired} click(s) repaired`;
    }
    case "denoise": {
      const d = r as DenoiseReport;
      if (d.skippedEntirely) return "already clean — nothing removed";
      return `${d.reductionAchievedDb.toFixed(1)} dB of noise removed (${d.backend})`;
    }
    case "dereverb": {
      const d = r as DereverbReport;
      if (d.skipped) return `dry (${d.decayBeforeMs.toFixed(0)} ms decay) — left alone`;
      return `decay ${d.decayBeforeMs.toFixed(0)} → ${d.decayAfterMs.toFixed(0)} ms`;
    }
    case "eq": {
      const d = r as EqReport;
      if (d.skipped) return "not enough speech to measure";
      const rumble = d.rumbleFreq ? `, rumble filter at ${d.rumbleFreq} Hz` : "";
      if (d.bands.length === 0) return `no correction needed${rumble}`;
      return (
        d.bands.map((b) => `${Math.round(b.freq)} Hz ${signed(b.gainDb)} dB`).join(", ") + rumble
      );
    }
    case "dyneq": {
      const d = r as DynEqReport;
      if (d.activeFraction < 0.001) return "no resonances found";
      return `up to ${d.maxReductionDb.toFixed(1)} dB, ${(d.activeFraction * 100).toFixed(0)}% of the time`;
    }
    case "level": {
      const d = r as LevellerReport;
      return `${pluralize(d.segments.length, "segment")} levelled`;
    }
    default:
      return "";
  }
}

function renderResult(result: NonNullable<ProcessResponse["result"]>): void {
  const { inputPath, outputPath, roomTonePath, report } = result;
  lastInputPath = inputPath;

  const chain = report.stages
    .map(
      (s) => `<li>
        <label>
          <input type="checkbox" data-stage="${s.name}" ${s.enabled ? "checked" : ""}>
          <span class="${s.enabled ? "" : "dim"}">${s.name}</span>
        </label>
        <div class="meta stage-detail">${summarise(s)}${
          s.enabled ? ` <span class="dim">· ${s.elapsedMs.toFixed(0)} ms</span>` : ""
        }</div>
      </li>`,
    )
    .join("");

  const leveller = levellerReportOf(report);
  let detail = "";
  if (leveller) {
    const segRows = leveller.segments
      .map((s, i) => {
        const l = Number.isFinite(s.loudnessLufs) ? `${s.loudnessLufs.toFixed(1)}` : "—";
        return `<tr><td>${i + 1}</td><td>${l} LUFS</td><td>${signed(s.gainDb)} dB</td></tr>`;
      })
      .join("");

    const rt = leveller.roomTone;
    const roomToneLine = roomTonePath
      ? `<div class="done">✓ Wrote <strong>${basename(roomTonePath)}</strong></div>
         <div class="meta">${rt.durationSec.toFixed(1)}s room tone ·
           ${pluralize(rt.clips.length, "clean clip")} ·
           ${signed(rt.gainDb)} dB</div>`
      : `<div class="meta">No room tone (no usable silence found)</div>`;

    detail = `<div class="meta">${pluralize(leveller.silences.length, "silence")} ·
        ${pluralize(leveller.segments.length, "segment")}</div>
      <table class="segments">
        <thead><tr><th>#</th><th>Was</th><th>Gain</th></tr></thead>
        <tbody>${segRows}</tbody>
      </table>
      ${roomToneLine}`;
  }

  setStatus(
    `<div class="done">✓ Wrote <strong>${basename(outputPath)}</strong></div>
     <div class="meta">${report.input.integratedLufs.toFixed(1)} LUFS →
       ${report.output.integratedLufs.toFixed(1)} LUFS ·
       ${report.sourceSampleRate} Hz · ${report.durationSec.toFixed(1)}s</div>
     <ul class="chain">${chain}</ul>
     <button id="rerender">Re-render with these stages</button>
     ${detail}`,
    "done",
  );

  const button = document.getElementById("rerender");
  button?.addEventListener("click", () => {
    const bypass = [...statusEl.querySelectorAll<HTMLInputElement>("input[data-stage]")]
      .filter((input) => !input.checked)
      .map((input) => input.dataset.stage ?? "");
    if (lastInputPath) void run(lastInputPath, basename(lastInputPath), bypass);
  });
}

/** The last file processed, so the inspector can re-render it. */
let lastInputPath: string | null = null;

async function handleFile(file: File): Promise<void> {
  if (busy) return;
  if (!/\.wav$/i.test(file.name)) {
    setStatus(`<strong>${file.name}</strong> isn't a .wav file.`, "error");
    return;
  }

  const path = leveller.getPathForFile(file);
  console.log(`resolved path = "${path}"`);
  if (!path) {
    setStatus(`Couldn't resolve a file path for <strong>${file.name}</strong>.`, "error");
    return;
  }
  await run(path, file.name, []);
}

async function run(path: string, name: string, bypass: string[]): Promise<void> {
  if (busy) return;
  busy = true;
  drop.classList.add("busy");
  setStatus(`<span class="spinner"></span> Processing <strong>${name}</strong>…`, "working");

  const unsubscribe = leveller.onProgress((p) => {
    setStatus(
      `<span class="spinner"></span> <strong>${name}</strong>
       <div class="meta">${p.stage} — step ${p.index + 1} of ${p.total}</div>
       <div class="bar"><span style="width:${Math.round(p.overall * 100)}%"></span></div>`,
      "working",
    );
  });

  try {
    const res = await leveller.processFile(path, bypass);
    console.log(`processFile result: ok=${res.ok}${res.error ? ` error=${res.error}` : ""}`);

    if (!res.ok || !res.result) {
      setStatus(`Failed: ${res.error ?? "unknown error"}`, "error");
      return;
    }
    renderResult(res.result);
  } finally {
    unsubscribe();
    busy = false;
    drop.classList.remove("busy");
  }
}

function stop(e: Event): void {
  e.preventDefault();
  e.stopPropagation();
}

["dragenter", "dragover"].forEach((evt) =>
  drop.addEventListener(evt, (e) => {
    stop(e);
    if (!busy) drop.classList.add("hover");
  }),
);

["dragleave", "dragend"].forEach((evt) =>
  drop.addEventListener(evt, (e) => {
    stop(e);
    drop.classList.remove("hover");
  }),
);

drop.addEventListener("drop", (e) => {
  stop(e);
  drop.classList.remove("hover");
  const dt = (e as DragEvent).dataTransfer;
  const file = dt?.files?.[0];
  console.log(`drop: files=${dt?.files?.length ?? 0}, first=${file?.name ?? "none"}`);
  if (file) void handleFile(file);
});

// Guard the whole window so a stray drop elsewhere doesn't navigate away.
window.addEventListener("dragover", stop);
window.addEventListener("drop", stop);
