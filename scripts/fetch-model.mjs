/**
 * Download the model weights the ONNX denoise backend needs.
 *
 *   pnpm fetch-model [id]        (default: every model in the registry)
 *
 * Weights are not bundled — they are large and carry their own licences — so
 * this exists to make fetching them one reproducible step rather than a page
 * of instructions. It refuses to install anything whose hash does not match
 * what `src/models/onnx.ts` pins: first the archive as a whole, then every
 * file extracted from it. A download that verifies is the same bytes the
 * checksums in the registry were taken from, or the run fails.
 */

import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { execFile } from "node:child_process";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);

// The registry is TypeScript; read the compiled copy so there is exactly one
// source of truth for the hashes. `pnpm build` runs first via the npm script.
const { MODELS, modelPath, modelFilePath } = await import("../dist/models/onnx.js");

const sha256 = (buffer) => createHash("sha256").update(buffer).digest("hex");

async function fetchModel(spec) {
  const target = modelPath(spec);
  const missing = Object.keys(spec.files).filter((role) => !existsSync(modelFilePath(spec, role)));
  if (missing.length === 0) {
    console.log(`${spec.id}: already present in ${target}`);
    return;
  }

  console.log(`${spec.id}: downloading ${spec.archive.url}`);
  const response = await fetch(spec.archive.url);
  if (!response.ok) throw new Error(`${spec.archive.url}: HTTP ${response.status}`);
  const archive = Buffer.from(await response.arrayBuffer());

  const digest = sha256(archive);
  if (digest !== spec.archive.sha256) {
    throw new Error(
      `${spec.id}: archive checksum mismatch\n  expected ${spec.archive.sha256}\n  got      ${digest}`,
    );
  }
  console.log(`${spec.id}: archive verified (${(archive.length / 1e6).toFixed(1)} MB)`);

  const scratch = await mkdtemp(join(tmpdir(), "audio-leveller-model-"));
  try {
    const archivePath = join(scratch, basename(spec.archive.url));
    await writeFile(archivePath, archive);
    await run("tar", ["xzf", archivePath, "-C", scratch, `--strip-components=${spec.archive.strip}`]);

    // Verify before installing, so a bad archive never lands in the model
    // directory in a half-written state.
    for (const [role, file] of Object.entries(spec.files)) {
      const extracted = join(scratch, file.name);
      if (!existsSync(extracted)) throw new Error(`${spec.id}: archive has no ${file.name}`);
      const got = sha256(await readFile(extracted));
      if (got !== file.sha256) {
        throw new Error(
          `${spec.id}: ${file.name} (${role}) checksum mismatch\n  expected ${file.sha256}\n  got      ${got}`,
        );
      }
    }

    await mkdir(target, { recursive: true });
    for (const file of Object.values(spec.files)) {
      await rename(join(scratch, file.name), join(target, file.name));
      console.log(`  ${file.name}`);
    }
    console.log(`${spec.id}: installed into ${target}`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

const wanted = process.argv.slice(2).filter((a) => a !== "--");
const specs = wanted.length > 0 ? MODELS.filter((m) => wanted.includes(m.id)) : MODELS;
if (specs.length === 0) {
  console.error(`No such model. Known: ${MODELS.map((m) => m.id).join(", ")}`);
  process.exit(1);
}

for (const spec of specs) await fetchModel(spec);
