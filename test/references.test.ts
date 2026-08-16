/**
 * The reference manifest is the only guarantee that a fixture is the same
 * audio it was last time, so a typo in it is a silent loss of that guarantee.
 * These check the shape rather than the audio, which is not in the repository.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

interface ManifestFile {
  name: string;
  role: string;
  sha256: string;
  description: string;
}

interface ManifestSet {
  id: string;
  description: string;
  baseUrl: string;
  format: string;
  files: ManifestFile[];
}

const ROLES = new Set(["input", "reference", "archive"]);

const manifest = JSON.parse(
  readFileSync(join(process.cwd(), "eval/references/manifest.json"), "utf8"),
) as { sets: ManifestSet[] };

describe("reference manifest", () => {
  it("has at least one set, each with files", () => {
    expect(manifest.sets.length).toBeGreaterThan(0);
    for (const set of manifest.sets) expect(set.files.length).toBeGreaterThan(0);
  });

  it("gives every file a known role", () => {
    for (const set of manifest.sets) {
      for (const file of set.files) {
        expect(ROLES.has(file.role), `${file.name} has role "${file.role}"`).toBe(true);
      }
    }
  });

  it("pins a full SHA-256 for every file", () => {
    for (const set of manifest.sets) {
      for (const file of set.files) {
        expect(file.sha256, file.name).toMatch(/^[0-9a-f]{64}$/);
      }
    }
  });

  it("names files that resolve to absolute URLs under the set's base", () => {
    for (const set of manifest.sets) {
      expect(set.baseUrl).toMatch(/^https:\/\/.+\/$/);
      for (const file of set.files) {
        const url = new URL(file.name, set.baseUrl).href;
        expect(url.startsWith(set.baseUrl), `${file.name} escapes the base URL`).toBe(true);
      }
    }
  });

  it("keeps names unique and free of path separators", () => {
    const seen = new Set<string>();
    for (const set of manifest.sets) {
      for (const file of set.files) {
        expect(file.name).not.toMatch(/[/\\]/);
        expect(seen.has(file.name), `${file.name} appears twice`).toBe(false);
        seen.add(file.name);
      }
    }
  });

  it("says what every file and set is for", () => {
    for (const set of manifest.sets) {
      expect(set.description.length).toBeGreaterThan(20);
      for (const file of set.files) expect(file.description.length).toBeGreaterThan(20);
    }
  });

  it("keeps at least one input, so a fetched set gives the harness something to run", () => {
    for (const set of manifest.sets) {
      expect(set.files.some((f) => f.role === "input"), `${set.id} has no input`).toBe(true);
    }
  });
});
