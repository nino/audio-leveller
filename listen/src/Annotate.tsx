/**
 * Region annotation: pick a WAV from `listening/annotate/`, then mark up
 * breaths, clicks, plosives… on a zoomable waveform. Labels autosave to
 * `<name>.annotations.json` next to the file; `/api/annotate/export.csv`
 * flattens every file for the eval harness.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DEFAULT_ANNOTATION_LABELS, type Annotation, type AnnotationFile } from "../../src/listen/types";
import { api } from "./api";
import { usePlayer } from "./player";
import { buildPeaks, type PeakCache } from "./peaks";
import { AnnotateWave, fmtTime, labelColour, type WaveHandle } from "./AnnotateWave";

const byStart = (a: Annotation, b: Annotation): number => a.start - b.start || a.end - b.end;
const newId = (): string => Math.random().toString(36).slice(2, 10);
/** Playing a selected region loops it with a little context either side. */
const CONTEXT_S = 0.25;
const GAINS = [1, 2, 4, 8, 16];

// ---------------------------------------------------------------- file list

export function AnnotateIndexPage() {
  const files = useQuery({ queryKey: ["annotatable"], queryFn: api.annotatable });
  const navigate = useNavigate();
  return (
    <div className="page">
      <div className="crumbs">
        <Link to="/">Sessions</Link> › Annotate
      </div>
      <h1>Annotate audio</h1>
      <p className="hint">
        Drop WAVs into <code>listening/annotate/</code>; labels are saved next to each file as{" "}
        <code>&lt;name&gt;.annotations.json</code>.{" "}
        <a className="aqua-link" href={api.annotateExportUrl()}>
          Export all as CSV
        </a>
      </p>
      {files.isLoading && <p className="hint">Loading…</p>}
      {files.error && <p className="error">{String(files.error)}</p>}
      {files.data && files.data.length === 0 && <p className="hint">No WAVs in listening/annotate/ yet.</p>}
      <ul className="session-list">
        {files.data?.map((f) => (
          <li key={f.file} className="session-row metal-inset">
            <div className="session-main">
              <div className="session-name">{f.file}</div>
              <div className="session-meta">
                {(f.bytes / 1e6).toFixed(1)} MB
                {f.duration != null && <> · {fmtTime(f.duration).replace(/\.\d+$/, "")}</>}
                {f.annotations == null ? (
                  <> · not annotated</>
                ) : (
                  <>
                    {" "}
                    · {f.annotations} region{f.annotations === 1 ? "" : "s"} · saved{" "}
                    {f.updatedAt ? new Date(f.updatedAt).toLocaleString() : ""}
                  </>
                )}
              </div>
            </div>
            <div className="session-actions">
              <button
                className="aqua-button primary"
                onClick={() => navigate({ to: "/annotate/$file", params: { file: f.file } })}
              >
                {f.annotations == null ? "Annotate" : "Continue"}
              </button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ------------------------------------------------------------------- editor

export function AnnotatePage() {
  const { file } = useParams({ from: "/annotate/$file" });
  const qc = useQueryClient();
  const saved = useQuery({ queryKey: ["annotations", file], queryFn: () => api.annotations(file) });
  if (saved.isLoading) return <div className="page hint">Loading…</div>;
  if (saved.error) return <div className="page error">{String(saved.error)}</div>;
  return (
    <Editor
      key={file}
      file={file}
      initial={
        saved.data ?? {
          file,
          sampleRate: 0,
          duration: 0,
          labels: [...DEFAULT_ANNOTATION_LABELS],
          annotations: [],
          updatedAt: new Date().toISOString(),
        }
      }
      onSaved={(a) => qc.setQueryData(["annotations", file], a)}
    />
  );
}

interface EditorProps {
  file: string;
  initial: AnnotationFile;
  onSaved: (a: AnnotationFile) => void;
}

function Editor({ file, initial, onSaved }: EditorProps) {
  const urls = useMemo(() => [api.annotateAudioUrl(file)], [file]);
  const player = usePlayer(urls);
  const { state } = player;
  const buffer = player.buffers[0] ?? null;

  const [doc, setDoc] = useState<AnnotationFile>(initial);
  const latest = useRef(doc);
  latest.current = doc;
  const [dirty, setDirty] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selection, setSelection] = useState<[number, number] | null>(null);
  const [sortBy, setSortBy] = useState<"time" | "label">("time");
  const [gain, setGain] = useState(1);
  const [peaks, setPeaks] = useState<PeakCache | null>(null);
  const wave = useRef<WaveHandle>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Build the peak cache once the file is decoded (off the render path so
  // "decoding…" shows first).
  useEffect(() => {
    if (!buffer) return;
    const id = window.setTimeout(() => setPeaks(buildPeaks(buffer)), 0);
    return () => window.clearTimeout(id);
  }, [buffer]);

  // ---- persistence (debounced, like the trial page) ----
  const save = useMutation({
    mutationFn: (a: AnnotationFile) => api.saveAnnotations(file, a),
    onSuccess: (_d, a) => {
      onSaved(a);
      if (latest.current === a) setDirty(false);
    },
  });
  const timer = useRef(0);
  const update = useCallback(
    (fn: (d: AnnotationFile) => AnnotationFile) => {
      const next = { ...fn(latest.current), updatedAt: new Date().toISOString() };
      latest.current = next;
      setDoc(next);
      setDirty(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => save.mutate(next), 400);
    },
    [save],
  );

  // Record what we decoded, so the sidecar can be trusted without the WAV.
  useEffect(() => {
    if (!buffer) return;
    if (latest.current.sampleRate !== buffer.sampleRate || Math.abs(latest.current.duration - buffer.duration) > 1e-3)
      update((d) => ({ ...d, sampleRate: buffer.sampleRate, duration: buffer.duration }));
  }, [buffer, update]);

  const sorted = useMemo(() => {
    const s = [...doc.annotations].sort(byStart);
    return sortBy === "time" ? s : s.sort((a, b) => a.label.localeCompare(b.label) || byStart(a, b));
  }, [doc.annotations, sortBy]);
  const timeOrder = useMemo(() => [...doc.annotations].sort(byStart), [doc.annotations]);
  const selected = doc.annotations.find((a) => a.id === selectedId) ?? null;

  // ---- loop region follows the selection / selected region ----
  const loopRegion = useMemo<[number, number] | null>(() => {
    if (selection) return selection;
    if (selected) return [Math.max(0, selected.start - CONTEXT_S), Math.min(state.duration || Infinity, selected.end + CONTEXT_S)];
    return null;
  }, [selection, selected, state.duration]);
  const loopKey = loopRegion ? `${loopRegion[0]}|${loopRegion[1]}` : "";
  const setRegion = player.setRegion, setLoop = player.setLoop;
  useEffect(() => {
    setRegion(loopRegion);
    setLoop(!!loopRegion);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loopKey, setRegion, setLoop]);

  // ---- editing ----
  const changeAnnotation = useCallback(
    (a: Annotation) => update((d) => ({ ...d, annotations: d.annotations.map((x) => (x.id === a.id ? a : x)) })),
    [update],
  );
  const patchSelected = (p: Partial<Annotation>): void => {
    if (selected) changeAnnotation({ ...selected, ...p });
  };
  const addRegion = useCallback(
    (label: string): void => {
      if (!selection || selection[1] - selection[0] < 0.002) return;
      const a: Annotation = { id: newId(), start: selection[0], end: selection[1], label, confidence: "sure" };
      update((d) => ({ ...d, annotations: [...d.annotations, a] }));
      setSelection(null);
      setSelectedId(a.id);
    },
    [selection, update],
  );
  const applyLabel = useCallback(
    (label: string): void => {
      if (selection) addRegion(label);
      else if (selected) changeAnnotation({ ...selected, label });
    },
    [addRegion, changeAnnotation, selected, selection],
  );
  const remove = useCallback(
    (id: string): void => {
      update((d) => ({ ...d, annotations: d.annotations.filter((a) => a.id !== id) }));
      setSelectedId((s) => (s === id ? null : s));
    },
    [update],
  );
  const goTo = useCallback(
    (a: Annotation): void => {
      setSelectedId(a.id);
      setSelection(null);
      wave.current?.reveal(a.start);
      player.seek(Math.max(0, a.start - CONTEXT_S));
    },
    [player],
  );
  const step = useCallback(
    (dir: 1 | -1): void => {
      if (timeOrder.length === 0) return;
      let i = timeOrder.findIndex((a) => a.id === selectedId);
      if (i >= 0) i = (i + dir + timeOrder.length) % timeOrder.length;
      else if (dir > 0) {
        // Nothing selected: the next region after the playhead (or the first).
        i = timeOrder.findIndex((a) => a.start > state.position);
        if (i < 0) i = 0;
      } else {
        i = timeOrder.length - 1;
        for (let j = timeOrder.length - 1; j >= 0; j--) if (timeOrder[j].start < state.position) { i = j; break; }
      }
      goTo(timeOrder[i]);
    },
    [goTo, selectedId, state.position, timeOrder],
  );

  /** First letter of each label is its shortcut; earlier labels win a clash. */
  const shortcuts = useMemo(() => {
    const m = new Map<string, string>();
    for (const l of doc.labels) {
      const k = l[0]?.toLowerCase();
      if (k && !m.has(k)) m.set(k, l);
    }
    return m;
  }, [doc.labels]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
        if (e.key === "Escape") (e.target as HTMLElement).blur();
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const k = e.key;
      if (k === " ") {
        e.preventDefault();
        // A focused gel button would also "click" on space: one toggle, not two.
        if (tag === "BUTTON") (e.target as HTMLElement).blur();
        player.toggle();
      } else if (k === "Tab") {
        e.preventDefault();
        step(e.shiftKey ? -1 : 1);
      } else if (k === "Backspace" || k === "Delete") {
        if (selected) {
          e.preventDefault();
          remove(selected.id);
        }
      } else if (k === "Enter") {
        if (selection) addRegion(doc.labels[0] ?? "other");
      } else if (k === "Escape") {
        setSelection(null);
        setSelectedId(null);
      } else if (k === "+" || k === "=") {
        wave.current?.zoomBy(1.5);
      } else if (k === "-" || k === "_") {
        wave.current?.zoomBy(1 / 1.5);
      } else if (k === "0") {
        wave.current?.fit();
      } else if (k === "ArrowLeft") {
        player.seek(state.position - (e.shiftKey ? 5 : 1));
      } else if (k === "ArrowRight") {
        player.seek(state.position + (e.shiftKey ? 5 : 1));
      } else if (k === "u" && selected) {
        patchSelected({ confidence: selected.confidence === "sure" ? "maybe" : "sure" });
      } else if (shortcuts.has(k.toLowerCase()) && !e.shiftKey) {
        applyLabel(shortcuts.get(k.toLowerCase())!);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Keep the selected row in view in the list.
  useEffect(() => {
    if (!selectedId || !listRef.current) return;
    listRef.current.querySelector<HTMLElement>(`[data-id="${selectedId}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [selectedId]);

  const setLabels = (text: string): void => {
    const labels = text.split(",").map((s) => s.trim()).filter(Boolean);
    update((d) => ({ ...d, labels: labels.length ? labels : [...DEFAULT_ANNOTATION_LABELS] }));
  };
  const [labelsText, setLabelsText] = useState(doc.labels.join(", "));

  const counts = useMemo(() => {
    const m = new Map<string, number>();
    for (const a of doc.annotations) m.set(a.label, (m.get(a.label) ?? 0) + 1);
    return m;
  }, [doc.annotations]);

  return (
    <div className="page annotate-page">
      <header className="trial-head">
        <div>
          <div className="crumbs">
            <Link to="/">Sessions</Link> › <Link to="/annotate">Annotate</Link> › {file}
          </div>
          <h1>
            {file} <span className="of">{doc.annotations.length} regions</span>
          </h1>
        </div>
        <div className="save-state">
          {save.isPending ? "Saving…" : save.isError ? "Save failed!" : dirty ? "Unsaved…" : "Saved"}
          {" · "}
          <a className="aqua-link" href={api.annotateExportUrl()}>
            CSV
          </a>
        </div>
      </header>

      <section className="panel glass">
        {state.error && <p className="error">{state.error}</p>}
        <AnnotateWave
          ref={wave}
          peaks={peaks}
          duration={state.duration}
          position={state.position}
          playing={state.playing}
          annotations={doc.annotations}
          labels={doc.labels}
          selectedId={selectedId}
          selection={selection}
          gain={gain}
          onSeek={player.seek}
          onSelection={setSelection}
          onSelect={setSelectedId}
          onChange={changeAnnotation}
        />
        <div className="transport">
          <button className="aqua-button round" onClick={player.toggle} disabled={state.loading} title="Space">
            {state.playing ? "❚❚" : "▶"}
          </button>
          <span className="time">
            {fmtTime(state.position)} / {fmtTime(state.duration)}
          </span>
          {state.loading && <span className="hint">decoding…</span>}
          {!state.loading && !peaks && !state.error && <span className="hint">building peaks…</span>}
          <span className="hint small">
            wave{" "}
            <span className="segmented tiny">
              {GAINS.map((g) => (
                <button key={g} className={`seg ${gain === g ? "on" : ""}`} onClick={() => setGain(g)}>
                  ×{g}
                </button>
              ))}
            </span>
          </span>
          <span className="segmented tiny">
            <button className="seg" onClick={() => wave.current?.zoomBy(1 / 1.5)} title="-">
              −
            </button>
            <button className="seg" onClick={() => wave.current?.fit()} title="0">
              fit
            </button>
            <button className="seg" onClick={() => wave.current?.zoomBy(1.5)} title="+">
              +
            </button>
          </span>
          {selection && (
            <span className="hint small">
              selection {fmtTime(selection[0])} – {fmtTime(selection[1])} ({((selection[1] - selection[0]) * 1000).toFixed(0)} ms)
            </span>
          )}
        </div>
        <div className="annot-labelbar">
          {doc.labels.map((l) => {
            const [r, g, b] = labelColour(doc.labels, l);
            const key = [...shortcuts.entries()].find(([, v]) => v === l)?.[0];
            const active = selected?.label === l;
            return (
              <button
                key={l}
                className={`tag label-tag ${active ? "on" : ""}`}
                style={{ ["--label" as string]: `rgb(${r},${g},${b})` }}
                disabled={!selection && !selected}
                onClick={() => applyLabel(l)}
                title={selection ? `Create ${l} region from selection` : `Label selected region ${l}`}
              >
                {key && <kbd>{key}</kbd>} {l}
                {counts.get(l) ? <span className="count">{counts.get(l)}</span> : null}
              </button>
            );
          })}
          <span className="hint keys">
            drag: select · drag edges/body: resize/move · letter or ⏎: make region · ⌫ delete · ⇥/⇧⇥ next/prev · u sure/maybe ·
            wheel/±/0 zoom · ←/→ seek · space play
          </span>
        </div>
      </section>

      <div className="annot-columns">
        <section className="panel metal-inset annot-inspector">
          {selected ? (
            <>
              <h2>
                Region <span className="hint">— {fmtTime(selected.start)} – {fmtTime(selected.end)} ({((selected.end - selected.start) * 1000).toFixed(0)} ms)</span>
              </h2>
              <div className="question">
                <div className="q-label">Confidence</div>
                <div className="segmented">
                  {(["sure", "maybe"] as const).map((c) => (
                    <button key={c} className={`seg ${selected.confidence === c ? "on" : ""}`} onClick={() => patchSelected({ confidence: c })}>
                      {c}
                    </button>
                  ))}
                </div>
              </div>
              <div className="question">
                <div className="q-label">Note</div>
                <input
                  className="aqua-input"
                  value={selected.note ?? ""}
                  placeholder="optional"
                  onChange={(e) => patchSelected({ note: e.target.value || undefined })}
                />
              </div>
              <div className="annot-actions">
                <button className="aqua-button small" onClick={() => goTo(selected)}>
                  play here
                </button>
                <button className="aqua-button small" onClick={() => remove(selected.id)}>
                  delete
                </button>
              </div>
            </>
          ) : selection ? (
            <>
              <h2>Selection</h2>
              <p className="hint">Press a label key (or click a label above) to make a region; ⏎ makes a “{doc.labels[0]}”.</p>
            </>
          ) : (
            <>
              <h2>Nothing selected</h2>
              <p className="hint">Drag on the waveform to select a span, or click a region.</p>
            </>
          )}
          <div className="question">
            <div className="q-label">Labels</div>
            <input
              className="aqua-input"
              value={labelsText}
              onChange={(e) => setLabelsText(e.target.value)}
              onBlur={() => setLabels(labelsText)}
              onKeyDown={(e) => e.key === "Enter" && setLabels(labelsText)}
              title="Comma-separated; the first letter of each is its shortcut"
            />
          </div>
        </section>

        <section className="panel metal-inset annot-list">
          <h2>
            Regions{" "}
            <span className="segmented tiny">
              <button className={`seg ${sortBy === "time" ? "on" : ""}`} onClick={() => setSortBy("time")}>
                time
              </button>
              <button className={`seg ${sortBy === "label" ? "on" : ""}`} onClick={() => setSortBy("label")}>
                label
              </button>
            </span>
          </h2>
          <div className="annot-rows" ref={listRef}>
            {sorted.length === 0 && <p className="hint">No regions yet.</p>}
            <table className="results small">
              <tbody>
                {sorted.map((a) => {
                  const [r, g, b] = labelColour(doc.labels, a.label);
                  return (
                    <tr
                      key={a.id}
                      data-id={a.id}
                      className={a.id === selectedId ? "selected" : ""}
                      onClick={() => goTo(a)}
                    >
                      <td className="mono">{fmtTime(a.start)}</td>
                      <td className="mono">{((a.end - a.start) * 1000).toFixed(0)} ms</td>
                      <td>
                        <span className="swatch" style={{ background: `rgb(${r},${g},${b})` }} />
                        {a.label}
                        {a.confidence === "maybe" ? "?" : ""}
                      </td>
                      <td className="note-cell">{a.note}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </section>
      </div>
    </div>
  );
}
