/**
 * The Audio Leveller window: the title strip, the drop zone, the run's status,
 * the presets and the chain.
 *
 * It holds no state of its own beyond the latest `View`. Every click is an
 * `Input` sent to the shell, and whatever comes back is drawn; drops, focus and
 * progress arrive from the shell as `view` events. So what is on screen is
 * always what `leveller-ui` decided, and its tests cover the behaviour.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { shell, type Input, type ParamView, type StageView, type Summary, type View } from "./ipc";
import { basename, formatValue, pluralize } from "./format";

/** The latest view, and a way to send an input and draw its reply. */
function useShell(): [View | null, (input: Input) => void, () => void] {
  const [view, setView] = useState<View | null>(null);
  // Replies and events can interleave. Each carries the whole view, so the
  // newest one to arrive is drawn and a stale reply is dropped.
  const latest = useRef(0);
  const apply = useCallback((promise: Promise<View>) => {
    const ticket = ++latest.current;
    promise
      .then((next) => {
        if (ticket === latest.current) setView(next);
      })
      .catch((error: unknown) => console.error(error));
  }, []);

  useEffect(() => {
    const unlisten = shell.onView((next) => {
      latest.current++;
      setView(next);
    });
    apply(shell.view());
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [apply]);

  const send = useCallback((input: Input) => apply(shell.send(input)), [apply]);
  const choose = useCallback(() => apply(shell.chooseFile()), [apply]);
  return [view, send, choose];
}

export function App() {
  const [view, send, choose] = useShell();

  useEffect(() => {
    document.body.classList.toggle("inactive", view ? !view.windowActive : false);
  }, [view?.windowActive]);

  return (
    <div className="window">
      <TitleBar />
      {view && (
        <div className="content">
          <DropZone view={view} onChoose={choose} />
          <Status view={view} />
          <Presets view={view} send={send} />
          <section className="panel metal-inset">
            <h2>Pipeline</h2>
            <ul className="chain">
              {view.stages.map((stage, index) => (
                <Stage
                  key={stage.name}
                  stage={stage}
                  index={index}
                  line={doneSummary(view)?.stages.find((line) => line.name === stage.name)}
                  send={send}
                />
              ))}
            </ul>
          </section>
        </div>
      )}
    </div>
  );
}

function doneSummary(view: View): Summary | null {
  return view.status.kind === "done" ? view.status.summary : null;
}

function TitleBar() {
  const window = getCurrentWindow();
  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="lights">
        <button className="light red" aria-label="Close" onClick={() => void window.close()} />
        <button className="light yellow" aria-label="Minimise" onClick={() => void window.minimize()} />
        <button className="light green" aria-label="Zoom" onClick={() => void window.toggleMaximize()} />
      </div>
      <span className="title" data-tauri-drag-region>
        Audio Leveller
      </span>
    </div>
  );
}

function DropZone({ view, onChoose }: { view: View; onChoose: () => void }) {
  const busy = view.status.kind === "working";
  const classes = ["dropzone", view.dragOver && !busy ? "hover" : "", busy ? "busy" : ""];
  return (
    <button
      className={classes.filter(Boolean).join(" ")}
      disabled={busy}
      onClick={onChoose}
      aria-label="Choose a .wav file to level, or drop one here"
    >
      <div className="icon" aria-hidden>
        🎚️
      </div>
      <h1>Audio Leveller</h1>
      <p className="lede">
        Drop a <strong>.wav</strong> file here
      </p>
      <p className="sub">Each speech segment is normalised to its target loudness</p>
    </button>
  );
}

function Status({ view }: { view: View }) {
  const status = view.status;
  switch (status.kind) {
    case "idle":
      return null;

    case "working": {
      const step = status.total > 0 ? ` – ${status.stage}, step ${status.index + 1} of ${status.total}` : "…";
      return (
        <section className="panel glass status working" role="status">
          <div className="working-line">
            <span className="spinner" />
            <span>
              <strong>{status.file}</strong>
              {step}
            </span>
          </div>
          <div
            className="bar"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(status.overall * 100)}
          >
            <span style={{ width: `${Math.round(status.overall * 100)}%` }} />
          </div>
        </section>
      );
    }

    case "failed":
      return (
        <section className="panel glass status error" role="alert">
          Failed: {status.message}
        </section>
      );

    case "done": {
      const s = status.summary;
      return (
        <section className="panel glass status done" role="status">
          <div className="done">Wrote {basename(s.outputPath)}</div>
          {/* No preset name here: the presets can change after the run, and
              the line would then name settings the file was not made with. */}
          <div className="meta">
            {s.inputLufs.toFixed(1)} LUFS → {s.outputLufs.toFixed(1)} LUFS · peak {s.outputPeakDbfs.toFixed(1)} dBFS ·{" "}
            {s.sampleRate} Hz · {s.durationSec.toFixed(1)}s
          </div>
          <div className="meta">
            {pluralize(s.silences, "silence")} · {pluralize(s.segments, "segment")}
          </div>
          <div className="meta">
            {s.roomTonePath ? (
              <>
                Wrote <strong>{basename(s.roomTonePath)}</strong> – {s.roomToneSec.toFixed(1)}s room tone
              </>
            ) : (
              "No room tone (no usable silence found)"
            )}
          </div>
        </section>
      );
    }
  }
}

function Presets({ view, send }: { view: View; send: (input: Input) => void }) {
  return (
    <section className="panel metal-inset">
      <div className="preset-row">
        <h2>Preset</h2>
        <div className="segmented" role="radiogroup" aria-label="Preset">
          {view.presets.map((preset) => (
            <button
              key={preset.name}
              className={`seg${preset.name === view.preset ? " on" : ""}`}
              role="radio"
              aria-checked={preset.name === view.preset}
              title={preset.description}
              onClick={() => send({ kind: "preset", name: preset.name })}
            >
              {preset.name}
            </button>
          ))}
        </div>
        <button className="aqua-button small" disabled={!view.edited} onClick={() => send({ kind: "revert" })}>
          Revert
        </button>
        <span className="spacer" />
        <button
          className={`aqua-button${view.canRerender ? " primary" : ""}`}
          disabled={!view.canRerender}
          onClick={() => send({ kind: "rerender" })}
        >
          Re-render
        </button>
      </div>
      <p className="preset-note">
        {view.presetDescription}
        {view.edited && <span className="edited"> · edited</span>}
        {view.bypassed.length > 0 && <span className="dim"> · bypassing {view.bypassed.join(", ")}</span>}
      </p>
    </section>
  );
}

/**
 * One row per stage: a checkbox that bypasses it, a disclosure triangle for
 * its parameters, and a note that says what the stage did once a file has
 * been through it.
 */
function Stage({
  stage,
  index,
  line,
  send,
}: {
  stage: StageView;
  index: number;
  line: Summary["stages"][number] | undefined;
  send: (input: Input) => void;
}) {
  const note = !line
    ? stage.description
    : line.enabled
      ? `${line.decision} · ${line.elapsedMs.toFixed(0)} ms`
      : "bypassed";
  return (
    <li>
      <div className={`stage-head${stage.enabled ? "" : " off"}`}>
        <button
          className="aqua-button disclose"
          title={stage.description}
          disabled={stage.params.length === 0}
          aria-expanded={stage.expanded}
          aria-label={`${stage.label} parameters`}
          onClick={() => send({ kind: "toggleExpanded", index })}
        >
          {stage.expanded ? "▼" : "▶"}
        </button>
        <label>
          <input
            type="checkbox"
            checked={stage.enabled}
            onChange={() => send({ kind: "toggleStage", index })}
          />
          <span>{stage.label}</span>
        </label>
        <span className="stage-note">{note}</span>
      </div>
      {stage.expanded && (
        <div className="stage-params">
          {stage.params.map((param) => (
            <Param key={param.key} stage={stage.name} param={param} send={send} />
          ))}
        </div>
      )}
    </li>
  );
}

function Param({ stage, param, send }: { stage: string; param: ParamView; send: (input: Input) => void }) {
  const set = (value: unknown) => send({ kind: "setParam", stage, key: param.key, value });
  const id = `${stage}-${param.key}`;

  let control;
  switch (param.kind) {
    case "number": {
      const value = Number(param.value);
      control = (
        <>
          <input
            id={id}
            type="range"
            min={param.min}
            max={param.max}
            step={param.step}
            value={value}
            onChange={(event) => set(Number(event.target.value))}
          />
          <span className="value">{formatValue(value, param.step, param.unit)}</span>
        </>
      );
      break;
    }
    case "boolean": {
      const on = param.value === true;
      control = (
        <label className="check-value">
          <input id={id} type="checkbox" checked={on} onChange={(event) => set(event.target.checked)} />
          <span>{on ? "on" : "off"}</span>
        </label>
      );
      break;
    }
    case "choice":
      control = (
        <select
          id={id}
          className="aqua-select"
          value={String(param.value)}
          onChange={(event) => set(event.target.value)}
        >
          {param.options.map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      );
      break;
  }

  return (
    <div className={`param${param.edited ? " changed" : ""}`} title={param.help}>
      <label className="name" htmlFor={id}>
        {param.label}
      </label>
      {control}
    </div>
  );
}
