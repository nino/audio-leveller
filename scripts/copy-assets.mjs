// Copy static renderer assets (HTML/CSS) into dist after the TypeScript build.
import { copyFile, mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const srcDir = join(root, "src", "renderer");
const outDir = join(root, "dist", "renderer");

await mkdir(outDir, { recursive: true });
for (const file of ["index.html", "styles.css"]) {
  await copyFile(join(srcDir, file), join(outDir, file));
}
console.log("Copied renderer assets to dist/renderer");
