/**
 * Renderer logic. Loaded as an ES module (`<script type="module">`) so its
 * top-level declarations live in module scope and don't collide with globals
 * such as the `window.leveller` bridge that the preload script exposes.
 *
 * It deliberately has no imports: it is compiled as a plain browser script, so
 * the few shapes it shares with the Node side (the pipeline report, the
 * parameter schema) are declared here rather than imported. The bridge is the
 * only thing that crosses.
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
interface ExpandReport {
  floorReductionDb: number;
  maxReductionDb: number;
  skipped: boolean;
}
interface CompressReport {
  loudnessRangeBeforeLu: number;
  loudnessRangeAfterLu: number;
  maxReductionDb: number;
  skipped: boolean;
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

/* ---- the parameter schema, as `src/params.ts` defines it ---- */

interface NumberParam {
  kind: "number";
  key: string;
  label: string;
  min: number;
  max: number;
  step: number;
  unit?: string;
  help: string;
}
interface BooleanParam {
  kind: "boolean";
  key: string;
  label: string;
  help: string;
}
interface ChoiceParam {
  kind: "choice";
  key: string;
  label: string;
  options: { value: string; label: string }[];
  help: string;
}
type ParamSpec = NumberParam | BooleanParam | ChoiceParam;

interface StageParamGroup {
  stage: string;
  label: string;
  description: string;
  params: ParamSpec[];
}

type ParamOverrides = Record<string, Record<string, unknown>>;

interface Preset {
  name: string;
  description: string;
  bypass: string[];
  params: ParamOverrides;
}

interface PipelineSchema {
  chain: string[];
  groups: StageParamGroup[];
  defaults: ParamOverrides;
  presets: Preset[];
  defaultPreset: string;
}

interface ProcessRequest {
  bypass?: string[];
  params?: ParamOverrides;
}

type WindowCommand = "close" | "minimise" | "zoom";

interface LevellerBridge {
  getPathForFile(file: File): string;
  getSchema(): Promise<PipelineSchema>;
  processFile(inputPath: string, request?: ProcessRequest): Promise<ProcessResponse>;
  onProgress(callback: (progress: PipelineProgress) => void): () => void;
  windowCommand(command: WindowCommand): void;
  onWindowActive(callback: (active: boolean) => void): void;
}

// The preload script exposes this on window. Typed access without turning this
// file into a module (so it compiles to a plain browser script).
const leveller = (window as unknown as { leveller: LevellerBridge }).leveller;

console.log(`renderer loaded; bridge present = ${!!leveller}`);

const drop = document.getElementById("drop") as HTMLDivElement;
const statusEl = document.getElementById("status") as HTMLElement;
const chainEl = document.getElementById("chain") as HTMLUListElement;
const presetsEl = document.getElementById("presets") as HTMLElement;
const presetNoteEl = document.getElementById("preset-note") as HTMLElement;
const resetButton = document.getElementById("reset") as HTMLButtonElement;
const rerenderButton = document.getElementById("rerender") as HTMLButtonElement;

let busy = false;
/** The last file processed, so a parameter change can be re-rendered. */
let lastInputPath: string | null = null;

/* ---- pipeline settings ---- */

let schema: PipelineSchema | null = null;
/** Which preset the current settings came from. */
let activePreset = "";
/** Fully resolved parameter values, keyed by stage — what gets sent. */
let settings: ParamOverrides = {};
/** Per-stage on/off. */
const stageEnabled = new Map<string, boolean>();

function groupFor(stage: string): StageParamGroup | undefined {
  return schema?.groups.find((g) => g.stage === stage);
}

/** The value a preset asks for, falling back to the stage default. */
function presetValue(preset: Preset, stage: string, key: string): unknown {
  const override = preset.params[stage]?.[key];
  return override !== undefined ? override : schema?.defaults[stage]?.[key];
}

/** True when any parameter or bypass differs from the active preset. */
function isEdited(): boolean {
  if (!schema) return false;
  const preset = schema.presets.find((p) => p.name === activePreset);
  if (!preset) return false;

  for (const group of schema.groups) {
    for (const param of group.params) {
      if (settings[group.stage]?.[param.key] !== presetValue(preset, group.stage, param.key)) {
        return true;
      }
    }
  }
  return schema.chain.some(
    (stage) => (stageEnabled.get(stage) ?? true) === preset.bypass.includes(stage),
  );
}

function bypassed(): string[] {
  return (schema?.chain ?? []).filter((stage) => !(stageEnabled.get(stage) ?? true));
}

/* ---- formatting ---- */

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

/** "1 segment", "2 segments", "1 silence", "3 silences". */
function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

const signed = (n: number): string => `${n >= 0 ? "+" : ""}${n.toFixed(1)}`;

/** Decimals implied by a slider's step, so 0.05 doesn't read as "0.1". */
function formatNumber(value: number, step: number): string {
  const decimals = step >= 1 ? 0 : step >= 0.1 ? 1 : step >= 0.01 ? 2 : 3;
  return value.toFixed(decimals);
}

function formatValue(param: NumberParam, value: number): string {
  const unit = param.unit ? ` ${param.unit}` : "";
  return `${formatNumber(value, param.step)}${unit}`;
}

/* ---- the parameter panel ---- */

function markEdited(): void {
  refreshChangeMarks();
  updatePresetChrome();
}

function setParam(stage: string, key: string, value: unknown): void {
  settings[stage] = { ...settings[stage], [key]: value };
  markEdited();
}

function numberControl(stage: string, param: NumberParam): HTMLElement {
  const row = document.createElement("div");
  row.className = "param";
  row.title = param.help;
  row.dataset.stage = stage;
  row.dataset.key = param.key;

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = param.label;

  const slider = document.createElement("input");
  slider.type = "range";
  slider.min = String(param.min);
  slider.max = String(param.max);
  slider.step = String(param.step);
  slider.value = String(settings[stage]?.[param.key] ?? param.min);

  const value = document.createElement("span");
  value.className = "value";
  value.textContent = formatValue(param, Number(slider.value));

  slider.addEventListener("input", () => {
    const parsed = Number(slider.value);
    value.textContent = formatValue(param, parsed);
    setParam(stage, param.key, parsed);
  });

  row.append(name, slider, value);
  return row;
}

function booleanControl(stage: string, param: BooleanParam): HTMLElement {
  const row = document.createElement("div");
  row.className = "param";
  row.title = param.help;
  row.dataset.stage = stage;
  row.dataset.key = param.key;

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = param.label;

  const label = document.createElement("label");
  label.className = "check-value";
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = settings[stage]?.[param.key] === true;
  const caption = document.createElement("span");
  caption.textContent = box.checked ? "on" : "off";
  box.addEventListener("change", () => {
    caption.textContent = box.checked ? "on" : "off";
    setParam(stage, param.key, box.checked);
  });
  label.append(box, caption);

  row.append(name, label);
  return row;
}

function choiceControl(stage: string, param: ChoiceParam): HTMLElement {
  const row = document.createElement("div");
  row.className = "param";
  row.title = param.help;
  row.dataset.stage = stage;
  row.dataset.key = param.key;

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = param.label;

  const select = document.createElement("select");
  select.className = "aqua-select";
  for (const option of param.options) {
    const el = document.createElement("option");
    el.value = option.value;
    el.textContent = option.label;
    select.append(el);
  }
  select.value = String(settings[stage]?.[param.key] ?? param.options[0]?.value ?? "");
  select.addEventListener("change", () => setParam(stage, param.key, select.value));

  row.append(name, select);
  return row;
}

function control(stage: string, param: ParamSpec): HTMLElement {
  switch (param.kind) {
    case "number":
      return numberControl(stage, param);
    case "boolean":
      return booleanControl(stage, param);
    case "choice":
      return choiceControl(stage, param);
  }
}

/**
 * One row per stage: a checkbox that bypasses it, a disclosure triangle for
 * its parameters, and a note that fills in with what the stage actually did
 * once a file has been through it.
 */
function buildChainUi(): void {
  if (!schema) return;
  chainEl.replaceChildren();

  for (const stage of schema.chain) {
    const group = groupFor(stage);

    const li = document.createElement("li");
    li.dataset.stage = stage;

    const head = document.createElement("div");
    head.className = "stage-head";

    const label = document.createElement("label");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.dataset.stage = stage;
    box.checked = stageEnabled.get(stage) ?? true;
    const title = document.createElement("span");
    title.textContent = group?.label ?? stage;
    label.append(box, title);

    const params = document.createElement("div");
    params.className = "stage-params";
    params.hidden = true;

    const disclose = document.createElement("button");
    disclose.className = "aqua-button disclose";
    disclose.textContent = "▶";
    disclose.title = group?.description ?? "";
    disclose.disabled = !group;
    disclose.addEventListener("click", () => {
      params.hidden = !params.hidden;
      disclose.textContent = params.hidden ? "▶" : "▼";
    });

    const note = document.createElement("span");
    note.className = "stage-note";
    note.textContent = group?.description ?? "";

    box.addEventListener("change", () => {
      stageEnabled.set(stage, box.checked);
      head.classList.toggle("off", !box.checked);
      markEdited();
    });
    head.classList.toggle("off", !box.checked);

    head.append(disclose, label, note);

    if (group) for (const param of group.params) params.append(control(stage, param));

    li.append(head, params);
    chainEl.append(li);
  }

  refreshChangeMarks();
}

/** Push `settings` back into the controls, after a preset switch. */
function syncControls(): void {
  if (!schema) return;

  for (const box of chainEl.querySelectorAll<HTMLInputElement>(".stage-head input[type=checkbox]")) {
    const stage = box.dataset.stage ?? "";
    box.checked = stageEnabled.get(stage) ?? true;
    box.closest(".stage-head")?.classList.toggle("off", !box.checked);
  }

  for (const group of schema.groups) {
    for (const param of group.params) {
      const row = chainEl.querySelector<HTMLElement>(
        `.param[data-stage="${group.stage}"][data-key="${param.key}"]`,
      );
      if (!row) continue;
      const value = settings[group.stage]?.[param.key];

      if (param.kind === "number") {
        const slider = row.querySelector("input[type=range]") as HTMLInputElement;
        const readout = row.querySelector(".value") as HTMLElement;
        slider.value = String(value);
        readout.textContent = formatValue(param, Number(value));
      } else if (param.kind === "boolean") {
        const box = row.querySelector("input[type=checkbox]") as HTMLInputElement;
        box.checked = value === true;
        (row.querySelector(".check-value span") as HTMLElement).textContent = box.checked
          ? "on"
          : "off";
      } else {
        (row.querySelector("select") as HTMLSelectElement).value = String(value);
      }
    }
  }

  refreshChangeMarks();
}

/** Highlight the parameters that no longer match the active preset. */
function refreshChangeMarks(): void {
  if (!schema) return;
  const preset = schema.presets.find((p) => p.name === activePreset);
  if (!preset) return;

  for (const group of schema.groups) {
    for (const param of group.params) {
      const row = chainEl.querySelector<HTMLElement>(
        `.param[data-stage="${group.stage}"][data-key="${param.key}"]`,
      );
      row?.classList.toggle(
        "changed",
        settings[group.stage]?.[param.key] !== presetValue(preset, group.stage, param.key),
      );
    }
  }
}

function applyPreset(name: string): void {
  if (!schema) return;
  const preset = schema.presets.find((p) => p.name === name) ?? schema.presets[0];
  activePreset = preset.name;

  settings = {};
  for (const [stage, values] of Object.entries(schema.defaults)) {
    settings[stage] = { ...values, ...(preset.params[stage] ?? {}) };
  }
  for (const stage of schema.chain) stageEnabled.set(stage, !preset.bypass.includes(stage));

  syncControls();
  updatePresetChrome();
}

function updatePresetChrome(): void {
  if (!schema) return;
  const edited = isEdited();
  const preset = schema.presets.find((p) => p.name === activePreset);

  // The preset stays selected once it has been edited: it is still the base
  // the settings came from, and the note says what has moved since.
  for (const button of presetsEl.querySelectorAll<HTMLButtonElement>(".seg")) {
    button.classList.toggle("on", button.dataset.preset === activePreset);
  }

  resetButton.disabled = !edited;
  const skipped = bypassed();
  presetNoteEl.innerHTML =
    `${preset?.description ?? ""}` +
    (edited ? ` <span class="edited">· edited</span>` : "") +
    (skipped.length > 0 ? ` <span class="dim">· bypassing ${skipped.join(", ")}</span>` : "");
}

function buildPresetUi(): void {
  if (!schema) return;
  presetsEl.replaceChildren();

  for (const preset of schema.presets) {
    const button = document.createElement("button");
    button.className = "seg";
    button.dataset.preset = preset.name;
    button.textContent = preset.name;
    button.title = preset.description;
    button.addEventListener("click", () => applyPreset(preset.name));
    presetsEl.append(button);
  }
}

/* ---- the report ---- */

function setStatus(html: string, cls = ""): void {
  statusEl.hidden = false;
  statusEl.className = `panel glass status ${cls}`;
  statusEl.innerHTML = html;
}

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
    case "expand": {
      const d = r as ExpandReport;
      if (d.skipped) return "floor already deep — left alone";
      return `floor down ${d.floorReductionDb.toFixed(1)} dB`;
    }
    case "compress": {
      const d = r as CompressReport;
      if (d.skipped) return "already even — left alone";
      return `range ${d.loudnessRangeBeforeLu.toFixed(1)} → ${d.loudnessRangeAfterLu.toFixed(1)} LU`;
    }
    case "level": {
      const d = r as LevellerReport;
      return `${pluralize(d.segments.length, "segment")} levelled`;
    }
    default:
      return "";
  }
}

/** Fill each stage row's note with what that stage decided this run. */
function updateStageNotes(report: PipelineReport): void {
  for (const stage of report.stages) {
    const note = chainEl.querySelector<HTMLElement>(`li[data-stage="${stage.name}"] .stage-note`);
    if (!note) continue;
    note.textContent = stage.enabled
      ? `${summarise(stage)} · ${stage.elapsedMs.toFixed(0)} ms`
      : "bypassed";
  }
}

function renderResult(result: NonNullable<ProcessResponse["result"]>): void {
  const { inputPath, outputPath, roomTonePath, report } = result;
  lastInputPath = inputPath;

  updateStageNotes(report);

  const level = levellerReportOf(report);
  let detail = "";
  if (level) {
    const segRows = level.segments
      .map((s, i) => {
        const l = Number.isFinite(s.loudnessLufs) ? `${s.loudnessLufs.toFixed(1)}` : "—";
        return `<tr><td>${i + 1}</td><td>${l} LUFS</td><td>${signed(s.gainDb)} dB</td></tr>`;
      })
      .join("");

    const rt = level.roomTone;
    const roomToneLine = roomTonePath
      ? `<div class="meta">Wrote <strong>${basename(roomTonePath)}</strong> —
           ${rt.durationSec.toFixed(1)}s room tone ·
           ${pluralize(rt.clips.length, "clean clip")} · ${signed(rt.gainDb)} dB</div>`
      : `<div class="meta">No room tone (no usable silence found)</div>`;

    detail = `<div class="meta">${pluralize(level.silences.length, "silence")} ·
        ${pluralize(level.segments.length, "segment")}</div>
      <table class="segments">
        <thead><tr><th>#</th><th>Was</th><th>Gain</th></tr></thead>
        <tbody>${segRows}</tbody>
      </table>
      ${roomToneLine}`;
  }

  setStatus(
    `<div class="done">Wrote ${basename(outputPath)}</div>
     <div class="meta">${activePreset}${isEdited() ? " (edited)" : ""} ·
       ${report.input.integratedLufs.toFixed(1)} LUFS →
       ${report.output.integratedLufs.toFixed(1)} LUFS ·
       peak ${report.output.peakDbfs.toFixed(1)} dBFS ·
       ${report.sourceSampleRate} Hz · ${report.durationSec.toFixed(1)}s</div>
     ${detail}`,
    "done",
  );
}

/* ---- running ---- */

function setRerenderEnabled(enabled: boolean): void {
  rerenderButton.disabled = !enabled;
  rerenderButton.classList.toggle("primary", enabled);
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
  await run(path, file.name);
}

async function run(path: string, name: string): Promise<void> {
  if (busy) return;
  busy = true;
  drop.classList.add("busy");
  setRerenderEnabled(false);
  setStatus(
    `<div class="working-line"><span class="spinner"></span>
       <span>Processing <strong>${name}</strong>…</span></div>`,
    "working",
  );

  const unsubscribe = leveller.onProgress((p) => {
    setStatus(
      `<div class="working-line"><span class="spinner"></span>
         <span><strong>${name}</strong> — ${p.stage}, step ${p.index + 1} of ${p.total}</span></div>
       <div class="bar"><span style="width:${Math.round(p.overall * 100)}%"></span></div>`,
      "working",
    );
  });

  try {
    const res = await leveller.processFile(path, { bypass: bypassed(), params: settings });
    console.log(`processFile result: ok=${res.ok}${res.error ? ` error=${res.error}` : ""}`);

    if (!res.ok || !res.result) {
      setStatus(`Failed: ${res.error ?? "unknown error"}`, "error");
      lastInputPath = path;
      return;
    }
    renderResult(res.result);
  } finally {
    unsubscribe();
    busy = false;
    drop.classList.remove("busy");
    setRerenderEnabled(lastInputPath !== null);
  }
}

/* ---- wiring ---- */

rerenderButton.addEventListener("click", () => {
  if (lastInputPath) void run(lastInputPath, basename(lastInputPath));
});

resetButton.addEventListener("click", () => applyPreset(activePreset));

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

/* ---- title bar ---- */

// The traffic lights are drawn by this window, so they have to ask the main
// process to do what the system's own buttons would have done.
for (const light of document.querySelectorAll<HTMLButtonElement>(".light[data-window]")) {
  light.addEventListener("click", () => {
    leveller.windowCommand(light.dataset.window as WindowCommand);
  });
}

leveller.onWindowActive((active) => {
  document.body.classList.toggle("inactive", !active);
});

void (async () => {
  try {
    schema = await leveller.getSchema();
    buildPresetUi();
    applyPreset(schema.defaultPreset);
    buildChainUi();
    syncControls();
    updatePresetChrome();
  } catch (err) {
    console.log(`schema load failed: ${err instanceof Error ? err.message : String(err)}`);
    setStatus("Couldn't load the pipeline parameters.", "error");
  }
})();
