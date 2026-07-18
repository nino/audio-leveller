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

interface LevellerReport {
  sampleRate: number;
  integratedLufs: number;
  thresholdLufs: number;
  segments: SegmentReport[];
  silences: { start: number; end: number }[];
  limiterGainReductionDb: number;
  roomTone: RoomToneReport;
}

interface ProcessResponse {
  ok: boolean;
  result?: {
    inputPath: string;
    outputPath: string;
    roomTonePath: string | null;
    report: LevellerReport;
  };
  error?: string;
}

interface LevellerBridge {
  getPathForFile(file: File): string;
  processFile(inputPath: string): Promise<ProcessResponse>;
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

  const res = await leveller.processFile(path);
  console.log(`processFile result: ok=${res.ok}${res.error ? ` error=${res.error}` : ""}`);

  busy = false;
  drop.classList.remove("busy");

  if (!res.ok || !res.result) {
    setStatus(`Failed: ${res.error ?? "unknown error"}`, "error");
    return;
  }

  const { outputPath, roomTonePath, report } = res.result;
  const segRows = report.segments
    .map((s, i) => {
      const l = Number.isFinite(s.loudnessLufs) ? `${s.loudnessLufs.toFixed(1)}` : "—";
      const g = `${s.gainDb >= 0 ? "+" : ""}${s.gainDb.toFixed(1)}`;
      return `<tr><td>${i + 1}</td><td>${l} LUFS</td><td>${g} dB</td></tr>`;
    })
    .join("");

  const rt = report.roomTone;
  const roomToneLine = roomTonePath
    ? `<div class="done">✓ Wrote <strong>${basename(roomTonePath)}</strong></div>
       <div class="meta">${rt.durationSec.toFixed(1)}s room tone ·
         ${pluralize(rt.clips.length, "clean clip")} ·
         ${rt.gainDb >= 0 ? "+" : ""}${rt.gainDb.toFixed(1)} dB</div>`
    : `<div class="meta">No room tone (no usable silence found)</div>`;

  setStatus(
    `<div class="done">✓ Wrote <strong>${basename(outputPath)}</strong></div>
     <div class="meta">Input ${report.integratedLufs.toFixed(1)} LUFS ·
       ${pluralize(report.silences.length, "silence")} ·
       ${pluralize(report.segments.length, "segment")}</div>
     <table class="segments">
       <thead><tr><th>#</th><th>Was</th><th>Gain</th></tr></thead>
       <tbody>${segRows}</tbody>
     </table>
     ${roomToneLine}`,
    "done",
  );
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
