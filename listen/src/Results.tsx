import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { useState } from "react";
import { api } from "./api";
import { getListener } from "./routes";

interface VariantStat {
  variant: string;
  n: number;
  scores: Record<string, number[]>; // scale question → values
  picks: number;
  tags: Record<string, number>;
  notes: string[];
}

export function ResultsPage() {
  const { name } = useParams({ from: "/s/$name/results" });
  const listener = getListener();
  const [revealed, setRevealed] = useState(false);
  const session = useQuery({ queryKey: ["session", name], queryFn: () => api.session(name) });
  const results = useQuery({
    queryKey: ["results", name, listener],
    queryFn: () => api.results(name, listener),
    enabled: !!listener,
  });
  const key = useQuery({ queryKey: ["key", name], queryFn: () => api.key(name), enabled: revealed });

  if (session.isLoading || results.isLoading) return <div className="page hint">Loading…</div>;
  if (!session.data) return <div className="page error">{String(session.error)}</div>;
  const s = session.data;
  const r = results.data;
  const done = r ? Object.keys(r.trials).length : 0;

  const scaleQs = s.clipQuestions.filter((q) => q.kind === "scale");
  const tagQs = s.clipQuestions.filter((q) => q.kind === "tags");
  const pickQ = s.trialQuestions.find((q) => q.kind === "pick");

  let stats: VariantStat[] = [];
  if (revealed && key.data && r) {
    const byVariant = new Map<string, VariantStat>();
    const stat = (v: string): VariantStat => {
      let x = byVariant.get(v);
      if (!x) {
        x = { variant: v, n: 0, scores: {}, picks: 0, tags: {}, notes: [] };
        byVariant.set(v, x);
      }
      return x;
    };
    for (const [trialId, tr] of Object.entries(r.trials)) {
      const map = key.data.clips[trialId] ?? {};
      for (const [label, ans] of Object.entries(tr.clips)) {
        const v = map[label];
        if (!v) continue;
        const st = stat(v);
        st.n++;
        for (const q of scaleQs) {
          const val = ans[q.id];
          if (typeof val === "number") (st.scores[q.id] ??= []).push(val);
        }
        for (const q of tagQs) {
          for (const t of (ans[q.id] as string[] | undefined) ?? []) st.tags[t] = (st.tags[t] ?? 0) + 1;
        }
        const note = ans.notes;
        if (typeof note === "string" && note.trim()) st.notes.push(`${trialId}: ${note.trim()}`);
      }
      if (pickQ) {
        const p = tr.trial[pickQ.id];
        if (typeof p === "string" && map[p]) stat(map[p]).picks++;
      }
    }
    stats = [...byVariant.values()].sort((a, b) => {
      const qa = scaleQs[0]?.id;
      const ma = qa ? mean(a.scores[qa]) : 0, mb = qa ? mean(b.scores[qa]) : 0;
      return ma - mb;
    });
  }

  return (
    <div className="page">
      <div className="crumbs">
        <Link to="/">Sessions</Link> › {s.name}
      </div>
      <h1>Results</h1>
      <p className="hint">
        {listener ? `${listener}: ${done} of ${s.trials.length} trials scored.` : "No listener name set."}{" "}
        <a className="aqua-link" href={api.exportUrl(s.name)}>
          Export all listeners as CSV
        </a>
      </p>

      {!revealed ? (
        <div className="panel glass reveal">
          <p>
            The key is sealed. Revealing shows which variant each blind clip was – do it once you have
            finished scoring, because it is hard to un-know.
          </p>
          <button className="aqua-button primary" onClick={() => setRevealed(true)}>
            Reveal the key
          </button>
          {done < s.trials.length && (
            <Link className="aqua-button" to="/s/$name/$trial" params={{ name: s.name, trial: String(done + 1) }}>
              Keep scoring (trial {done + 1})
            </Link>
          )}
        </div>
      ) : key.isLoading ? (
        <p className="hint">Loading key…</p>
      ) : key.data ? (
        <>
          <section className="panel metal-inset">
            <h2>Per variant</h2>
            <table className="results">
              <thead>
                <tr>
                  <th>variant</th>
                  <th>clips</th>
                  {scaleQs.map((q) => (
                    <th key={q.id}>mean {q.label.toLowerCase()}</th>
                  ))}
                  {pickQ && <th>picked best</th>}
                  <th>tags</th>
                </tr>
              </thead>
              <tbody>
                {stats.map((st) => (
                  <tr key={st.variant}>
                    <td>
                      <b>{st.variant}</b>
                      <div className="hint small">{key.data.variants[st.variant]?.description ?? ""}</div>
                    </td>
                    <td>{st.n}</td>
                    {scaleQs.map((q) => (
                      <td key={q.id}>{st.scores[q.id]?.length ? mean(st.scores[q.id]).toFixed(2) : "–"}</td>
                    ))}
                    {pickQ && <td>{st.picks}</td>}
                    <td className="hint small">
                      {Object.entries(st.tags)
                        .sort((a, b) => b[1] - a[1])
                        .map(([t, n]) => `${t}×${n}`)
                        .join(", ")}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>

          <section className="panel metal-inset">
            <h2>Per trial</h2>
            {s.trials.map((t) => {
              const tr = r?.trials[t.id];
              const map = key.data.clips[t.id] ?? {};
              return (
                <div key={t.id} className="trial-block">
                  <div className="q-label">
                    {t.id} – {t.title}
                    {pickQ && tr?.trial[pickQ.id] && (
                      <span className="hint">
                        {" "}
                        · picked {String(tr.trial[pickQ.id])}
                        {map[String(tr.trial[pickQ.id])] ? ` = ${map[String(tr.trial[pickQ.id])]}` : ""}
                      </span>
                    )}
                  </div>
                  <table className="results small">
                    <tbody>
                      {t.clips.map((c) => {
                        const a = tr?.clips[c.label] ?? {};
                        return (
                          <tr key={c.label}>
                            <td>
                              <b>{c.label}</b>
                            </td>
                            <td>{map[c.label]}</td>
                            {scaleQs.map((q) => (
                              <td key={q.id}>{a[q.id] ?? "–"}</td>
                            ))}
                            <td className="hint">{((a.artefacts as string[] | undefined) ?? []).join(", ")}</td>
                            <td className="hint">{(a.notes as string | undefined) ?? ""}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                  {tr?.trial.comment ? <div className="hint small">“{String(tr.trial.comment)}”</div> : null}
                </div>
              );
            })}
          </section>

          {stats.some((st) => st.notes.length) && (
            <section className="panel metal-inset">
              <h2>Notes by variant</h2>
              {stats.map((st) =>
                st.notes.length ? (
                  <div key={st.variant} className="trial-block">
                    <div className="q-label">{st.variant}</div>
                    <ul className="notes">
                      {st.notes.map((n, i) => (
                        <li key={i}>{n}</li>
                      ))}
                    </ul>
                  </div>
                ) : null,
              )}
            </section>
          )}
        </>
      ) : (
        <p className="error">{String(key.error)}</p>
      )}
    </div>
  );
}

const mean = (xs: number[] | undefined): number =>
  xs && xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN;
