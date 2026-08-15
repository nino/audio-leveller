import {
  createRootRoute,
  createRoute,
  createRouter,
  Link,
  Outlet,
  useNavigate,
} from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "./api";
import { TrialPage } from "./Trial";
import { ResultsPage } from "./Results";

const LISTENER_KEY = "listening.listener";
export const getListener = (): string => localStorage.getItem(LISTENER_KEY) ?? "";
export const setListener = (name: string): void => localStorage.setItem(LISTENER_KEY, name);

function Shell() {
  return (
    <div className="desktop">
      <div className="window">
        <div className="titlebar">
          <div className="lights">
            <Link to="/" className="light red" title="Sessions" />
            <span className="light yellow" />
            <span className="light green" />
          </div>
          <div className="title">Listening Test</div>
        </div>
        <div className="content">
          <Outlet />
        </div>
      </div>
    </div>
  );
}

function SessionsPage() {
  const sessions = useQuery({ queryKey: ["sessions"], queryFn: api.sessions });
  const [listener, setName] = useState(getListener());
  const navigate = useNavigate();
  useEffect(() => setListener(listener), [listener]);

  return (
    <div className="page">
      <h1>Sessions</h1>
      <p className="hint">
        Each session is a folder in <code>listening/sessions/</code>. Build one with{" "}
        <code>pnpm listen:session &lt;spec.json&gt;</code>.
      </p>
      <label className="field">
        <span>Your name</span>
        <input
          className="aqua-input"
          value={listener}
          placeholder="listener"
          onChange={(e) => setName(e.target.value.replace(/[^\w.-]/g, ""))}
        />
      </label>
      {sessions.isLoading && <p className="hint">Loading…</p>}
      {sessions.error && <p className="error">{String(sessions.error)}</p>}
      {sessions.data && sessions.data.length === 0 && <p className="hint">No sessions yet.</p>}
      <ul className="session-list">
        {sessions.data?.map((s) => {
          const mine = s.results.find((r) => r.listener === listener);
          return (
            <li key={s.name} className="session-row metal-inset">
              <div className="session-main">
                <div className="session-name">{s.name}</div>
                <div className="session-meta">
                  {s.trials} trials · created {new Date(s.createdAt).toLocaleDateString()}
                  {s.results.length > 0 && (
                    <>
                      {" "}
                      · scored by{" "}
                      {s.results.map((r) => `${r.listener} (${r.done}/${s.trials})`).join(", ")}
                    </>
                  )}
                </div>
              </div>
              <div className="session-actions">
                <button
                  className="aqua-button primary"
                  disabled={!listener}
                  onClick={() => navigate({ to: "/s/$name/$trial", params: { name: s.name, trial: String((mine?.done ?? 0) + 1 > s.trials ? 1 : (mine?.done ?? 0) + 1) } })}
                >
                  {mine ? (mine.done >= s.trials ? "Review" : "Continue") : "Start"}
                </button>
                <button
                  className="aqua-button"
                  onClick={() => navigate({ to: "/s/$name/results", params: { name: s.name } })}
                >
                  Results
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

const rootRoute = createRootRoute({ component: Shell });
const indexRoute = createRoute({ getParentRoute: () => rootRoute, path: "/", component: SessionsPage });
const trialRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/s/$name/$trial",
  component: TrialPage,
});
const resultsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/s/$name/results",
  component: ResultsPage,
});

export const router = createRouter({
  routeTree: rootRoute.addChildren([indexRoute, trialRoute, resultsRoute]),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
