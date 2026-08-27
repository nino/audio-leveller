/**
 * Download the real recordings the evaluation harness can be run against.
 *
 *   pnpm fetch-fixtures [set-id]     (default: every set in the manifest)
 *
 * The audio is not in the repository — it is hundreds of megabytes and it is
 * someone's actual voice — so what lives here instead is a manifest of URLs
 * and SHA-256 checksums. That is the whole point of the exercise: a fixture
 * whose bytes can drift silently is worthless as a regression baseline, and
 * "it sounded better on my machine" is not a result anyone can check. If a
 * download does not match its pinned hash, this refuses to install it.
 *
 * Where files land depends on their role, which the harness treats differently:
 *
 *   input      → eval/fixtures/    picked up automatically as `fixture:<name>`
 *   reference  → eval/references/  never processed; something to compare against
 *   archive    → eval/references/  historical output, kept for the same reason
 *
 * References must not sit in `eval/fixtures/`, or the harness would enrol
 * somebody else's master as a recording to be mastered.
 */

import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, readFile, rename, rm, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = join(root, "eval", "references", "manifest.json");

const DESTINATIONS = {
  input: join(root, "eval", "fixtures"),
  reference: join(root, "eval", "references"),
  archive: join(root, "eval", "references"),
};

/** Hash a file without holding it in memory; these are ~200 MB each. */
async function sha256File(path) {
  const hash = createHash("sha256");
  await pipeline(createReadStream(path), hash);
  return hash.digest("hex");
}

const mb = (bytes) => `${(bytes / 1e6).toFixed(0)} MB`;

async function fetchFile(set, file) {
  const dir = DESTINATIONS[file.role];
  if (!dir) throw new Error(`${file.name}: unknown role "${file.role}"`);
  await mkdir(dir, { recursive: true });
  const target = join(dir, file.name);

  if (existsSync(target)) {
    const digest = await sha256File(target);
    if (digest === file.sha256) {
      console.log(`  ${file.name}: present and verified`);
      return "present";
    }
    // Do not silently overwrite: a mismatch here is either a corrupted
    // download or a file that has been edited in place, and both are worth
    // knowing about before the bytes are replaced.
    throw new Error(
      `${file.name}: on disk but does not match the manifest\n` +
        `  expected ${file.sha256}\n  got      ${digest}\n` +
        `  delete it and re-run if you want the pinned version back`,
    );
  }

  const url = new URL(file.name, set.baseUrl).href;
  console.log(`  ${file.name}: downloading`);
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);

  // Stream to a scratch name, verify, then rename — so an interrupted or
  // corrupt download never appears at the real path where the harness would
  // pick it up as a fixture.
  const scratch = `${target}.partial`;
  try {
    await pipeline(Readable.fromWeb(response.body), createWriteStream(scratch));
    const digest = await sha256File(scratch);
    if (digest !== file.sha256) {
      throw new Error(
        `${file.name}: checksum mismatch\n  expected ${file.sha256}\n  got      ${digest}`,
      );
    }
    const { size } = await stat(scratch);
    await rename(scratch, target);
    console.log(`  ${file.name}: verified (${mb(size)}) → ${dir.replace(root + "/", "")}/`);
    return "fetched";
  } catch (err) {
    await rm(scratch, { force: true });
    throw err;
  }
}

async function main() {
  const wanted = process.argv[2];
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  const sets = manifest.sets.filter((s) => !wanted || s.id === wanted);
  if (sets.length === 0) {
    const ids = manifest.sets.map((s) => s.id).join(", ");
    throw new Error(`No set called "${wanted}". Available: ${ids}`);
  }

  for (const set of sets) {
    console.log(`${set.id}: ${set.files.length} files, ${set.format}`);
    for (const file of set.files) await fetchFile(set, file);
  }
  console.log("\nRun `pnpm eval` to include them.");
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
