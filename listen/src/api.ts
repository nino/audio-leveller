import type { Results, Session, SessionKey } from "../../src/listen/types";

export interface SessionSummary {
  name: string;
  createdAt: string;
  trials: number;
  results: { listener: string; done: number; updatedAt: string }[];
}

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (!res.ok) throw new Error(`${init?.method ?? "GET"} ${url}: ${res.status}`);
  return (await res.json()) as T;
}

export const api = {
  sessions: () => json<SessionSummary[]>("/api/sessions"),
  session: (name: string) => json<Session>(`/api/sessions/${name}`),
  key: (name: string) => json<SessionKey>(`/api/sessions/${name}/key`),
  clipUrl: (name: string, file: string) => `/api/sessions/${name}/clip/${file}`,
  exportUrl: (name: string) => `/api/sessions/${name}/export.csv`,
  results: async (name: string, listener: string): Promise<Results | null> => {
    const res = await fetch(`/api/sessions/${name}/results/${listener}`);
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`results: ${res.status}`);
    return (await res.json()) as Results;
  },
  saveResults: (name: string, listener: string, results: Results) =>
    json<{ ok: true }>(`/api/sessions/${name}/results/${listener}`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(results, null, 2),
    }),
};
