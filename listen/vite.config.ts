/**
 * The listening-test app: a Vite + React front end, and a tiny API served by
 * the Vite dev server itself so there is nothing else to run.
 *
 *   pnpm listen            # opens the app
 *
 * Sessions live in `listening/sessions/<name>/` (gitignored); results are
 * written back there as `results.<listener>.json`.
 */

import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { createReadStream, existsSync } from "node:fs";
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";

const ROOT = resolve(__dirname, "..");
const SESSIONS = join(ROOT, "listening", "sessions");

const send = (res: ServerResponse, status: number, body: unknown): void => {
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify(body));
};

const readBody = (req: IncomingMessage): Promise<string> =>
  new Promise((ok, fail) => {
    let data = "";
    req.on("data", (c) => (data += c));
    req.on("end", () => ok(data));
    req.on("error", fail);
  });

/** Only ever touch files inside SESSIONS, and only by simple names. */
const safeName = (s: string): string => {
  if (!/^[\w.-]+$/.test(s)) throw new Error(`bad name: ${s}`);
  return s;
};

async function listSessions(): Promise<unknown[]> {
  if (!existsSync(SESSIONS)) return [];
  const out: unknown[] = [];
  for (const dir of await readdir(SESSIONS, { withFileTypes: true })) {
    if (!dir.isDirectory()) continue;
    const path = join(SESSIONS, dir.name, "session.json");
    if (!existsSync(path)) continue;
    const session = JSON.parse(await readFile(path, "utf8"));
    const files = await readdir(join(SESSIONS, dir.name));
    const results: unknown[] = [];
    for (const f of files) {
      const m = f.match(/^results\.(.+)\.json$/);
      if (!m) continue;
      const r = JSON.parse(await readFile(join(SESSIONS, dir.name, f), "utf8"));
      results.push({ listener: m[1], done: Object.keys(r.trials ?? {}).length, updatedAt: r.updatedAt });
    }
    out.push({
      name: session.name,
      createdAt: session.createdAt,
      trials: session.trials.length,
      results,
    });
  }
  return out;
}

async function exportCsv(name: string): Promise<string> {
  const dir = join(SESSIONS, name);
  const key = JSON.parse(await readFile(join(dir, "key.json"), "utf8"));
  const rows = [
    ["session", "listener", "trial", "window_start_s", "clip", "variant", "question", "answer"].join(","),
  ];
  const q = (v: unknown): string => `"${String(v ?? "").replace(/"/g, '""')}"`;
  for (const f of await readdir(dir)) {
    const m = f.match(/^results\.(.+)\.json$/);
    if (!m) continue;
    const r = JSON.parse(await readFile(join(dir, f), "utf8"));
    for (const [trialId, tr] of Object.entries<any>(r.trials ?? {})) {
      const start = key.windows?.[trialId]?.start ?? "";
      for (const [label, answers] of Object.entries<any>(tr.clips ?? {})) {
        const variant = key.clips?.[trialId]?.[label] ?? "";
        for (const [qid, ans] of Object.entries(answers)) {
          const a = Array.isArray(ans) ? ans.join("|") : ans;
          rows.push([name, m[1], trialId, start, label, variant, qid, q(a)].join(","));
        }
      }
      for (const [qid, ans] of Object.entries<any>(tr.trial ?? {})) {
        // A "pick" answer is a clip label; translate to the variant as well.
        const variant = typeof ans === "string" ? key.clips?.[trialId]?.[ans] ?? "" : "";
        rows.push([name, m[1], trialId, start, "", variant, qid, q(ans)].join(","));
      }
    }
  }
  return rows.join("\n") + "\n";
}

function api(): Plugin {
  return {
    name: "listening-api",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = new URL(req.url ?? "/", "http://x");
        if (!url.pathname.startsWith("/api/")) return next();
        try {
          const parts = url.pathname.split("/").filter(Boolean).slice(1); // drop "api"
          if (parts[0] === "sessions" && parts.length === 1) return send(res, 200, await listSessions());
          if (parts[0] !== "sessions") return send(res, 404, { error: "not found" });
          const name = safeName(parts[1]);
          const dir = join(SESSIONS, name);
          if (parts.length === 2) {
            return send(res, 200, JSON.parse(await readFile(join(dir, "session.json"), "utf8")));
          }
          if (parts[2] === "clip") {
            const file = safeName(parts[3]);
            res.setHeader("Content-Type", "audio/wav");
            createReadStream(join(dir, file)).on("error", () => send(res, 404, { error: "no clip" })).pipe(res);
            return;
          }
          if (parts[2] === "key") {
            return send(res, 200, JSON.parse(await readFile(join(dir, "key.json"), "utf8")));
          }
          if (parts[2] === "export.csv") {
            res.setHeader("Content-Type", "text/csv");
            res.setHeader("Content-Disposition", `attachment; filename="${name}-results.csv"`);
            res.end(await exportCsv(name));
            return;
          }
          if (parts[2] === "results") {
            const listener = safeName(parts[3]);
            const path = join(dir, `results.${listener}.json`);
            if (req.method === "PUT") {
              const body = await readBody(req);
              JSON.parse(body); // validate
              await mkdir(dir, { recursive: true });
              await writeFile(path, body);
              return send(res, 200, { ok: true });
            }
            if (!existsSync(path)) return send(res, 404, { error: "no results yet" });
            return send(res, 200, JSON.parse(await readFile(path, "utf8")));
          }
          send(res, 404, { error: "not found" });
        } catch (err) {
          send(res, 500, { error: err instanceof Error ? err.message : String(err) });
        }
      });
    },
  };
}

export default defineConfig({
  root: __dirname,
  plugins: [react(), api()],
  server: { port: 5199, open: true },
  build: { outDir: join(ROOT, "dist", "listen-app"), emptyOutDir: true },
});

