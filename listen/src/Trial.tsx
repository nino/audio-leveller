import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Answer, Results, Session, TrialResult } from "../../src/listen/types";
import { api } from "./api";
import { usePlayer } from "./player";
import { getListener } from "./routes";
import { Waveform } from "./Waveform";

const emptyTrial = (): TrialResult => ({ clips: {}, trial: {}, timeSpentMs: 0 });

const fmt = (s: number): string => {
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}:${r.toFixed(1).padStart(4, "0")}`;
};

export function TrialPage() {
  const { name, trial: trialParam } = useParams({ from: "/s/$name/$trial" });
  const trialIndex = Math.max(0, Number(trialParam) - 1);
  const listener = getListener();
  const navigate = useNavigate();
  const qc = useQueryClient();

  const session = useQuery({ queryKey: ["session", name], queryFn: () => api.session(name) });
  const saved = useQuery({
    queryKey: ["results", name, listener],
    queryFn: () => api.results(name, listener),
    enabled: !!listener,
  });

  if (!listener) {
    return (
      <div className="page">
        <p className="error">
          Enter your name on the <Link to="/">sessions page</Link> first.
        </p>
      </div>
    );
  }
  if (session.isLoading || saved.isLoading) return <div className="page hint">Loading…</div>;
  if (session.error || !session.data) return <div className="page error">{String(session.error)}</div>;
  const s = session.data;
  const trial = s.trials[trialIndex];
  if (!trial) return <div className="page error">No trial {trialParam}.</div>;

  return (
    <TrialView
      key={trial.id}
      session={s}
      trialIndex={trialIndex}
      listener={listener}
      initial={
        saved.data ?? {
          session: name,
          listener,
          startedAt: new Date().toISOString(),
          updatedAt: new Date().toISOString(),
          trials: {},
        }
      }
      onSaved={(r) => qc.setQueryData(["results", name, listener], r)}
      go={(i) => navigate({ to: "/s/$name/$trial", params: { name, trial: String(i + 1) } })}
      toResults={() => navigate({ to: "/s/$name/results", params: { name } })}
    />
  );
}

interface ViewProps {
  session: Session;
  trialIndex: number;
  listener: string;
  initial: Results;
  onSaved: (r: Results) => void;
  go: (index: number) => void;
  toResults: () => void;
}

function TrialView({ session, trialIndex, listener, initial, onSaved, go, toResults }: ViewProps) {
  const trial = session.trials[trialIndex];
  const urls = useMemo(() => trial.clips.map((c) => api.clipUrl(session.name, c.file)), [session.name, trial]);
  const player = usePlayer(urls);
  const { state } = player;
  // Snapshot every clip's samples as soon as they are decoded. Waveform used
  // to do this lazily per clip, which reopened the purge window peaks.ts
  // closes: a clip first viewed minutes into a session could already have had
  // its AudioBuffer storage reclaimed. Clips here are seconds long, so plain
  // float copies are fine.
  const waveData = useMemo(
    () => player.buffers.map((b) => Float32Array.from(b.getChannelData(0))),
    [player.buffers],
  );

  const [results, setResults] = useState<Results>(initial);
  const latest = useRef(results);
  latest.current = results;
  const answers = results.trials[trial.id] ?? emptyTrial();
  const [selectedClip, setSelectedClip] = useState(0);
  const openedAt = useRef(performance.now());

  // Persist with a short debounce; the whole results file is small.
  const save = useMutation({
    mutationFn: (r: Results) => api.saveResults(session.name, listener, r),
    onSuccess: (_d, r) => onSaved(r),
  });
  const timer = useRef<number>(0);
  const update = useCallback(
    (fn: (t: TrialResult) => TrialResult) => {
      const prev = latest.current;
      const cur = prev.trials[trial.id] ?? emptyTrial();
      const now = performance.now();
      const spent = now - openedAt.current;
      openedAt.current = now;
      const next: Results = {
        ...prev,
        updatedAt: new Date().toISOString(),
        trials: { ...prev.trials, [trial.id]: { ...fn(cur), timeSpentMs: cur.timeSpentMs + spent } },
      };
      latest.current = next;
      setResults(next);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => save.mutate(next), 400);
    },
    [save, trial.id],
  );

  const setClipAnswer = (label: string, q: string, a: Answer) =>
    update((t) => ({ ...t, clips: { ...t.clips, [label]: { ...(t.clips[label] ?? {}), [q]: a } } }));
  const setTrialAnswer = (q: string, a: Answer) => update((t) => ({ ...t, trial: { ...t.trial, [q]: a } }));

  // Selecting a clip both plays it and shows its questions.
  const choose = useCallback(
    (i: number) => {
      setSelectedClip(i);
      player.select(i);
    },
    [player],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (e.key === " ") {
        e.preventDefault();
        player.toggle();
      } else if (/^[1-9]$/.test(e.key)) {
        choose(Number(e.key) - 1);
      } else if (e.key.toLowerCase() === "l") {
        player.setLoop(!state.loop);
      } else if (e.key === "ArrowLeft") {
        player.seek(state.position - 2);
      } else if (e.key === "ArrowRight") {
        player.seek(state.position + 2);
      } else if (e.key === "Escape") {
        player.setRegion(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [choose, player, state.loop, state.position]);

  const clip = trial.clips[selectedClip];
  const clipAnswers = answers.clips[clip.label] ?? {};
  const answeredClips = trial.clips.filter((c) =>
    session.clipQuestions.some((q) => q.kind === "scale" && answers.clips[c.label]?.[q.id] != null),
  ).length;
  const complete = answeredClips === trial.clips.length;
  const isLast = trialIndex === session.trials.length - 1;

  return (
    <div className="page trial">
      <header className="trial-head">
        <div>
          <div className="crumbs">
            <Link to="/">Sessions</Link> › {session.name}
          </div>
          <h1>
            Trial {trialIndex + 1} <span className="of">of {session.trials.length}</span>
          </h1>
          <div className="trial-title">{trial.title}</div>
        </div>
        <div className="save-state">{save.isPending ? "Saving…" : save.isError ? "Save failed!" : "Saved"}</div>
      </header>

      <section className="panel glass">
        {state.error && <p className="error">{state.error}</p>}
        <Waveform
          data={waveData[selectedClip] ?? null}
          position={state.position}
          duration={state.duration}
          region={state.region}
          onSeek={player.seek}
          onRegion={player.setRegion}
        />
        <div className="transport">
          <button className="aqua-button round" onClick={player.toggle} disabled={state.loading} title="Space">
            {state.playing ? "❚❚" : "▶"}
          </button>
          <span className="time">
            {fmt(state.position)} / {fmt(state.duration)}
          </span>
          <label className="check">
            <input type="checkbox" checked={state.loop} onChange={(e) => player.setLoop(e.target.checked)} />
            loop
          </label>
          {state.region && (
            <button className="aqua-button small" onClick={() => player.setRegion(null)}>
              clear region
            </button>
          )}
          <span className="hint keys">1–{trial.clips.length} switch · space play · L loop · ←/→ seek · drag on wave to loop a region</span>
        </div>
        <div className="clip-buttons">
          {trial.clips.map((c, i) => {
            const done = session.clipQuestions.some(
              (q) => q.kind === "scale" && answers.clips[c.label]?.[q.id] != null,
            );
            return (
              <button
                key={c.label}
                className={`aqua-button clip ${i === selectedClip ? "active" : ""} ${done ? "done" : ""}`}
                onClick={() => choose(i)}
                disabled={state.loading}
              >
                <span className="letter">{c.label}</span>
                {i === selectedClip && state.playing && <span className="playing-dot" />}
              </button>
            );
          })}
          {state.loading && <span className="hint">decoding clips…</span>}
        </div>
      </section>

      <section className="panel metal-inset">
        <h2>
          Clip {clip.label}
          <span className="hint"> — rate what you hear in this clip</span>
        </h2>
        {session.clipQuestions.map((q) => (
          <div className="question" key={q.id}>
            <div className="q-label">{q.label}</div>
            {q.kind === "scale" && (
              <div className="segmented">
                {q.minLabel && <span className="seg-cap">{q.minLabel}</span>}
                {Array.from({ length: q.max - q.min + 1 }, (_, k) => q.min + k).map((v) => (
                  <button
                    key={v}
                    className={`seg ${clipAnswers[q.id] === v ? "on" : ""}`}
                    onClick={() => setClipAnswer(clip.label, q.id, v)}
                  >
                    {v}
                  </button>
                ))}
                {q.maxLabel && <span className="seg-cap">{q.maxLabel}</span>}
              </div>
            )}
            {q.kind === "tags" && (
              <div className="tags">
                {q.options.map((opt) => {
                  const cur = (clipAnswers[q.id] as string[] | undefined) ?? [];
                  const on = cur.includes(opt);
                  return (
                    <button
                      key={opt}
                      className={`tag ${on ? "on" : ""}`}
                      onClick={() =>
                        setClipAnswer(clip.label, q.id, on ? cur.filter((x) => x !== opt) : [...cur, opt])
                      }
                    >
                      {opt}
                    </button>
                  );
                })}
              </div>
            )}
            {q.kind === "text" && (
              <textarea
                className="aqua-input"
                rows={2}
                value={(clipAnswers[q.id] as string | undefined) ?? ""}
                onChange={(e) => setClipAnswer(clip.label, q.id, e.target.value)}
              />
            )}
          </div>
        ))}
      </section>

      <section className="panel metal-inset">
        <h2>
          The whole set <span className="hint">— {answeredClips}/{trial.clips.length} clips rated</span>
        </h2>
        {session.trialQuestions.map((q) => (
          <div className="question" key={q.id}>
            <div className="q-label">{q.label}</div>
            {q.kind === "pick" && (
              <div className="segmented">
                {trial.clips.map((c) => (
                  <button
                    key={c.label}
                    className={`seg ${answers.trial[q.id] === c.label ? "on" : ""}`}
                    onClick={() => setTrialAnswer(q.id, c.label)}
                  >
                    {c.label}
                  </button>
                ))}
                <button
                  className={`seg ${answers.trial[q.id] === "same" ? "on" : ""}`}
                  onClick={() => setTrialAnswer(q.id, "same")}
                >
                  can't tell
                </button>
              </div>
            )}
            {q.kind === "text" && (
              <textarea
                className="aqua-input"
                rows={2}
                value={(answers.trial[q.id] as string | undefined) ?? ""}
                onChange={(e) => setTrialAnswer(q.id, e.target.value)}
              />
            )}
          </div>
        ))}
      </section>

      <footer className="trial-nav">
        <button className="aqua-button" disabled={trialIndex === 0} onClick={() => go(trialIndex - 1)}>
          ‹ Previous
        </button>
        <span className="hint">{complete ? "All clips rated." : "Rate every clip before moving on (you can come back)."}</span>
        {isLast ? (
          <button className="aqua-button primary" onClick={toResults}>
            Finish & see results
          </button>
        ) : (
          <button className="aqua-button primary" onClick={() => go(trialIndex + 1)}>
            Next ›
          </button>
        )}
      </footer>
    </div>
  );
}
