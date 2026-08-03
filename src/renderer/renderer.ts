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
  processFile(inputPath: string): Promise<ProcessResponse>;
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

function renderResult(result: NonNullable<ProcessResponse["result"]>): void {
  const { outputPath, roomTonePath, report } = result;

  const chain = report.stages
    .map((s) =>
      s.enabled
        ? `<li>${s.name} <span class="dim">${s.elapsedMs.toFixed(0)} ms</span></li>`
        : `<li class="dim">${s.name} — bypassed</li>`,
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
     ${detail}`,
    "done",
  );
}

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
  busy = true;
  drop.classList.add("busy");
  setStatus(`<span class="spinner"></span> Processing <strong>${file.name}</strong>…`, "working");

  const unsubscribe = leveller.onProgress((p) => {
    setStatus(
      `<span class="spinner"></span> <strong>${file.name}</strong>
       <div class="meta">${p.stage} — step ${p.index + 1} of ${p.total}</div>
       <div class="bar"><span style="width:${Math.round(p.overall * 100)}%"></span></div>`,
      "working",
    );
  });

  try {
    const res = await leveller.processFile(path);
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
