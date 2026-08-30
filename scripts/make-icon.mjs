/**
 * Build build/icon.icns from the SVG artwork.
 *
 *   pnpm icon
 *
 * Two sources, because one drawing cannot serve both ends of the range:
 * icon.svg has the detail worth having at 128 points and up, and
 * icon-small.svg is the same object redrawn for the 16 and 32 point slots,
 * where that detail collapses into grey mush. Which file feeds which slot is
 * the SLOTS table below.
 *
 * The .icns is committed, so a release build never runs this — only someone
 * editing the artwork does. That is why it is allowed to want rsvg-convert
 * (or Inkscape) on the PATH rather than carrying a rasteriser of its own.
 */

import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const buildDir = join(root, "build");

/** iconutil's slot names, and the size and source each one is drawn from. */
const SLOTS = [
  ["icon_16x16.png", 16, "small"],
  ["icon_16x16@2x.png", 32, "small"],
  ["icon_32x32.png", 32, "small"],
  ["icon_32x32@2x.png", 64, "full"],
  ["icon_128x128.png", 128, "full"],
  ["icon_128x128@2x.png", 256, "full"],
  ["icon_256x256.png", 256, "full"],
  ["icon_256x256@2x.png", 512, "full"],
  ["icon_512x512.png", 512, "full"],
  ["icon_512x512@2x.png", 1024, "full"],
];

async function findRasteriser() {
  for (const [command, args] of [
    ["rsvg-convert", (svg, png, size) => ["-w", `${size}`, "-h", `${size}`, svg, "-o", png]],
    ["inkscape", (svg, png, size) => [svg, "-w", `${size}`, "-h", `${size}`, "-o", png]],
  ]) {
    try {
      await run("which", [command]);
      return { command, args };
    } catch {
      // try the next one
    }
  }
  throw new Error(
    "Needs an SVG rasteriser on the PATH: `brew install librsvg` (rsvg-convert) or Inkscape.",
  );
}

const sources = {
  full: join(buildDir, "icon.svg"),
  small: join(buildDir, "icon-small.svg"),
};
for (const [name, path] of Object.entries(sources)) {
  if (!existsSync(path)) throw new Error(`missing ${name} artwork: ${path}`);
}

const rasteriser = await findRasteriser();
const scratch = await mkdtemp(join(tmpdir(), "audio-leveller-icon-"));
const iconset = join(scratch, "icon.iconset");

try {
  await run("mkdir", ["-p", iconset]);
  for (const [slot, size, source] of SLOTS) {
    await run(rasteriser.command, rasteriser.args(sources[source], join(iconset, slot), size));
    console.log(`  ${slot.padEnd(22)} ${String(size).padStart(4)}px  ${source}`);
  }
  const icns = join(buildDir, "icon.icns");
  await run("iconutil", ["-c", "icns", iconset, "-o", icns]);
  console.log(`wrote ${icns} with ${rasteriser.command}`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
